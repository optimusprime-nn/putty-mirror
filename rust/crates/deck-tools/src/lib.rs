//! What PuTTY's command line tools share: their arguments, the console, and
//! connecting for file transfer.

pub mod scp;

use anyhow::{anyhow, Context};
use deck_core::config::{load_sessions, Protocol, Target};
use deck_core::hostkeys::{HostKeyStore, RegistryStore};
use deck_core::session::{self, Command, Event, EventSink, BATCH};
use deck_core::settings::Value;
use std::io::{Read, Write};
use std::sync::Arc;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use windows::Win32::System::Console::*;
use zeroize::Zeroize;

/// The options the tools have in common, with whatever else was on the line.
#[derive(Default, Debug)]
pub struct Args {
    pub protocol: Option<Protocol>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub key: Option<String>,
    pub load: Option<String>,
    pub forwards: Vec<String>,
    pub no_shell: bool,
    pub no_pty: bool,
    pub compress: bool,
    pub agent: bool,
    pub batch: bool,
    pub rest: Vec<String>,
}

/// Whether a name from a server is safe to create here: one plain file name,
/// and not one of the Windows device names, which go to a device, not a file.
pub fn safe_local_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit());
    !(name.is_empty() || name == "." || name == ".." || device || name.ends_with(['.', ' ']) || name.contains(|c: char| c.is_control() || "/\\:*?\"<>|".contains(c)))
}

/// `-L 8080:host:80` or `-D 1080` as PuTTY's `L8080=host:80` or `D1080=`.
pub fn forward_key(kind: &str, spec: &str) -> String {
    match spec.split_once(':') {
        Some((port, dest)) if kind != "D" => format!("{kind}{port}={dest}"),
        _ => format!("{kind}{spec}="),
    }
}

impl Args {
    pub fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Args> {
        let mut out = Args::default();
        let mut args = args;
        let mut started = false;
        while let Some(a) = args.next() {
            let mut value = |name: &str| args.next().ok_or_else(|| anyhow!("{name} needs a value"));
            if started || !a.starts_with('-') {
                started |= !out.rest.is_empty();
                out.rest.push(a);
                continue;
            }
            match a.as_str() {
                "-ssh" => out.protocol = Some(Protocol::Ssh),
                "-telnet" => out.protocol = Some(Protocol::Telnet),
                "-rlogin" => out.protocol = Some(Protocol::Rlogin),
                "-raw" => out.protocol = Some(Protocol::Raw),
                "-serial" => out.protocol = Some(Protocol::Serial),
                "-P" => out.port = Some(value("-P")?.parse().context("-P needs a port number")?),
                "-l" => out.user = Some(value("-l")?),
                "-i" => out.key = Some(value("-i")?),
                "-load" => out.load = Some(value("-load")?),
                "-L" | "-R" | "-D" => out.forwards.push(forward_key(&a[1..], &value(&a)?)),
                "-N" => out.no_shell = true,
                "-T" => out.no_pty = true,
                "-t" => out.no_pty = false,
                "-C" => out.compress = true,
                "-A" => out.agent = true,
                "-a" => out.agent = false,
                "-batch" => out.batch = true,
                "-pw" => anyhow::bail!("-pw isn't supported: a password on a command line can be read by other programs"),
                _ => out.rest.push(a),
            }
        }
        Ok(out)
    }

