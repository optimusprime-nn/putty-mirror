//! Telnet and raw TCP sessions.

use crate::config::Target;
use crate::proxy::Conn;
use crate::session::{Command, Event, Io};
use std::sync::atomic::Ordering;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The most of a subnegotiation that's kept.
const MAX_SUB: usize = 4096;
const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;
const ECHO: u8 = 1;
const SGA: u8 = 3;
const TTYPE: u8 = 24;
const TSPEED: u8 = 32;
const NAWS: u8 = 31;
const OLD_ENVIRON: u8 = 36;
const NEW_ENVIRON: u8 = 39;
const SUSP: u8 = 237;
const IP: u8 = 244;
const EC: u8 = 247;
const EL: u8 = 248;
const SEND: u8 = 1;
const IS: u8 = 0;
const RFC_VAR: u8 = 0;
const RFC_VALUE: u8 = 1;
const BSD_VAR: u8 = 1;
const BSD_VALUE: u8 = 0;

/// Splits telnet commands out of the stream, across reads.
#[derive(Default)]
struct Filter {
    state: State,
    sub: Vec<u8>,
    naws_on: bool,
    term: String,
    speed: String,
    env: Vec<(String, String)>,
    rfc_environ: bool,
    /// Options this end has already offered, which the answer must not repeat.
    offered: Vec<(u8, u8)>,
    remote_echo: bool,
    remote_sga: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum State {
    #[default]
    Data,
    Iac,
    Option(u8),
    Sub,
    SubIac,
}

fn naws(cols: u32, rows: u32) -> Vec<u8> {
    let mut v = vec![IAC, SB, NAWS];
    for b in [(cols >> 8) as u8, cols as u8, (rows >> 8) as u8, rows as u8] {
        v.push(b);
        if b == IAC {
            v.push(IAC);
        }
    }
    v.extend([IAC, SE]);
    v
}

impl Filter {
    fn new(target: &Target) -> Filter {
        let s = &target.settings;
        let mut env = s.map("Environment");
        if let Some(u) = &target.user {
            env.push(("USER".to_string(), u.clone()));
        }
        Filter {
            term: s.s("TerminalType", "xterm"),
            speed: s.s("TerminalSpeed", "38400,38400"),
            env,
            rfc_environ: s.b("RFCEnviron", false),
            ..Default::default()
        }
    }

    /// What an active client sends first (`PassiveTelnet` waits for the server).
    fn opening(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for (verb, option) in [(WILL, NAWS), (WILL, TSPEED), (WILL, TTYPE), (WILL, NEW_ENVIRON), (DO, ECHO), (WILL, SGA), (DO, SGA)] {
            self.offered.push((verb, option));
            out.extend([IAC, verb, option]);
        }
        out
    }

    fn answer(&mut self, verb: u8, option: u8, reply: &mut Vec<u8>) {
        if let Some(i) = self.offered.iter().position(|o| *o == (verb, option)) {
            self.offered.remove(i);
        } else {
            reply.extend([IAC, verb, option]);
        }
    }

    fn sub_is(option: u8, body: &[u8], reply: &mut Vec<u8>) {
        reply.extend([IAC, SB, option, IS]);
        reply.extend(body);
        reply.extend([IAC, SE]);
    }

    fn environment(&self, option: u8) -> Vec<u8> {
        let (var, value) = if option == NEW_ENVIRON || self.rfc_environ { (RFC_VAR, RFC_VALUE) } else { (BSD_VAR, BSD_VALUE) };
        let mut body = Vec::new();
        for (k, v) in &self.env {
            body.push(var);
            body.extend(k.as_bytes());
            body.push(value);
            body.extend(v.as_bytes());
        }
        body
    }

