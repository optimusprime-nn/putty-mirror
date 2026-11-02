//! Pageant: holds keys for PuTTY, RUSTuTTY and other SSH programs, answering
//! on the window PuTTY's clients look for.
//!
//!   pageant key.ppk [more keys]
//!
//! It runs in its console, which is where passphrases are asked for. Closing
//! that or pressing Ctrl+C removes the keys. There's no tray icon.

use anyhow::{anyhow, bail, Context};
use deck_core::ppk::Ppk;
use deck_tools::prompt;
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, PrivateKey};
use std::sync::OnceLock;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::*;

// from PuTTY's windows/pageant.c and pageant.h
const AGENT_COPYDATA_ID: usize = 0x804e50ba;
const AGENT_MAX_MSGLEN: usize = 262144;

struct Server {
    runtime: Runtime,
    connect: UnboundedSender<DuplexStream>,
}

static SERVER: OnceLock<Server> = OnceLock::new();

/// The agent's answer to one request, over a private connection to it.
fn ask(server: &Server, request: Vec<u8>) -> Option<Vec<u8>> {
    let (mut near, far) = tokio::io::duplex(AGENT_MAX_MSGLEN);
    server.connect.send(far).ok()?;
    server.runtime.block_on(async move {
        near.write_all(&request).await.ok()?;
        let mut len = [0u8; 4];
        near.read_exact(&mut len).await.ok()?;
        let n = u32::from_be_bytes(len) as usize;
        if n > AGENT_MAX_MSGLEN - 4 {
            return None;
        }
        let mut reply = vec![0u8; n + 4];
        reply[..4].copy_from_slice(&len);
        near.read_exact(&mut reply[4..]).await.ok()?;
        Some(reply)
    })
}

fn own_sid() -> Option<Vec<u8>> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        got.ok()?;
        Some(buf)
    }
}

/// Serves a client's request in its shared memory. The memory has to belong to
/// this user: otherwise another account could use the keys held here.
unsafe fn serve_mapping(name: &[u16]) -> bool {
    let Some(server) = SERVER.get() else { return false };
    let Ok(map) = OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, PCWSTR(name.as_ptr())) else { return false };
    let mut ok = false;
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let found = GetSecurityInfo(map, SE_KERNEL_OBJECT, OWNER_SECURITY_INFORMATION, Some(&mut owner), None, None, None, Some(&mut descriptor));
    if let (true, Some(sid)) = (found.is_ok(), own_sid()) {
        let user = &*(sid.as_ptr() as *const TOKEN_USER);
        if EqualSid(owner, user.User.Sid).is_ok() {
            let view = MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, 0);
            if !view.Value.is_null() {
                let mut info = MEMORY_BASIC_INFORMATION::default();
                VirtualQuery(Some(view.Value), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
                let size = info.RegionSize.min(AGENT_MAX_MSGLEN);
                let memory = std::slice::from_raw_parts_mut(view.Value as *mut u8, size);
                if size >= 4 {
                    let n = u32::from_be_bytes([memory[0], memory[1], memory[2], memory[3]]) as usize;
                    if n + 4 <= size {
                        if let Some(reply) = ask(server, memory[..n + 4].to_vec()) {
                            if reply.len() <= size {
                                memory[..reply.len()].copy_from_slice(&reply);
                                ok = true;
                            }
                        }
                    }
                }
                let _ = UnmapViewOfFile(view);
            }
        }
    }
    if !descriptor.0.is_null() {
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
    }
    let _ = CloseHandle(map);
    ok
}

unsafe extern "system" fn window(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        let cds = &*(lp.0 as *const COPYDATASTRUCT);
        if cds.dwData != AGENT_COPYDATA_ID || cds.cbData == 0 || cds.lpData.is_null() {
            return LRESULT(0);
        }
        let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
        if bytes.last() != Some(&0) {
            return LRESULT(0);
        }
        let name: Vec<u16> = bytes.iter().map(|&b| b as u16).collect();
        return LRESULT(serve_mapping(&name) as isize);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn load(path: &str) -> anyhow::Result<PrivateKey> {
    let text = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    if !text.starts_with(b"PuTTY-User-Key-File") {
        return match load_secret_key(path, None) {
            Err(russh::keys::Error::KeyIsEncrypted) => load_secret_key(path, Some(&prompt(&format!("Passphrase for {path}: "))?)).map_err(|e| anyhow!("{e}")),
            other => other.map_err(|e| anyhow!("{e}")),
        };
    }
    let file = Ppk::parse(&String::from_utf8_lossy(&text))?;
    if !file.encrypted {
        return file.to_private_key("");
    }
    for _ in 0..3 {
        match file.to_private_key(&prompt(&format!("Passphrase for {}: ", file.comment))?) {
            Err(e) if e.to_string() == "wrong passphrase" => eprintln!("Wrong passphrase"),
            other => return other,
        }
    }
    bail!("giving up on {path}")
}

fn main() -> anyhow::Result<()> {
    unsafe {
        if FindWindowW(w!("Pageant"), w!("Pageant")).is_ok() {
            bail!("Pageant is already running");
        }
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
    let (tx, mut rx) = unbounded_channel::<DuplexStream>();
    let incoming = futures::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|s| s.map(Ok::<_, std::io::Error>)));
    runtime.spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));

    for path in std::env::args().skip(1) {
        let key = load(&path)?;
        let (near, far) = tokio::io::duplex(AGENT_MAX_MSGLEN);
        tx.send(far).map_err(|_| anyhow!("the agent has stopped"))?;
        runtime.block_on(AgentClient::connect(near).add_identity(&key, &[]))?;
        eprintln!("Added {path}");
    }
    SERVER.set(Server { runtime, connect: tx }).map_err(|_| anyhow!("already started"))?;

    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(window), hInstance: instance, lpszClassName: w!("Pageant"), ..Default::default() });
        CreateWindowExW(WINDOW_EX_STYLE(0), w!("Pageant"), w!("Pageant"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, Some(instance), None)?;
        eprintln!("Pageant is running. Press Ctrl+C to stop and remove the keys.");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