    /// Builds the target for `[user@]host` or a saved session's name.
    pub fn target(&self, host: Option<&str>) -> anyhow::Result<Target> {
        let saved = load_sessions();
        let name = self.load.as_deref().or(host).ok_or_else(|| anyhow!("no host given"))?;
        let (user, plain) = name.rsplit_once('@').map_or((None, name), |(u, h)| (Some(u), h));
        let mut t = match saved.into_iter().find(|(n, _)| n == plain) {
            Some((_, t)) => t,
            None if self.load.is_some() => anyhow::bail!("there is no saved session called {name}"),
            None => Target::parse(name).ok_or_else(|| anyhow!("{name} isn't a host name"))?,
        };
        if let Some(u) = user {
            t.user = Some(u.to_string());
        }
        if let Some(p) = &self.protocol {
            t.protocol = p.clone();
        }
        t.port = self.port.unwrap_or(t.port);
        t.user = self.user.clone().or(t.user);
        t.key_file = self.key.clone().or(t.key_file);
        if !self.forwards.is_empty() {
            let mut all = t.settings.s("PortForwardings", "");
            for f in &self.forwards {
                if !all.is_empty() {
                    all.push(',');
                }
                all += f;
            }
            t.settings.set("PortForwardings", Value::Str(all));
        }
        for (key, on) in [("SshNoShell", self.no_shell), ("NoPTY", self.no_pty), ("Compression", self.compress), ("AgentFwd", self.agent)] {
            if on {
                t.settings.set(key, Value::Int(1));
            }
        }
        if self.batch {
            t.settings.set(BATCH, Value::Int(1));
        }
        anyhow::ensure!(t.refuse.is_none(), "{}", t.refuse.clone().unwrap_or_default());
        Ok(t)
    }
}

/// Puts a console in raw mode for as long as this lives, so that keys go to the
/// session and a password isn't echoed. Does nothing when input is a pipe.
pub struct RawConsole(Option<(windows::Win32::Foundation::HANDLE, CONSOLE_MODE)>, Option<(windows::Win32::Foundation::HANDLE, CONSOLE_MODE)>);

impl RawConsole {
    pub fn new() -> RawConsole {
        unsafe {
            let Ok(h) = GetStdHandle(STD_INPUT_HANDLE) else { return RawConsole(None, None) };
            let mut mode = CONSOLE_MODE(0);
            if GetConsoleMode(h, &mut mode).is_err() {
                return RawConsole(None, None);
            }
            let raw = (mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT)) | ENABLE_VIRTUAL_TERMINAL_INPUT;
            let _ = SetConsoleMode(h, raw);
            let mut output = None;
            if let Ok(out) = GetStdHandle(STD_OUTPUT_HANDLE) {
                let mut m = CONSOLE_MODE(0);
                if GetConsoleMode(out, &mut m).is_ok() {
                    let _ = SetConsoleMode(out, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                    output = Some((out, m));
                }
            }
            RawConsole(Some((h, mode)), output)
        }
    }
}

impl Default for RawConsole {
    fn default() -> Self {
        RawConsole::new()
    }
}

impl Drop for RawConsole {
    fn drop(&mut self) {
        for (h, mode) in self.0.into_iter().chain(self.1) {
            unsafe {
                let _ = SetConsoleMode(h, mode);
            }
        }
    }
}

/// Asks for something on the console without echoing it.
pub fn prompt(text: &str) -> anyhow::Result<String> {
    use std::io::BufRead;
    let _raw = RawConsole::new();
    eprint!("{text}");
    std::io::stderr().flush()?;
    let mut stdin = std::io::stdin().lock();
    let mut line = Vec::new();
    loop {
        let (used, done) = {
            let buf = stdin.fill_buf()?;
            match buf.iter().position(|&b| b == b'\r' || b == b'\n') {
                _ if buf.is_empty() => (0, true),
                Some(i) => {
                    line.extend_from_slice(&buf[..i]);
                    (i + 1, true)
                }
                None => {
                    line.extend_from_slice(buf);
                    (buf.len(), false)
                }
            }
        };
        stdin.consume(used);
        if done {
            break;
        }
    }
    eprintln!();
    let text = String::from_utf8_lossy(&line).into_owned();
    line.zeroize();
    Ok(text)
}

fn output_is_console() -> bool {
    let mut mode = CONSOLE_MODE(0);
    unsafe { GetStdHandle(STD_OUTPUT_HANDLE).is_ok_and(|h| GetConsoleMode(h, &mut mode).is_ok()) }
}

