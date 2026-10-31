//! A network session: a task on the runtime, driven by commands from the UI.

use crate::config::{Protocol, Target};
use crate::hostkeys::HostKeyStore;
use crate::ldisc::{Ldisc, LineState};
use crate::logging::Log;
use crate::proxy::Conn;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::Mutex;
use zeroize::Zeroize;

/// Set by the console tools for `-batch`, on their own copy of the settings.
pub const BATCH: &str = "DeckBatch";

pub enum Command {
    Input(Vec<u8>),
    /// The terminal answering the host, which is not the person typing.
    Reply(Vec<u8>),
    Resize { cols: u32, rows: u32 },
    Close,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// What the host sent.
    Output(Vec<u8>),
    /// Prompts and messages from this end, which stay out of session logs.
    Local(Vec<u8>),
    /// The session is over. The reason is shown in its terminal.
    Closed(String),
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

/// Server text for showing at login, with what could move the cursor or fake a
/// prompt taken out, as PuTTY's stripctrl does. Lines and tabs stay.
pub fn clean(text: &str) -> String {
    text.chars().filter(|&c| !c.is_control() || c == '\n' || c == '\t').filter(|c| !is_direction_mark(*c)).collect()
}

/// Characters that reorder text, so it can show as something it isn't.
pub fn is_direction_mark(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// The user's side of a session. Prompts run in the terminal itself, the way
/// PuTTY asks "login as:", so they work the same in every session.
pub(crate) struct Io {
    pub commands: UnboundedReceiver<Command>,
    pub events: EventSink,
    pub cols: u32,
    pub rows: u32,
    pub line: Arc<LineState>,
    /// `-batch`: nothing may wait for an answer.
    pub batch: bool,
}

impl Io {
    /// Called once login is over and typing belongs to the session.
    pub fn logged_in(&self) {
        self.line.ready.store(true, Ordering::Relaxed);
    }

    pub fn say(&self, text: &str) {
        (self.events)(Event::Local(text.replace('\n', "\r\n").into_bytes()));
    }

    /// Reads a line. `None` means the user gave up: Ctrl+C, Ctrl+D or closing.
    pub async fn read_line(&mut self, prompt: &str, echo: bool) -> Option<String> {
        if self.batch {
            self.say(&format!("{prompt}\nNot asked, because of -batch.\n"));
            return None;
        }
        self.say(prompt);
        let mut line = String::new();
        let mut pending = Vec::new();
        loop {
            match self.commands.recv().await? {
                Command::Close => return None,
                Command::Resize { cols, rows } => {
                    self.cols = cols;
                    self.rows = rows;
                }
                Command::Reply(_) => {}
                Command::Input(mut bytes) => {
                    pending.extend_from_slice(&bytes);
                    bytes.zeroize();
                    // an incomplete UTF-8 sequence waits for the rest of it
                    let (valid, bad) = match std::str::from_utf8(&pending) {
                        Ok(s) => (s.len(), 0),
                        Err(e) => (e.valid_up_to(), e.error_len().unwrap_or(0)),
                    };
                    let chars: Vec<char> = String::from_utf8_lossy(&pending[..valid]).chars().collect();
                    pending.drain(..valid + bad);
                    let mut i = 0;
                    while i < chars.len() {
                        let c = chars[i];
                        i += 1;
                        match c {
                            '\r' | '\n' => {
                                self.say("\n");
                                pending.zeroize();
                                return Some(line);
                            }
                            '\x03' | '\x04' => {
                                self.say("\n");
                                line.zeroize();
                                pending.zeroize();
                                return None;
                            }
                            '\x7f' | '\x08' => {
                                if line.pop().is_some() && echo {
                                    self.say("\x08 \x08");
                                }
                            }
                            '\x15' => {
                                if echo {
                                    self.say(&"\x08 \x08".repeat(line.chars().count()));
                                }
                                line.clear();
                            }
                            '\x1b' => {
                                // a cursor key: drop its sequence, not the line
                                while i < chars.len() {
                                    i += 1;
                                    if chars[i - 1] != '[' && chars[i - 1] != 'O' && !chars[i - 1].is_ascii_digit() && chars[i - 1] != ';' {
                                        break;
                                    }
                                }
                            }
                            c if !c.is_control() => {
                                line.push(c);
                                if echo {
                                    self.say(&c.to_string());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// Adds the session's log, if it has one, to the event sink.
fn logged(target: &Target, events: EventSink) -> (EventSink, Option<String>) {
    match Log::open(target) {
        Ok(None) => (events, None),
        Err(e) => (events, Some(e.to_string())),
        Ok(Some(log)) => {
            let log = StdMutex::new(log);
            let sink: EventSink = Arc::new(move |e: Event| {
                if let Event::Output(bytes) = &e {
                    if let Ok(mut log) = log.lock() {
                        log.write(bytes);
                    }
                }
                events(e);
            });
            (sink, None)
        }
    }
}

/// Reaches the host for the protocols that run over a plain stream.
async fn connect(target: &Target, store: &Arc<dyn HostKeyStore>, io: Io) -> anyhow::Result<(Conn, Io)> {
    let io = Arc::new(Mutex::new(io));
    let conn = crate::proxy::connect(target, store, &io).await?;
    let io = Arc::into_inner(io).ok_or_else(|| anyhow::anyhow!("The session is still in use."))?;
    Ok((conn, io.into_inner()))
}

async fn run(target: &Target, store: Arc<dyn HostKeyStore>, io: Io) -> anyhow::Result<String> {
    match &target.protocol {
        Protocol::Ssh => crate::ssh::run(target, store, io).await,
        Protocol::Telnet => {
            let (conn, io) = connect(target, &store, io).await?;
            crate::telnet::run(target, conn, true, io).await
        }
        Protocol::Raw => {
            let (conn, io) = connect(target, &store, io).await?;
            crate::telnet::run(target, conn, false, io).await
        }
        Protocol::Rlogin => {
            let mut privileged = target.clone();
            privileged.settings.set(crate::proxy::PRIVILEGED, crate::settings::Value::Int(1));
            let (conn, io) = connect(&privileged, &store, io).await?;
            crate::rlogin::run(target, conn, io).await
        }
        Protocol::Supdup => {
            let (conn, io) = connect(target, &store, io).await?;
            crate::supdup::run(target, conn, io).await
        }
        Protocol::Serial => crate::serial::run(target, io).await,
        Protocol::Unsupported(p) => Err(anyhow::anyhow!("Unsupported protocol {p}.")),
    }
}

/// Login prompts for the console tools, on standard input and error.
fn console_io(target: &Target) -> Io {
    use std::io::{Read, Write};
    let (tx, commands) = unbounded_channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 256];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || tx.send(Command::Input(buf[..n].to_vec())).is_err() {
                return;
            }
        }
    });
    let events: EventSink = Arc::new(|e| {
        if let Event::Output(b) | Event::Local(b) = e {
            let mut out = std::io::stderr();
            let _ = out.write_all(&b);
            let _ = out.flush();
        }
    });
    Io { commands, events, cols: 80, rows: 24, line: Default::default(), batch: target.settings.b(BATCH, false) }
}

/// Connects for file transfer.
pub async fn connect_sftp(target: Target, store: Arc<dyn HostKeyStore>) -> anyhow::Result<crate::ssh::Sftp> {
    let io = console_io(&target);
    crate::ssh::sftp(&target, store, io).await
}

/// Connects and runs a command, for the old SCP protocol.
pub async fn connect_exec(target: Target, store: Arc<dyn HostKeyStore>, command: &str) -> anyhow::Result<crate::ssh::Exec> {
    let io = console_io(&target);
    crate::ssh::exec(&target, store, io, command).await
}

/// Starts a session. The sink is called from the runtime, never the UI thread.
pub fn spawn(
    runtime: &Handle,
    target: Target,
    store: Arc<dyn HostKeyStore>,
    (cols, rows): (u32, u32),
    events: EventSink,
) -> UnboundedSender<Command> {
    let (tx, mut typed) = unbounded_channel();
    let (to_session, commands) = unbounded_channel();
    let (events, log_error) = logged(&target, events);
    let line = Arc::new(LineState::default());
    let mut ldisc = Ldisc::new(&target.settings, &target.protocol);
    let (state, sink) = (line.clone(), events.clone());
    runtime.spawn(async move {
        while let Some(cmd) = typed.recv().await {
            let cmd = match cmd {
                Command::Input(bytes) if state.ready.load(Ordering::Relaxed) => {
                    let (send, show) = ldisc.feed(&bytes, &state);
                    if !show.is_empty() {
                        sink(Event::Output(show));
                    }
                    if send.is_empty() {
                        continue;
                    }
                    Command::Input(send)
                }
                // an answer from the terminal mustn't land in a password prompt
                Command::Reply(bytes) if state.ready.load(Ordering::Relaxed) => Command::Input(bytes),
                Command::Reply(_) => continue,
                other => other,
            };
            if to_session.send(cmd).is_err() {
                return;
            }
        }
    });
    runtime.spawn(async move {
        let batch = target.settings.b(BATCH, false);
        let io = Io { commands, events: events.clone(), cols, rows, line, batch };
        if let Some(why) = log_error {
            io.say(&format!("Could not open the log file: {why}\n"));
        }
        for note in &target.notes {
            io.say(&format!("{note}\n"));
        }
        let result = if let Some(why) = &target.refuse {
            Err(anyhow::anyhow!("Not connecting: {why}."))
        } else {
            run(&target, store, io).await
        };
        events(Event::Closed(match result {
            Ok(why) => why,
            Err(e) => e.to_string(),
        }));
    });
    tx
}