    /// Returns the data to show and any replies to send.
    fn feed(&mut self, input: &[u8], cols: u32, rows: u32) -> (Vec<u8>, Vec<u8>) {
        let (mut data, mut reply) = (Vec::new(), Vec::new());
        for &b in input {
            match self.state {
                State::Data if b == IAC => self.state = State::Iac,
                State::Data => data.push(b),
                State::Iac => match b {
                    IAC => {
                        data.push(IAC);
                        self.state = State::Data;
                    }
                    DO | DONT | WILL | WONT => self.state = State::Option(b),
                    SB => {
                        self.sub.clear();
                        self.state = State::Sub;
                    }
                    _ => self.state = State::Data,
                },
                State::Option(verb) => {
                    self.state = State::Data;
                    match (verb, b) {
                        (DO, NAWS) => {
                            self.answer(WILL, NAWS, &mut reply);
                            reply.extend(naws(cols, rows));
                            self.naws_on = true;
                        }
                        (DO, TTYPE | SGA | TSPEED | NEW_ENVIRON | OLD_ENVIRON) => self.answer(WILL, b, &mut reply),
                        (DO, _) => reply.extend([IAC, WONT, b]),
                        (WILL, ECHO | SGA) => {
                            self.answer(DO, b, &mut reply);
                            if b == ECHO {
                                self.remote_echo = true;
                            } else {
                                self.remote_sga = true;
                            }
                        }
                        (WONT, ECHO) => self.remote_echo = false,
                        (WONT, SGA) => self.remote_sga = false,
                        (WILL, _) => reply.extend([IAC, DONT, b]),
                        _ => {}
                    }
                }
                State::Sub if b == IAC => self.state = State::SubIac,
                // nothing answered here is long, so the rest of a huge one is dropped
                State::Sub if self.sub.len() >= MAX_SUB => {}
                State::Sub => self.sub.push(b),
                State::SubIac if b == IAC => {
                    if self.sub.len() < MAX_SUB {
                        self.sub.push(IAC);
                    }
                    self.state = State::Sub;
                }
                State::SubIac => {
                    self.state = State::Data;
                    match self.sub.as_slice() {
                        [TTYPE, SEND] => Self::sub_is(TTYPE, self.term.to_uppercase().as_bytes(), &mut reply),
                        [TSPEED, SEND] => Self::sub_is(TSPEED, self.speed.as_bytes(), &mut reply),
                        [o @ (NEW_ENVIRON | OLD_ENVIRON), SEND, ..] => {
                            let o = *o;
                            Self::sub_is(o, &self.environment(o), &mut reply);
                        }
                        _ => {}
                    }
                }
            }
        }
        (data, reply)
    }
}

/// Typed bytes for the wire. `keyboard` is `TelnetKey`, which turns editing and
/// interrupt keys into Telnet commands, and `newline` is `TelnetRet`.
fn encode_input(bytes: &[u8], keyboard: bool, newline: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            0x7f | 0x08 if keyboard => v.extend([IAC, EC]),
            0x15 if keyboard => v.extend([IAC, EL]),
            0x03 if keyboard => v.extend([IAC, IP]),
            0x1a if keyboard => v.extend([IAC, SUSP]),
            b'\r' if newline => v.extend(b"\r\n"),
            b'\r' => v.extend(b"\r\0"),
            IAC => v.extend([IAC, IAC]),
            b => v.push(b),
        }
    }
    v
}

