//! SUPDUP sessions (RFC 734), after `otherbackends/supdup.c`. The server's
//! display codes are turned into the escape sequences the terminal understands.

use crate::config::Target;
use crate::proxy::Conn;
use crate::session::{Command, Event, Io};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// TTYOPT bits, in octal as the RFC gives them
const TOERS: u64 = 0o040000000000;
const TOMVB: u64 = 0o010000000000;
const TOSAI: u64 = 0o004000000000;
const TOSA1: u64 = 0o002000000000;
const TOMVU: u64 = 0o000400000000;
const TOMOR: u64 = 0o000200000000;
const TOROL: u64 = 0o000100000000;
const TOLWR: u64 = 0o000020000000;
const TOLID: u64 = 0o000002000000;
const TOCID: u64 = 0o000001000000;
const TPCBS: u64 = 0o000000000040;
const TPORS: u64 = 0o000000000010;

const WORDS: u64 = 0o777773000000;
const TCTYP: u64 = 7;
const TTYROL: u64 = 1;

const TDMOV: u8 = 0o200;
const TDMV1: u8 = 0o201;
const TDEOF: u8 = 0o202;
const TDEOL: u8 = 0o203;
const TDDLF: u8 = 0o204;
const TDCRL: u8 = 0o207;
const TDNOP: u8 = 0o210;
const TDBS: u8 = 0o211;
const TDLF: u8 = 0o212;
const TDCR: u8 = 0o213;
const TDORS: u8 = 0o214;
const TDQOT: u8 = 0o215;
const TDFS: u8 = 0o216;
const TDMV0: u8 = 0o217;
const TDCLR: u8 = 0o220;
const TDBEL: u8 = 0o221;
const TDILP: u8 = 0o223;
const TDDLP: u8 = 0o224;
const TDICP: u8 = 0o225;
const TDDCP: u8 = 0o226;

const ASCII: u32 = 0;
const ITS: u32 = 1;

/// The low 32 characters of the ITS set (RFC 734) and of WAITS.
const ITS_LOW: [&str; 32] = [
    "·", "↓", "α", "β", "∧", "¬", "ε", "π", "λ", "γ", "δ", "↑", "±", "⊕", "∞", "∂",
    "⊂", "⊃", "∩", "∪", "∀", "∃", "⊗", "↔", "←", "→", "≠", "◊", "≤", "≥", "≡", "∨",
];
const WAITS_LOW: [&str; 32] = [
    "", "↓", "α", "β", "∧", "¬", "ε", "π", "λ", "", "", "", "", "", "∞", "∂",
    "⊂", "⊃", "∩", "∪", "∀", "∃", "⊗", "↔", "_", "→", "~", "≠", "≤", "≥", "≡", "∨",
];

fn glyph(charset: u32, c: u8, out: &mut Vec<u8>) {
    match charset {
        ASCII => {
            if (0o40..0o177).contains(&c) {
                out.push(c);
            }
        }
        ITS => match c {
            0..=31 => out.extend(ITS_LOW[c as usize].as_bytes()),
            127 => out.extend("∫".as_bytes()),
            _ => out.push(c),
        },
        _ => match c {
            0..=31 => out.extend(WAITS_LOW[c as usize].as_bytes()),
            94 => out.extend("↑".as_bytes()),
            95 => out.extend("←".as_bytes()),
            125 => out.extend("◊".as_bytes()),
            126 => out.push(b'}'),
            127 => {}
            _ => out.push(c),
        },
    }
}

fn arg_count(code: u8) -> usize {
    match code {
        TDMOV => 4,
        TDMV0 | TDMV1 => 2,
        TDQOT | TDILP | TDDLP | TDICP | TDDCP => 1,
        _ => 0,
    }
}

/// Reads the server's stream, across reads.
struct Decoder {
    charset: u32,
    connected: bool,
    code: Option<u8>,
    args: Vec<u8>,
}

impl Decoder {
    fn run(&mut self, code: u8, out: &mut Vec<u8>, reply: &mut Vec<u8>) {
        let a = &self.args;
        let text = match code {
            TDMOV => format!("\x1b[{};{}H", a[2] as u32 + 1, a[3] as u32 + 1),
            TDMV0 | TDMV1 => format!("\x1b[{};{}H", a[0] as u32 + 1, a[1] as u32 + 1),
            TDEOF => "\x1b[J".into(),
            TDEOL => "\x1b[K".into(),
            TDDLF => "\x1b[X".into(),
            TDCRL => "\r\n\x1b[K".into(),
            TDFS => "\x1b[C".into(),
            TDCLR => "\x1b[2J\x1b[H".into(),
            TDBEL => "\x07".into(),
            TDBS => "\x08".into(),
            TDLF => "\n".into(),
            TDCR => "\r".into(),
            TDILP => format!("\x1b[{}L", a[0]),
            TDDLP => format!("\x1b[{}M", a[0]),
            TDICP => format!("\x1b[{}@", a[0]),
            TDDCP => format!("\x1b[{}P", a[0]),
            TDQOT => {
                out.push(a[0]);
                String::new()
            }
            TDORS => {
                // Discards pending output and reports where the cursor is.
                // PuTTY asks its terminal; here that's not known, so this says home.
                out.clear();
                reply.extend([0o34, 0o20, 0, 0]);
                String::new()
            }
            _ => String::new(),
        };
        out.extend(text.as_bytes());
    }

