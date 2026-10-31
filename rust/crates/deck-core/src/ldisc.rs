//! Local echo and local line editing, after PuTTY's `ldisc.c`.

use crate::config::Protocol;
use crate::settings::Settings;
use std::sync::atomic::{AtomicBool, Ordering};

const FORCE_ON: u32 = 0;
const FORCE_OFF: u32 = 1;
const AUTO: u32 = 2;

/// What the protocol knows. Login prompts have their own editing in the
/// terminal, so nothing happens here until `ready` is set.
#[derive(Default)]
pub struct LineState {
    pub ready: AtomicBool,
    pub remote_echo: AtomicBool,
    pub remote_sga: AtomicBool,
}

pub struct Ldisc {
    echo: u32,
    edit: u32,
    telnet: bool,
    raw: bool,
    line: Vec<u8>,
}

fn is_word_break(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

impl Ldisc {
    /// A session with neither keyword, such as one typed in the box, has both
    /// off. Every session PuTTY saved has them.
    pub fn new(s: &Settings, protocol: &Protocol) -> Ldisc {
        Ldisc {
            echo: s.i("LocalEcho", if s.has("LocalEdit") { AUTO } else { FORCE_OFF }),
            edit: s.i("LocalEdit", if s.has("LocalEcho") { AUTO } else { FORCE_OFF }),
            telnet: *protocol == Protocol::Telnet,
            raw: *protocol == Protocol::Raw,
            line: Vec::new(),
        }
    }

    fn on(&self, mode: u32, automatic: bool) -> bool {
        match mode {
            FORCE_ON => true,
            AUTO => automatic,
            _ => false,
        }
    }

    fn echoing(&self, state: &LineState) -> bool {
        let auto = self.raw || (self.telnet && !state.remote_echo.load(Ordering::Relaxed));
        self.on(self.echo, auto)
    }

    fn editing(&self, state: &LineState) -> bool {
        let auto = self.raw || (self.telnet && !state.remote_sga.load(Ordering::Relaxed));
        self.on(self.edit, auto)
    }

    fn rub_out(&mut self, show: &mut Vec<u8>, echo: bool) {
        if self.line.pop().is_some() && echo {
            show.extend(b"\x08 \x08");
        }
    }

    /// Takes typed bytes, and returns what to send and what to show.
    pub fn feed(&mut self, input: &[u8], state: &LineState) -> (Vec<u8>, Vec<u8>) {
        let (mut send, mut show) = (Vec::new(), Vec::new());
        let echo = self.echoing(state);
        if !self.editing(state) {
            send.extend(self.line.drain(..));
            send.extend(input);
            if echo {
                for &b in input {
                    match b {
                        b'\r' => show.extend(b"\r\n"),
                        0x7f => show.extend(b"\x08 \x08"),
                        b => show.push(b),
                    }
                }
            }
            return (send, show);
        }
        for &b in input {
            match b {
                0x7f | 0x08 => self.rub_out(&mut show, echo),
                0x15 => {
                    while !self.line.is_empty() {
                        self.rub_out(&mut show, echo);
                    }
                }
                0x17 => {
                    while self.line.last().is_some_and(|&c| is_word_break(c)) {
                        self.rub_out(&mut show, echo);
                    }
                    while self.line.last().is_some_and(|&c| !is_word_break(c)) {
                        self.rub_out(&mut show, echo);
                    }
                }
                // Ctrl+D sends what's there; on an empty line it goes through
                0x04 if !self.line.is_empty() => send.append(&mut self.line),
                0x03 | 0x04 | 0x1a => {
                    self.line.clear();
                    send.push(b);
                }
                b'\r' | b'\n' => {
                    send.append(&mut self.line);
                    send.push(b'\r');
                    if echo {
                        show.extend(b"\r\n");
                    }
                }
                b => {
                    self.line.push(b);
                    if echo {
                        show.push(b);
                    }
                }
            }
        }
        (send, show)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Value;

    fn modes(echo: u32, edit: u32, p: Protocol) -> Ldisc {
        let mut s = Settings::default();
        s.set("LocalEcho", Value::Int(echo));
        s.set("LocalEdit", Value::Int(edit));
        Ldisc::new(&s, &p)
    }

    #[test]
    fn off_by_default_for_ssh_and_typed_sessions() {
        let state = LineState::default();
        let (send, show) = modes(AUTO, AUTO, Protocol::Ssh).feed(b"ab", &state);
        assert_eq!((send, show), (b"ab".to_vec(), vec![]));
        let (send, show) = Ldisc::new(&Settings::default(), &Protocol::Raw).feed(b"ab", &state);
        assert_eq!((send, show), (b"ab".to_vec(), vec![]));
    }

    #[test]
    fn a_line_waits_for_return_and_can_be_edited() {
        let state = LineState::default();
        let mut l = modes(AUTO, AUTO, Protocol::Raw);
        let (send, show) = l.feed(b"abx\x7f", &state);
        assert!(send.is_empty());
        assert_eq!(show, b"abx\x08 \x08");
        let (send, _) = l.feed(b"c\r", &state);
        assert_eq!(send, b"abc\r");
    }

    #[test]
    fn word_and_line_erase() {
        let state = LineState::default();
        let mut l = modes(FORCE_OFF, FORCE_ON, Protocol::Ssh);
        l.feed(b"one two", &state);
        l.feed(b"\x17", &state);
        assert_eq!(l.feed(b"\r", &state).0, b"one \r");
        l.feed(b"gone\x15kept", &state);
        assert_eq!(l.feed(b"\r", &state).0, b"kept\r");
    }

    #[test]
    fn telnet_auto_follows_the_server() {
        let state = LineState::default();
        let mut l = modes(AUTO, AUTO, Protocol::Telnet);
        assert_eq!(l.feed(b"a", &state), (vec![], b"a".to_vec()));
        l.feed(b"\x15", &state);
        state.remote_echo.store(true, Ordering::Relaxed);
        state.remote_sga.store(true, Ordering::Relaxed);
        assert_eq!(l.feed(b"b", &state), (b"b".to_vec(), vec![]));
    }
}
