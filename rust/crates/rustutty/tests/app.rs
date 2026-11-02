//! Drives a real RustuTTY window. The build under test uses a private clipboard
//! and never takes focus, and everything here is addressed to that process's
//! own windows, so it can run while someone is using the PC.

#[path = "../../deck-core/tests/support/server.rs"]
mod server;

use russh::keys::Algorithm;
use russh::MethodKind;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use windows::core::BOOL;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_TEST_KEY: u32 = WM_APP + 0x10;
const WM_TEST_DUMP: u32 = WM_APP + 0x11;
const WM_TEST_CLIP: u32 = WM_APP + 0x12;
const BM_CLICK: u32 = 0x00F5;
const CTRL: isize = 1;
const SHIFT: isize = 2;

/// A raw TCP peer that records what it receives from each connection.
struct Peer {
    port: u16,
    got: Arc<Mutex<Vec<Arc<Mutex<Vec<u8>>>>>>,
    out: Arc<Mutex<Vec<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
}

impl Peer {
    async fn start() -> Peer {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let (got, out): (Arc<Mutex<Vec<_>>>, Arc<Mutex<Vec<_>>>) = Default::default();
        let (g, o) = (got.clone(), out.clone());
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = l.accept().await else { return };
                let buf = Arc::new(Mutex::new(Vec::new()));
                g.lock().unwrap().push(buf.clone());
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                o.lock().unwrap().push(tx);
                tokio::spawn(serve(s, buf, rx));
            }
        });
        Peer { port, got, out }
    }

    async fn connected(&self, n: usize) {
        let end = Instant::now() + Duration::from_secs(8);
        while self.got.lock().unwrap().len() < n {
            assert!(Instant::now() < end, "only {} of {n} connected", self.got.lock().unwrap().len());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn send(&self, conn: usize, data: &[u8]) {
        self.out.lock().unwrap()[conn].send(data.to_vec()).unwrap();
    }

    fn received(&self, conn: usize) -> Vec<u8> {
        self.got.lock().unwrap().get(conn).map(|b| b.lock().unwrap().clone()).unwrap_or_default()
    }

    async fn wait(&self, conn: usize, want: &[u8]) {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            if self.received(conn).windows(want.len()).any(|w| w == want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("conn {conn} never got {want:?}, has {:?}", self.received(conn));
    }
}

async fn serve(mut s: TcpStream, buf: Arc<Mutex<Vec<u8>>>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>) {
    let mut b = [0u8; 4096];
    loop {
        tokio::select! {
            n = s.read(&mut b) => match n {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.lock().unwrap().extend_from_slice(&b[..n]),
            },
            data = rx.recv() => match data {
                Some(d) => { let _ = s.write_all(&d).await; }
                None => return,
            },
        }
    }
}

struct App {
    child: Child,
    main: HWND,
    content: HWND,
    edit: HWND,
    dir: PathBuf,
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

unsafe extern "system" fn collect(h: HWND, lp: LPARAM) -> BOOL {
    (*(lp.0 as *mut Vec<isize>)).push(h.0 as isize);
    true.into()
}

fn class_of(h: HWND) -> String {
    let mut b = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

fn text_of(h: HWND) -> String {
    let mut b = [0u16; 256];
    let n = unsafe { GetWindowTextW(h, &mut b) } as usize;
    String::from_utf16_lossy(&b[..n])
}

/// The main window of this process only, never anyone else's.
fn window_of(pid: u32) -> Option<HWND> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter().map(|h| HWND(h as *mut _)).find(|&h| {
        let mut p = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
        p == pid && class_of(h) == "RustuTTYMain"
    })
}

fn children(h: HWND) -> Vec<HWND> {
    let mut all: Vec<isize> = Vec::new();
    unsafe {
        let _ = EnumChildWindows(Some(h), Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    all.into_iter().map(|h| HWND(h as *mut _)).collect()
}

impl App {
    fn start(args: &[String]) -> App {
        let dir = std::env::temp_dir().join(format!("rustutty-test-{}-{}", std::process::id(), rand_tag()));
        std::fs::create_dir_all(&dir).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_rustutty"))
            .args(args)
            .env("RUSTUTTY_TEST_DUMP", dir.join("dump.txt"))
            .env("RUSTUTTY_TEST_CLIP", dir.join("clip.txt"))
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        let main = loop {
            if let Some(h) = window_of(child.id()) {
                break h;
            }
            assert!(Instant::now() < end, "window never appeared");
            std::thread::sleep(Duration::from_millis(20));
        };
        // the window exists before its controls do
        let (content, edit) = loop {
            let kids = children(main);
            let find = |c: &str| kids.iter().find(|&&h| class_of(h) == c).copied();
            if let (Some(c), Some(e), true) = (find("RustuTTYContent"), find("Edit"), kids.len() >= 11) {
                break (c, e);
            }
            assert!(Instant::now() < end, "controls never appeared");
            std::thread::sleep(Duration::from_millis(20));
        };
        let app = App { child, main, content, edit, dir };
        let tick = Instant::now() + Duration::from_secs(10);
        while !app.try_dump() {
            assert!(Instant::now() < tick, "deck never became ready");
        }
        app
    }

    fn try_dump(&self) -> bool {
        let path = self.dir.join("dump.txt");
        let _ = std::fs::remove_file(&path);
        if unsafe { PostMessageW(Some(self.main), WM_TEST_DUMP, WPARAM(0), LPARAM(0)) }.is_err() {
            return false;
        }
        for _ in 0..50 {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    fn post(&self, h: HWND, msg: u32, wp: usize, lp: isize) {
        unsafe { PostMessageW(Some(h), msg, WPARAM(wp), LPARAM(lp)).unwrap() };
    }

    fn dump(&self) -> String {
        let path = self.dir.join("dump.txt");
        let _ = std::fs::remove_file(&path);
        self.post(self.main, WM_TEST_DUMP, 0, 0);
        let end = Instant::now() + Duration::from_secs(4);
        while Instant::now() < end {
            if let Ok(s) = std::fs::read_to_string(&path) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no dump");
    }

    async fn until(&self, what: &str, f: impl Fn(&str) -> bool) -> String {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let d = self.dump();
            if f(&d) {
                return d;
            }
            assert!(Instant::now() < end, "waiting for {what}, have:\n{d}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    async fn sees(&self, text: &str) -> String {
        self.until(text, |d| d.contains(text)).await
    }

    fn typed(&self, text: &str) {
        for u in text.encode_utf16() {
            self.post(self.content, WM_CHAR, u as usize, 0);
        }
    }

    fn key(&self, vk: u8, flags: isize) {
        self.post(self.main, WM_TEST_KEY, vk as usize, flags);
    }

    fn set_clip(&self, text: &str) {
        std::fs::write(self.dir.join("clip.txt"), text).unwrap();
        self.post(self.main, WM_TEST_CLIP, 0, 0);
    }

    fn click(&self, label: &str) {
        let b = children(self.main).into_iter().find(|&h| class_of(h) == "Button" && text_of(h) == label).unwrap();
        self.post(b, BM_CLICK, 0, 0);
    }

    fn open(&self, target: &str) {
        let t: Vec<u16> = target.encode_utf16().chain([0]).collect();
        unsafe { SendMessageW(self.edit, WM_SETTEXT, None, Some(LPARAM(t.as_ptr() as isize))) };
        self.click("Open");
    }

    fn drag(&self, from: (i32, i32), to: (i32, i32)) {
        let cell = self.cell();
        let at = |c: (i32, i32)| ((c.1 * cell.1 + cell.1 / 2) << 16 | (c.0 * cell.0 + cell.0 / 2)) as isize;
        self.post(self.content, WM_LBUTTONDOWN, 1, at(from));
        self.post(self.content, WM_MOUSEMOVE, 1, at(to));
        self.post(self.content, WM_LBUTTONUP, 0, at(to));
    }

    fn cell(&self) -> (i32, i32) {
        let d = self.dump();
        let g = field(&d, "grid=");
        let (c, r) = g.split_once('x').unwrap();
        let mut rect = RECT::default();
        unsafe { GetClientRect(self.content, &mut rect).unwrap() };
        (rect.right / c.parse::<i32>().unwrap(), rect.bottom / r.parse::<i32>().unwrap())
    }

    fn resize(&self, w: i32, h: i32) {
        unsafe { SetWindowPos(self.main, None, -4000, 40, w, h, SWP_NOZORDER | SWP_NOACTIVATE).unwrap() };
    }

    fn exited(&mut self) -> bool {
        let end = Instant::now() + Duration::from_secs(8);
        while Instant::now() < end {
            if self.child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

fn rand_tag() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn field<'a>(d: &'a str, key: &str) -> &'a str {
    d.lines().find_map(|l| l.strip_prefix(key)).unwrap_or("")
}

fn raw_args(port: u16) -> Vec<String> {
    vec!["-raw".into(), "-P".into(), port.to_string(), "127.0.0.1".into()]
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_switch_and_keep_their_own_screens() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first session", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second session", |d| d.contains("sessions=2") && d.contains("active=Some(1)")).await;
    p.connected(2).await;
    p.send(0, b"screen zero");
    p.send(1, b"screen one");
    app.sees("row=screen one").await;

    app.typed("x");
    p.wait(1, b"x").await;
    assert!(p.received(0).is_empty(), "typing goes only to the selected session");

    app.key(VK_TAB.0 as u8, CTRL);
    let d = app.sees("row=screen zero").await;
    assert!(d.contains("active=Some(0)"));
    app.key(b'2', CTRL | SHIFT);
    app.sees("row=screen one").await;
    app.key(VK_TAB.0 as u8, CTRL | SHIFT);
    app.sees("row=screen zero").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_and_paste_rules() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("session", |d| d.contains("sessions=1")).await;
    p.connected(1).await;

    app.set_clip("hello-paste");
    app.key(b'V', CTRL);
    p.wait(0, b"hello-paste").await;

    p.send(0, b"copy this text");
    app.sees("row=copy this text").await;
    app.drag((5, 0), (8, 0));
    let d = app.until("selection copied", |d| field(d, "clip=") == "\"this\"").await;
    assert!(d.contains("selection=true"));

    let before = p.received(0).len();
    app.key(b'C', CTRL);
    app.until("selection cleared", |d| d.contains("selection=false")).await;
    assert_eq!(p.received(0).len(), before, "Ctrl+C with a selection sends nothing");

    app.key(VK_LEFT.0 as u8, 0);
    p.wait(0, b"\x1b[D").await;

    // a click selects nothing, so the clipboard must survive it
    app.drag((3, 0), (3, 0));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(field(&app.dump(), "clip="), "\"this\"");

    app.click("Paste");
    p.wait(0, b"this").await;

    app.click("Copy all");
    app.until("copy all", |d| field(d, "clip=").contains("copy this text")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_v_stays_out_of_full_screen_programs() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("session", |d| d.contains("sessions=1")).await;
    p.connected(1).await;
    app.set_clip("nope");
    p.send(0, b"\x1b[?1049h");
    app.until("alt screen", |d| d.contains("alt=true")).await;
    app.key(b'V', CTRL);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(p.received(0).is_empty(), "Ctrl+V must not paste in a full-screen program");
    app.key(b'V', CTRL | SHIFT);
    p.wait(0, b"nope").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn paste_to_all_reaches_every_session_and_only_once() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    for n in 2..=3 {
        app.open(&format!("raw://127.0.0.1:{}", p.port));
        app.until("session", move |d| d.contains(&format!("sessions={n}"))).await;
    }
    p.connected(3).await;
    app.set_clip("to-everyone");
    for burst in 1..=20 {
        app.click("Paste to all");
        let want = b"to-everyone".repeat(burst);
        for c in 0..3 {
            p.wait(c, &want).await;
        }
    }
    for c in 0..3 {
        assert_eq!(p.received(c).len(), 20 * 11, "conn {c}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn background_bell_and_titles_show_in_the_list() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second", |d| d.contains("sessions=2")).await;
    p.connected(2).await;
    p.send(0, b"\x1b]0;my-box\x07");
    app.sees("label0=my-box").await;
    p.send(0, b"\x07");
    app.sees("label0=* my-box").await;
    app.key(b'1', CTRL | SHIFT);
    app.sees("label0=my-box\n").await;
    assert!(text_of(app.main).contains("my-box"));
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_close_and_the_deck_goes_with_them() {
    let p = Peer::start().await;
    let mut app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second", |d| d.contains("sessions=2")).await;

    app.key(b'W', CTRL | SHIFT);
    app.until("one left", |d| d.contains("sessions=1") && d.contains("active=Some(0)")).await;
    app.key(b'D', CTRL | SHIFT);
    app.until("duplicated", |d| d.contains("sessions=2")).await;

    app.post(app.main, WM_CLOSE, 0, 0);
    assert!(app.exited(), "deck should exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_close_box_closes_its_own_row_only() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    app.until("second", |d| d.contains("sessions=2") && d.contains("active=Some(1)")).await;

    assert_ne!(unsafe { GetClassLongPtrW(app.main, GCLP_HICON) }, 0, "the title bar needs an icon");
    let list = children(app.main).into_iter().find(|&h| class_of(h) == "ListBox").unwrap();
    let mut r = RECT::default();
    unsafe { GetClientRect(list, &mut r).unwrap() };
    let at = |x: i32| x as isize & 0xffff | 8 << 16;

    // A click on the row selects it
    app.post(list, WM_LBUTTONDOWN, 1, at(20));
    app.post(list, WM_LBUTTONUP, 0, at(20));
    app.until("selected", |d| d.contains("sessions=2") && d.contains("active=Some(0)")).await;

    // Pressing the box and letting go elsewhere closes nothing
    app.post(list, WM_LBUTTONDOWN, 1, at(r.right - 6));
    app.post(list, WM_LBUTTONUP, 0, at(20));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(app.dump().contains("sessions=2"));

    app.post(list, WM_LBUTTONDOWN, 1, at(r.right - 6));
    app.post(list, WM_LBUTTONUP, 0, at(r.right - 6));
    app.until("closed", |d| d.contains("sessions=1")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ended_session_can_be_restarted_from_the_window_menu() {
    const IDM_RESTART: usize = 0x40;
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    p.connected(1).await;
    // Refused while it's still running
    app.post(app.main, WM_SYSCOMMAND, IDM_RESTART, 0);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(p.got.lock().unwrap().len(), 1, "a live session isn't restarted");

    p.out.lock().unwrap()[0] = tokio::sync::mpsc::unbounded_channel().0;
    app.until("ended", |d| d.contains("(ended)")).await;
    assert!(text_of(app.main).contains("(inactive)"));
    app.post(app.main, WM_SYSCOMMAND, IDM_RESTART, 0);
    p.connected(2).await;
    app.until("running again", |d| d.contains("sessions=1") && !d.contains("(ended)")).await;
    p.send(1, b"back again");
    app.sees("back again").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn special_commands_reach_a_telnet_host() {
    const IDM_SPECIAL_MIN: usize = 0x400;
    let p = Peer::start().await;
    let app = App::start(&[format!("telnet://127.0.0.1:{}", p.port)]);
    p.connected(1).await;
    app.until("open", |d| d.contains("sessions=1")).await;
    app.post(app.main, WM_SYSCOMMAND, IDM_SPECIAL_MIN, 0);
    p.wait(0, &[255, 246]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_connection_leaves_the_session_readable() {
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    app.until("first", |d| d.contains("sessions=1")).await;
    p.connected(1).await;
    p.send(0, b"last words");
    app.sees("row=last words").await;
    p.out.lock().unwrap().clear();
    let d = app.sees("Connection closed by remote host").await;
    assert!(d.contains("row=last words"));
    assert!(d.contains("label0=(ended) 127.0.0.1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_session_from_login_to_exit() {
    let key = server::new_key(Algorithm::Ed25519);
    let (port, seen) = server::start(key, vec![MethodKind::Password]).await;
    let app = App::start(&[]);
    app.open(&format!("{}@127.0.0.1:{port}", server::USER));
    app.sees("Trust this host and connect?").await;
    app.typed("y\r");
    app.sees("password:").await;
    app.typed(&format!("{}\r", server::PASSWORD));
    let d = app.sees("row=welcome").await;
    assert!(!d.contains(server::PASSWORD), "password shown on screen:\n{d}");

    let grid = field(&d, "grid=").to_string();
    let (c, r) = grid.split_once('x').unwrap();
    let pty = seen.lock().unwrap().pty.clone().unwrap();
    assert_eq!((pty.1.to_string().as_str(), pty.2.to_string().as_str()), (c, r), "server told the window size");

    app.typed("ping");
    app.sees("$ ping").await;

    app.resize(900, 500);
    app.until("grid change", |d| field(d, "grid=") != grid).await;
    let end = Instant::now() + Duration::from_secs(5);
    while seen.lock().unwrap().resizes.is_empty() {
        assert!(Instant::now() < end, "server was never told about the resize");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    app.typed("\rexit\r");
    app.sees("exit status 3").await;
    app.sees("(ended)").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn typing_into_a_prompt_is_not_lost_when_keys_come_fast() {
    let key = server::new_key(Algorithm::Ed25519);
    let (port, _) = server::start(key, vec![MethodKind::Password]).await;
    let app = App::start(&[]);
    app.open(&format!("127.0.0.1:{port}"));
    app.sees("Trust this host and connect?").await;
    app.typed("y\r");
    app.sees("login as:").await;
    app.typed("tester\r");
    app.sees("tester@127.0.0.1's password:").await;
}

/// Run by hand: renders the off-screen window to RUSTUTTY_SHOT to look at.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn screenshot() {
    let path = std::env::var("RUSTUTTY_SHOT").unwrap();
    let p = Peer::start().await;
    let app = App::start(&raw_args(p.port));
    p.connected(1).await;
    app.open(&format!("raw://127.0.0.1:{}", p.port));
    p.connected(2).await;
    p.send(0, b"\x1b]0;web-01\x07");
    p.send(1, b"\x1b]0;db-02\x07nick@db-02:~$ ls -la\r\n\x1b[1;34mdrwxr-xr-x\x1b[0m  5 nick nick 4096 Oct 30 \x1b[1;32mbin\x1b[0m\r\n\
        -rw-r--r--  1 nick nick  220 Oct 30 .bash_logout\r\n\x1b[7m inverse \x1b[0m \x1b[4munderlined\x1b[0m \x1b[31mred\x1b[33m yellow\x1b[35m magenta\x1b[0m \xe2\x94\x82 box \xc3\xa9\r\n\
        \x1b[38;5;208m256 colour\x1b[0m \x1b[38;2;0;200;255mtrue colour\x1b[0m\r\nnick@db-02:~$ ");
    app.sees("row=nick@db-02:~$").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let script = format!(
        r#"Add-Type -AssemblyName System.Drawing
Add-Type 'using System;using System.Runtime.InteropServices;public class W{{[DllImport("user32.dll")]public static extern bool PrintWindow(IntPtr h,IntPtr d,uint f);[StructLayout(LayoutKind.Sequential)]public struct R{{public int a,b,c,d;}}[DllImport("user32.dll")]public static extern bool GetWindowRect(IntPtr h,out R r);}}'
$h=[IntPtr]{}
$r=New-Object W+R;[void][W]::GetWindowRect($h,[ref]$r)
$b=New-Object Drawing.Bitmap(($r.c-$r.a),($r.d-$r.b));$g=[Drawing.Graphics]::FromImage($b);$d=$g.GetHdc()
[void][W]::PrintWindow($h,$d,2);$g.ReleaseHdc($d);$b.Save('{path}',[Drawing.Imaging.ImageFormat]::Png)"#,
        app.main.0 as isize
    );
    let out = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn test_build_takes_no_focus() {
    let app = App::start(&[]);
    std::thread::sleep(Duration::from_millis(300));
    assert_ne!(unsafe { GetForegroundWindow() }, app.main);
}

/// The settings window opens, every control is built and read back, and what
/// comes out of it connects.
#[tokio::test(flavor = "multi_thread")]
async fn settings_window_round_trips_a_session() {
    let peer = Peer::start().await;
    let app = App::start(&[]);
    app.post(app.main, WM_APP + 0x13, peer.port as usize, 0);
    peer.connected(1).await;
    let dump = app.dump();
    assert!(dump.contains("sessions=1"), "{dump}");
}

/// Keys reach the host in the form a session's settings ask for, through the
/// real window procedure.
#[tokio::test(flavor = "multi_thread")]
async fn the_window_has_a_scrollbar_that_follows_the_scrollback() {
    let peer = Peer::start().await;
    let app = App::start(&[format!("raw://127.0.0.1:{}", peer.port)]);
    peer.connected(1).await;
    peer.send(0, &b"line\r\n".repeat(200));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut info = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
    unsafe { GetScrollInfo(app.content, SB_VERT, &mut info).unwrap() };
    assert!(info.nMax > info.nPage as i32 + 100, "{info:?}");
    assert_eq!(info.nPos, info.nMax - info.nPage as i32 + 1, "the view starts at the bottom");
}