    /// Returns what to show and what to send back.
    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (mut out, mut reply) = (Vec::new(), Vec::new());
        for &c in input {
            if !self.connected {
                if c == TDNOP {
                    self.connected = true;
                } else {
                    out.push(c);
                }
            } else if let Some(code) = self.code {
                self.args.push(c);
                if self.args.len() == arg_count(code) {
                    self.code = None;
                    self.run(code, &mut out, &mut reply);
                }
            } else if c >= 0o200 {
                self.args.clear();
                if arg_count(c) == 0 {
                    self.run(c, &mut out, &mut reply);
                } else {
                    self.code = Some(c);
                }
            } else {
                glyph(self.charset, c, &mut out);
            }
        }
        (out, reply)
    }
}

fn word(value: u64, out: &mut Vec<u8>) {
    for i in (0..6).rev() {
        out.push((value >> (i * 6) & 0o77) as u8);
    }
}

fn config(charset: u32, more: bool, scroll: bool, cols: u32, rows: u32) -> Vec<u8> {
    let mut options = TOERS | TOMVB | TOMVU | TOLWR | TOLID | TOCID | TPCBS | TPORS;
    if charset != ASCII {
        options |= TOSAI | TOSA1;
    }
    if more {
        options |= TOMOR;
    }
    if scroll {
        options |= TOROL;
    }
    let mut out = Vec::new();
    // one column is kept for showing that a line continues
    for w in [WORDS, TCTYP, options, rows as u64, cols.saturating_sub(1) as u64, TTYROL] {
        word(w, &mut out);
    }
    out
}

fn encode_input(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for &b in input {
        if b == 0o34 {
            out.extend([0o34, 0o34]);
        } else {
            out.push(b & 0o177);
        }
    }
    out
}

pub(crate) async fn run(target: &Target, mut conn: Conn, mut io: Io) -> anyhow::Result<String> {
    let s = &target.settings;
    let charset = s.i("SUPDUPCharset", ASCII);
    let more = s.b("SUPDUPMoreProcessing", false);
    let scroll = s.b("SUPDUPScrolling", false);
    conn.write_all(&config(charset, more, scroll, io.cols, io.rows)).await?;
    let mut decoder = Decoder { charset, connected: false, code: None, args: Vec::new() };
    let mut sent_location = false;
    io.logged_in();
    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            n = conn.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    return Ok("Connection closed by remote host.".into());
                }
                let (out, reply) = decoder.feed(&buf[..n]);
                if !reply.is_empty() {
                    conn.write_all(&reply).await?;
                }
                if decoder.connected && !sent_location {
                    // after the greeting: Symbolics servers get confused any earlier
                    sent_location = true;
                    conn.write_all(&[0o300, 0o302]).await?;
                    conn.write_all(s.s("SUPDUPLocation", "The Internet").as_bytes()).await?;
                    conn.write_all(&[0]).await?;
                }
                if !out.is_empty() {
                    (io.events)(Event::Output(out));
                }
            }
            cmd = io.commands.recv() => match cmd {
                None | Some(Command::Close) => return Ok("Session closed.".into()),
                // answers reach here as Input, once the session is ready
                Some(Command::Reply(_) | Command::Special(_)) => {}
                Some(Command::Input(b)) => conn.write_all(&encode_input(&b)).await?,
                // SUPDUP has no way to resize after connecting
                Some(Command::Resize { .. }) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder() -> Decoder {
        Decoder { charset: ASCII, connected: true, code: None, args: Vec::new() }
    }

    #[test]
    fn config_is_six_words_of_six_bit_bytes() {
        let c = config(ASCII, false, false, 80, 24);
        assert_eq!(c.len(), 36);
        assert!(c.iter().all(|&b| b < 64));
        assert_eq!(&c[..6], &[0o77, 0o77, 0o73, 0, 0, 0]);
        assert_eq!(c[23], 24, "height");
        assert_eq!(&c[28..30], &[1, 15], "79 wide, keeping one column back");
    }

    #[test]
    fn greeting_ends_at_tdnop() {
        let mut d = Decoder { connected: false, ..decoder() };
        let (out, _) = d.feed(&[b'h', b'i', TDNOP, b'x', 0o30]);
        assert_eq!(out, b"hix", "control characters are dropped in ASCII");
    }

    #[test]
    fn codes_survive_a_split_across_reads() {
        let mut d = decoder();
        assert_eq!(d.feed(&[TDMOV, 1, 2]).0, b"");
        assert_eq!(d.feed(&[3, 4]).0, b"\x1b[4;5H");
        assert_eq!(d.feed(&[TDCLR]).0, b"\x1b[2J\x1b[H");
        assert_eq!(d.feed(&[TDILP, 2]).0, b"\x1b[2L");
    }

    #[test]
    fn output_reset_discards_and_replies() {
        let (out, reply) = decoder().feed(&[b'a', TDORS, b'b']);
        assert_eq!((out, reply), (b"b".to_vec(), vec![0o34, 0o20, 0, 0]));
    }

    #[test]
    fn its_characters_map_to_unicode() {
        let mut d = Decoder { charset: ITS, ..decoder() };
        assert_eq!(d.feed(&[2, 127]).0, "α∫".as_bytes());
    }

    #[test]
    fn input_doubles_the_escape_and_clears_the_top_bit() {
        assert_eq!(encode_input(&[0o34, 0xe1]), vec![0o34, 0o34, 0x61]);
    }
}