/// A command's output for a console, with nothing that could steer the console.
/// With a terminal session the host expects those to work, as they do in PuTTY.
fn plain_bytes(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    text.chars().filter(|&c| !c.is_control() || matches!(c, '\t' | '\r' | '\n')).collect::<String>().into_bytes()
}

pub fn console_size() -> (u32, u32) {
    unsafe {
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        if let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) {
            if GetConsoleScreenBufferInfo(h, &mut info).is_ok() {
                let cols = info.srWindow.Right - info.srWindow.Left + 1;
                let rows = info.srWindow.Bottom - info.srWindow.Top + 1;
                return (cols.max(1) as u32, rows.max(1) as u32);
            }
        }
    }
    (80, 24)
}

/// Sends standard input to a session until it ends. At end of input a session
/// closes, unless `keep` holds it open, as with `plink -N`.
pub fn pump_stdin(tx: UnboundedSender<Command>, keep: bool) {
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf) {
                Ok(n) if n > 0 => {
                    if tx.send(Command::Input(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                _ => break,
            }
        }
        if !keep {
            let _ = tx.send(Command::Close);
        }
    });
}

pub fn host_keys() -> Arc<dyn HostKeyStore> {
    Arc::new(RegistryStore)
}

/// Runs a session on the console. Returns why it ended.
pub async fn run_console(target: Target) -> anyhow::Result<String> {
    let (etx, mut erx) = unbounded_channel();
    let sink: EventSink = Arc::new(move |e| {
        let _ = etx.send(e);
    });
    let keep = target.settings.b("SshNoShell", false);
    let plain = target.settings.b("NoPTY", false) && output_is_console();
    let raw = RawConsole::new();
    let tx = session::spawn(&tokio::runtime::Handle::current(), target, host_keys(), console_size(), sink);
    pump_stdin(tx.clone(), keep);
    let mut out = std::io::stdout();
    let reason = loop {
        match erx.recv().await {
            Some(Event::Output(b)) => {
                out.write_all(&if plain { plain_bytes(&b) } else { b })?;
                out.flush()?;
            }
            // prompts stay out of output that's going to a file or a pipe
            Some(Event::Local(b)) => {
                let mut err = std::io::stderr();
                err.write_all(&b)?;
                err.flush()?;
            }
            Some(Event::Closed(why)) => break why,
            None => break "The session ended.".to_string(),
        }
    };
    drop(raw);
    drop(tx);
    Ok(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Args {
        Args::parse(line.split_whitespace().map(str::to_string)).unwrap()
    }

    #[test]
    fn options_stop_at_the_command() {
        let a = parse("-P 2222 -l me host ls -la");
        assert_eq!((a.port, a.user.as_deref()), (Some(2222), Some("me")));
        assert_eq!(a.rest, ["host", "ls", "-la"]);
    }

    #[test]
    fn forwards_become_putty_keys() {
        let a = parse("-L 8080:web:80 -R 2222:localhost:22 -D 1080 -N host");
        assert_eq!(a.forwards, ["L8080=web:80", "R2222=localhost:22", "D1080="]);
        assert!(a.no_shell);
    }

    #[test]
    fn device_names_and_paths_are_not_safe_names() {
        for bad in ["NUL", "nul.txt", "COM1", "lpt9.log", "CONOUT$", "..", ".", "", "a/b", "a\\b", "c:x", "x.", "x ", "a\x1bb", "a*b"] {
            assert!(!safe_local_name(bad), "{bad:?}");
        }
        for good in ["report.txt", "null", "COM", "console.log", ".profile", "com10"] {
            assert!(safe_local_name(good), "{good:?}");
        }
    }

    #[test]
    fn plain_output_loses_escapes_and_keeps_lines() {
        assert_eq!(plain_bytes(b"a\x1b[2Jb\r\n\tc\x07"), b"a[2Jb\r\n\tc");
    }

    #[test]
    fn a_password_on_the_command_line_is_refused() {
        let e = Args::parse(["-pw".to_string(), "x".to_string()].into_iter()).unwrap_err();
        assert!(e.to_string().contains("-pw"));
    }
}