pub(crate) async fn run(target: &Target, conn: Conn, telnet: bool, mut io: Io) -> anyhow::Result<String> {
    let (mut rd, mut wr) = tokio::io::split(conn);
    let s = &target.settings;
    let mut filter = Filter::new(target);
    let (keyboard, newline) = (s.b("TelnetKey", false), s.b("TelnetRet", true));
    if telnet && !s.b("PassiveTelnet", false) {
        wr.write_all(&filter.opening()).await?;
    }
    io.logged_in();
    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            n = rd.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    return Ok("Connection closed by remote host.".into());
                }
                if telnet {
                    let (data, reply) = filter.feed(&buf[..n], io.cols, io.rows);
                    if !reply.is_empty() {
                        wr.write_all(&reply).await?;
                    }
                    io.line.remote_echo.store(filter.remote_echo, Ordering::Relaxed);
                    io.line.remote_sga.store(filter.remote_sga, Ordering::Relaxed);
                    if !data.is_empty() {
                        (io.events)(Event::Output(data));
                    }
                } else {
                    (io.events)(Event::Output(buf[..n].to_vec()));
                }
            }
            cmd = io.commands.recv() => match cmd {
                None | Some(Command::Close) => return Ok("Session closed.".into()),
                // answers reach here as Input, once the session is ready
                Some(Command::Reply(_)) => {}
                Some(Command::Special(crate::session::Special::Telnet(code))) if telnet => wr.write_all(&[255, code]).await?,
                Some(Command::Special(_)) => {}
                Some(Command::Input(b)) => {
                    let b = if telnet { encode_input(&b, keyboard, newline) } else { b };
                    wr.write_all(&b).await?;
                }
                Some(Command::Resize { cols, rows }) => {
                    io.cols = cols;
                    io.rows = rows;
                    if telnet && filter.naws_on {
                        wr.write_all(&naws(cols, rows)).await?;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_split_from_data_across_reads() {
        let mut f = Filter::default();
        let (d, r) = f.feed(&[b'a', IAC], 80, 24);
        assert_eq!((d, r), (vec![b'a'], vec![]));
        let (d, r) = f.feed(&[DO], 80, 24);
        assert_eq!((d, r), (vec![], vec![]));
        let (d, r) = f.feed(&[ECHO, b'b', IAC, IAC], 80, 24);
        assert_eq!(d, vec![b'b', IAC]);
        assert_eq!(r, vec![IAC, WONT, ECHO]);
    }

    #[test]
    fn window_size_is_negotiated() {
        let mut f = Filter::default();
        let (_, r) = f.feed(&[IAC, DO, NAWS], 80, 24);
        assert_eq!(r, vec![IAC, WILL, NAWS, IAC, SB, NAWS, 0, 80, 0, 24, IAC, SE]);
        assert!(f.naws_on);
    }

    #[test]
    fn size_of_255_is_doubled() {
        assert_eq!(naws(255, 24), vec![IAC, SB, NAWS, 0, IAC, IAC, 0, 24, IAC, SE]);
    }

    #[test]
    fn terminal_type_is_answered() {
        let mut f = Filter { term: "xterm".into(), ..Default::default() };
        let (_, r) = f.feed(&[IAC, SB, TTYPE, 1, IAC, SE], 80, 24);
        assert_eq!(r, [&[IAC, SB, TTYPE, 0][..], b"XTERM", &[IAC, SE]].concat());
    }

    #[test]
    fn input_iac_is_escaped() {
        assert_eq!(encode_input(&[1, IAC, 2], false, true), vec![1, IAC, IAC, 2]);
    }

    #[test]
    fn keyboard_and_newline_settings() {
        assert_eq!(encode_input(b"\r", false, true), b"\r\n");
        assert_eq!(encode_input(b"\r", false, false), b"\r\0");
        assert_eq!(encode_input(&[0x7f, 3], false, true), vec![0x7f, 3]);
        assert_eq!(encode_input(&[0x7f, 3, 0x15, 0x1a], true, true), vec![IAC, EC, IAC, IP, IAC, EL, IAC, SUSP]);
    }

    #[test]
    fn an_offer_is_not_repeated_in_the_answer() {
        let mut f = Filter::default();
        let opening = f.opening();
        assert_eq!(&opening[..3], &[IAC, WILL, NAWS]);
        let (_, r) = f.feed(&[IAC, DO, TTYPE], 80, 24);
        assert!(r.is_empty());
        let (_, r) = f.feed(&[IAC, WILL, ECHO], 80, 24);
        assert!(r.is_empty());
        assert!(f.remote_echo);
    }

    #[test]
    fn speed_and_environment_are_sent_on_request() {
        let mut f = Filter { speed: "9600,9600".into(), env: vec![("USER".into(), "me".into())], ..Default::default() };
        let (_, r) = f.feed(&[IAC, SB, TSPEED, SEND, IAC, SE], 80, 24);
        assert_eq!(r, [&[IAC, SB, TSPEED, IS][..], b"9600,9600", &[IAC, SE]].concat());
        let (_, r) = f.feed(&[IAC, SB, NEW_ENVIRON, SEND, IAC, SE], 80, 24);
        assert_eq!(r, [&[IAC, SB, NEW_ENVIRON, IS, RFC_VAR][..], b"USER", &[RFC_VALUE], b"me", &[IAC, SE]].concat());
        let (_, r) = f.feed(&[IAC, SB, OLD_ENVIRON, SEND, IAC, SE], 80, 24);
        assert_eq!(r, [&[IAC, SB, OLD_ENVIRON, IS, BSD_VAR][..], b"USER", &[BSD_VALUE], b"me", &[IAC, SE]].concat());
    }
}
