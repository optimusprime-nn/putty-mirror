//! Sits in front of the emulator, doing what PuTTY's terminal settings ask for
//! that alacritty_terminal has no option for. It reads the host's output, and
//! drops or rewrites the sequences a session has turned off.

use crate::settings::Settings;

#[derive(Clone, Debug)]
pub struct FilterConfig {
    cr_implies_lf: bool,
    lf_implies_cr: bool,
    no_alt_screen: bool,
    no_clear_scroll: bool,
    no_charset: bool,
    no_dbackspace: bool,
    erase_to_scrollback: bool,
    ansi_colour: bool,
    xterm256: bool,
    true_colour: bool,
    answerback: Vec<u8>,
    /// `RemoteQTitleAction`: 0 ignores the query, 1 answers empty, 2 truthfully.
    title_query: u32,
    /// With no printer set, a host's requests to print are ignored.
    printer: bool,
}

impl FilterConfig {
    pub fn new(s: &Settings) -> FilterConfig {
        FilterConfig {
            cr_implies_lf: s.b("CRImpliesLF", false),
            lf_implies_cr: s.b("LFImpliesCR", false),
            no_alt_screen: s.b("NoAltScreen", false),
            no_clear_scroll: s.b("NoRemoteClearScroll", false),
            no_charset: s.b("NoRemoteCharset", false),
            no_dbackspace: s.b("NoDBackspace", false),
            erase_to_scrollback: s.b("EraseToScrollback", true),
            ansi_colour: s.b("ANSIColour", true),
            xterm256: s.b("Xterm256Colour", true),
            true_colour: s.b("TrueColour", true),
            answerback: s.s("Answerback", "PuTTY").into_bytes(),
            title_query: s.i("RemoteQTitleAction", 1),
            printer: !s.s("Printer", "").is_empty(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Action {
    Reply(Vec<u8>),
    ReportTitle,
    Print(Vec<u8>),
    PrintEnd,
}

/// The end of a print job, which is looked for in everything that's printed.
const PRINT_END: &[u8] = b"\x1b[4i";
/// The longest control sequence and string kept. Past these, a sequence can
/// only be an attempt to use up memory, so it's dropped.
const MAX_PARAMS: usize = 256;
const MAX_STRING: usize = 64 * 1024;

struct Printing {
    /// `CSI 5 i` prints instead of showing; `CSI ? 5 i` does both.
    only: bool,
    matched: usize,
}

enum State {
    Ground,
    Escape,
    Charset,
    Csi(Vec<u8>),
    /// A control sequence too long to keep, dropped to its end.
    CsiIgnore,
    /// An OSC or similar string, passed through to its end.
    Text(usize),
    /// A string too long to keep, dropped to its end.
    TextIgnore,
}

pub struct Filter {
    config: FilterConfig,
    state: State,
    printing: Option<Printing>,
}

/// The nearest entry in the 256-colour table's 6x6x6 cube.
fn nearest_256(r: u32, g: u32, b: u32) -> u32 {
    let step = |v: u32| if v < 48 { 0 } else if v < 115 { 1 } else { (v - 35) / 40 };
    16 + 36 * step(r) + 6 * step(g) + step(b)
}

impl Filter {
    pub fn new(config: FilterConfig) -> Filter {
        Filter { config, state: State::Ground, printing: None }
    }

    fn sgr(&self, params: &[u8]) -> Option<Vec<u8>> {
        let c = &self.config;
        if c.ansi_colour && c.xterm256 && c.true_colour {
            return None;
        }
        let text = String::from_utf8_lossy(params);
        let parts: Vec<&str> = text.split(';').collect();
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < parts.len() {
            let p = parts[i];
            let code: u32 = p.parse().unwrap_or(0);
            match code {
                38 | 48 if p.contains(':') => {}
                38 | 48 => match parts.get(i + 1).copied() {
                    Some("5") => {
                        if c.ansi_colour && c.xterm256 {
                            out.push(format!("{code};5;{}", parts.get(i + 2).unwrap_or(&"0")));
                        }
                        i += 2;
                    }
                    Some("2") => {
                        let v = |n: usize| parts.get(i + n).and_then(|p| p.parse().ok()).unwrap_or(0);
                        if c.ansi_colour && c.true_colour {
                            out.push(format!("{code};2;{};{};{}", v(2), v(3), v(4)));
                        } else if c.ansi_colour && c.xterm256 {
                            out.push(format!("{code};5;{}", nearest_256(v(2), v(3), v(4))));
                        }
                        i += 4;
                    }
                    _ => {}
                },
                30..=37 | 39 | 40..=47 | 49 | 90..=97 | 100..=107 if !c.ansi_colour => {}
                _ => out.push(p.to_string()),
            }
            i += 1;
        }
        // an empty SGR would mean "reset", which nothing here asked for
        Some(if out.is_empty() { Vec::new() } else { format!("\x1b[{}m", out.join(";")).into_bytes() })
    }

    fn private_modes(&self, params: &[u8], last: u8) -> Option<Vec<u8>> {
        let text = String::from_utf8_lossy(&params[1..]);
        // 2026 holds back the display, which would let a host freeze it
        let dropped = |m: &&str| match m.parse::<u32>() {
            Ok(47 | 1047 | 1048 | 1049) => self.config.no_alt_screen,
            Ok(2026) => true,
            _ => false,
        };
        let all: Vec<&str> = text.split(';').collect();
        if !all.iter().any(|m| dropped(m)) {
            return None;
        }
        let keep: Vec<&str> = all.into_iter().filter(|m| !dropped(m)).collect();
        Some(if keep.is_empty() { Vec::new() } else { format!("\x1b[?{}{}", keep.join(";"), last as char).into_bytes() })
    }

    /// What to do with a whole CSI sequence. `None` leaves it as it came.
    fn csi(&self, params: &[u8], last: u8, actions: &mut Vec<Action>) -> Option<Vec<u8>> {
        let c = &self.config;
        // `03` and `3;1` mean the same as `3` to the emulator
        let first = std::str::from_utf8(params).ok().and_then(|p| p.split(';').next()?.parse::<u32>().ok());
        match (params, last) {
            (b"5" | b"?5", b'i') if c.printer => Some(Vec::new()),
            (b"?4", b'i') => Some(Vec::new()),
            ([b'?', ..], b'h' | b'l') => self.private_modes(params, last),
            (_, b'J') if first == Some(3) && c.no_clear_scroll => Some(Vec::new()),
            // keeps the old screen out of the scrollback; the cursor stays put
            (_, b'J') if first == Some(2) && !c.erase_to_scrollback => Some(b"\x1b7\x1b[H\x1b[J\x1b8".to_vec()),
            (_, b't') if first == Some(21) => {
                match c.title_query {
                    1 => actions.push(Action::Reply(b"\x1b]l\x1b\\".to_vec())),
                    2 => actions.push(Action::ReportTitle),
                    _ => {}
                }
                Some(Vec::new())
            }
            (_, b'm') => self.sgr(params),
            _ => None,
        }
    }

    pub fn stop_printing(&mut self) {
        self.printing = None;
    }

    pub fn feed(&mut self, input: &[u8], actions: &mut Vec<Action>) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if let Some(p) = self.printing.as_mut().filter(|p| p.only) {
                if b == PRINT_END[p.matched] {
                    p.matched += 1;
                    if p.matched == PRINT_END.len() {
                        self.printing = None;
                        self.state = State::Ground;
                        actions.push(Action::PrintEnd);
                    }
                } else {
                    let mut data = PRINT_END[..p.matched].to_vec();
                    // an escape can be the real start of the end
                    if b == PRINT_END[0] {
                        p.matched = 1;
                    } else {
                        data.push(b);
                        p.matched = 0;
                    }
                    match actions.last_mut() {
                        Some(Action::Print(all)) => all.extend(data),
                        _ => actions.push(Action::Print(data)),
                    }
                }
                continue;
            }
            // auto print copies what reaches the screen, once it's been read
            let auto = self.printing.is_some();
            let start = out.len();
            let state = std::mem::replace(&mut self.state, State::Ground);
            self.state = match state {
                State::Ground => match b {
                    0x1b => State::Escape,
                    0x05 => {
                        if !self.config.answerback.is_empty() {
                            actions.push(Action::Reply(self.config.answerback.clone()));
                        }
                        State::Ground
                    }
                    0x0e | 0x0f if self.config.no_charset => State::Ground,
                    b'\r' if self.config.cr_implies_lf => {
                        out.extend(b"\r\n");
                        State::Ground
                    }
                    b'\n' if self.config.lf_implies_cr => {
                        out.extend(b"\r\n");
                        State::Ground
                    }
                    0x7f if !self.config.no_dbackspace => {
                        out.extend(b"\x08 \x08");
                        State::Ground
                    }
                    0x7f => {
                        out.push(0x08);
                        State::Ground
                    }
                    b => {
                        out.push(b);
                        State::Ground
                    }
                },
                // These follow the emulator's parser, as a sequence it reads
                // differently from this one would get past the settings here.
                State::Escape => match b {
                    0x1b => State::Escape,
                    0x18 | 0x1a => {
                        out.push(b);
                        State::Ground
                    }
                    0x7f => State::Escape,
                    0..=0x1f => {
                        out.push(b);
                        State::Escape
                    }
                    b'[' => State::Csi(Vec::new()),
                    b']' | b'P' | b'^' | b'_' | b'X' => {
                        out.extend([0x1b, b]);
                        State::Text(0)
                    }
                    b'(' | b')' | b'*' | b'+' if self.config.no_charset => State::Charset,
                    b => {
                        out.extend([0x1b, b]);
                        State::Ground
                    }
                },
                State::Charset => State::Ground,
                State::Csi(_) | State::CsiIgnore if b == 0x1b => State::Escape,
                State::Csi(_) | State::CsiIgnore if matches!(b, 0x18 | 0x1a) => {
                    out.push(b);
                    State::Ground
                }
                State::Csi(params) if b == 0x7f => State::Csi(params),
                State::CsiIgnore if b == 0x7f => State::CsiIgnore,
                State::Csi(params) if b < 0x20 => {
                    out.push(b);
                    State::Csi(params)
                }
                State::CsiIgnore if b < 0x20 => {
                    out.push(b);
                    State::CsiIgnore
                }
                State::CsiIgnore => {
                    if (0x40..=0x7e).contains(&b) {
                        State::Ground
                    } else {
                        State::CsiIgnore
                    }
                }
                State::Csi(mut params) => {
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'i' && self.config.printer {
                            match params.as_slice() {
                                b"5" | b"?5" => self.printing = Some(Printing { only: params[0] == b'5', matched: 0 }),
                                b"?4" => {
                                    if self.printing.take().is_some() {
                                        actions.push(Action::PrintEnd);
                                    }
                                }
                                _ => {}
                            }
                        }
                        match self.csi(&params, b, actions) {
                            Some(rewritten) => out.extend(rewritten),
                            None => {
                                out.extend(b"\x1b[");
                                out.extend(&params);
                                out.push(b);
                            }
                        }
                        State::Ground
                    } else if params.len() >= MAX_PARAMS {
                        State::CsiIgnore
                    } else {
                        params.push(b);
                        State::Csi(params)
                    }
                }
                State::Text(length) => match b {
                    0x07 | 0x18 | 0x1a => {
                        out.push(b);
                        State::Ground
                    }
                    // the string ends here, whatever the escape turns out to be
                    0x1b => {
                        out.extend(b"\x1b\\");
                        State::Escape
                    }
                    _ if length >= MAX_STRING => {
                        out.push(0x07);
                        State::TextIgnore
                    }
                    _ => {
                        out.push(b);
                        State::Text(length + 1)
                    }
                },
                State::TextIgnore => match b {
                    0x07 | 0x18 | 0x1a => State::Ground,
                    0x1b => State::Escape,
                    _ => State::TextIgnore,
                },
            };
            if auto && self.printing.is_some() && out.len() > start {
                match actions.last_mut() {
                    Some(Action::Print(data)) => data.extend(&out[start..]),
                    _ => actions.push(Action::Print(out[start..].to_vec())),
                }
            }
        }
        out
    }
}

/// `BellOverload*`: too many bells in a burst silences them for a while.
pub struct BellLimit {
    enabled: bool,
    limit: usize,
    window_ms: u64,
    silence_ms: u64,
    recent: Vec<u64>,
    overloaded: bool,
    last: u64,
}

impl BellLimit {
    pub fn new(s: &Settings) -> BellLimit {
        BellLimit {
            enabled: s.b("BellOverload", true),
            limit: s.i("BellOverloadN", 5) as usize,
            window_ms: s.i("BellOverloadT", 2000) as u64,
            silence_ms: s.i("BellOverloadS", 5000) as u64,
            recent: Vec::new(),
            overloaded: false,
            last: 0,
        }
    }

    /// Records a bell, and says whether it should be heard.
    pub fn ring(&mut self, now_ms: u64) -> bool {
        if !self.enabled {
            return true;
        }
        if self.overloaded && now_ms.saturating_sub(self.last) >= self.silence_ms {
            self.overloaded = false;
            self.recent.clear();
        }
        self.last = now_ms;
        // a flood lands here, so it has to stay cheap
        if self.overloaded {
            return false;
        }
        self.recent.retain(|t| now_ms.saturating_sub(*t) < self.window_ms);
        self.recent.push(now_ms);
        if self.recent.len() >= self.limit {
            self.overloaded = true;
        }
        !self.overloaded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Value;

    #[test]
    fn sequences_read_the_way_the_emulator_reads_them() {
        for evil in [&b"\x1b]x\x1b[3J"[..], b"\x1b\x1b[3J", b"\x1b[\x1b[3J", b"\x1b[03J", b"\x1b[3\x07J", b"\x1b[3;1J", b"\x1b]\xc2\x9c\x1b[3J"] {
            let out = run(&mut filter(&[("NoRemoteClearScroll", 1)]), evil).0;
            assert!(!out.windows(2).any(|w| w == b"3J") && !out.ends_with(b"J"), "{evil:?} gave {out:?}");
        }
        let out = run(&mut filter(&[("NoAltScreen", 1)]), b"\x1b\x1b[?01049h").0;
        assert!(!out.ends_with(b"h"), "{out:?}");
    }

    #[test]
    fn endless_sequences_do_not_keep_growing() {
        let mut f = filter(&[]);
        let zeros = vec![b'0'; 1 << 20];
        let mut out = run(&mut f, b"\x1b[").0;
        out.extend(run(&mut f, &zeros).0);
        assert!(out.is_empty() && matches!(f.state, State::CsiIgnore));
        assert_eq!(run(&mut f, b"m\x1b[1mok").0, b"\x1b[1mok");
        let mut f = filter(&[]);
        let long = vec![b'x'; 1 << 20];
        let out = run(&mut f, &[b"\x1b]0;", &long[..], b"\x07after"].concat()).0;
        assert!(out.len() < MAX_STRING + 100 && out.ends_with(b"after"));
    }

    #[test]
    fn synchronized_updates_cannot_freeze_the_display() {
        assert_eq!(run(&mut filter(&[]), b"\x1b[?2026h").0, b"");
        assert_eq!(run(&mut filter(&[]), b"\x1b[?25;2026l").0, b"\x1b[?25l");
    }

    #[test]
    fn a_flood_of_bells_stays_silent_and_cheap() {
        let mut limit = BellLimit::new(&Settings::default());
        let rung = (0..200_000).filter(|_| limit.ring(1000)).count();
        assert!(rung < 5 && limit.recent.len() < 10);
        assert!(limit.ring(1000 + 5000));
    }

    fn filter(pairs: &[(&str, u32)]) -> Filter {
        let mut s = Settings::default();
        for (k, v) in pairs {
            s.set(k, Value::Int(*v));
        }
        Filter::new(FilterConfig::new(&s))
    }

    fn run(f: &mut Filter, input: &[u8]) -> (Vec<u8>, Vec<Action>) {
        let mut a = Vec::new();
        (f.feed(input, &mut a), a)
    }

    #[test]
    fn plain_output_passes_unchanged_across_reads() {
        let mut f = filter(&[]);
        assert_eq!(run(&mut f, b"hi \x1b[3").0, b"hi ");
        assert_eq!(run(&mut f, b"1mred\x1b]0;t\x07").0, b"\x1b[31mred\x1b]0;t\x07");
    }

    #[test]
    fn newline_settings() {
        assert_eq!(run(&mut filter(&[("LFImpliesCR", 1)]), b"a\nb").0, b"a\r\nb");
        assert_eq!(run(&mut filter(&[("CRImpliesLF", 1)]), b"a\rb").0, b"a\r\nb");
    }

    #[test]
    fn alternate_screen_can_be_refused() {
        let mut f = filter(&[("NoAltScreen", 1)]);
        assert_eq!(run(&mut f, b"\x1b[?1049h").0, b"");
        assert_eq!(run(&mut f, b"\x1b[?1049;25h").0, b"\x1b[?25h");
        assert_eq!(run(&mut filter(&[]), b"\x1b[?1049h").0, b"\x1b[?1049h");
    }

    #[test]
    fn colours_step_down() {
        let mut f = filter(&[("TrueColour", 0)]);
        assert_eq!(run(&mut f, b"\x1b[38;2;255;0;0m").0, b"\x1b[38;5;196m");
        let mut f = filter(&[("Xterm256Colour", 0), ("TrueColour", 0)]);
        assert_eq!(run(&mut f, b"\x1b[1;38;5;9m").0, b"\x1b[1m");
        let mut f = filter(&[("ANSIColour", 0)]);
        assert_eq!(run(&mut f, b"\x1b[31m").0, b"", "dropping the colour must not turn into a reset");
        assert_eq!(run(&mut f, b"\x1b[1;31m").0, b"\x1b[1m");
    }

    #[test]
    fn answerback_and_title_query() {
        let (out, a) = run(&mut filter(&[]), b"\x05\x1b[21t");
        assert!(out.is_empty());
        assert_eq!(a, [Action::Reply(b"PuTTY".to_vec()), Action::Reply(b"\x1b]l\x1b\\".to_vec())]);
        assert_eq!(run(&mut filter(&[("RemoteQTitleAction", 2)]), b"\x1b[21t").1, [Action::ReportTitle]);
        assert!(run(&mut filter(&[("RemoteQTitleAction", 0)]), b"\x1b[21t").1.is_empty());
    }

    #[test]
    fn clearing_rules() {
        assert_eq!(run(&mut filter(&[("NoRemoteClearScroll", 1)]), b"\x1b[3J").0, b"");
        assert_eq!(run(&mut filter(&[]), b"\x1b[2J").0, b"\x1b[2J");
        assert_eq!(run(&mut filter(&[("EraseToScrollback", 0)]), b"\x1b[2J").0, b"\x1b7\x1b[H\x1b[J\x1b8");
    }

    #[test]
    fn charset_changes_can_be_refused() {
        let mut f = filter(&[("NoRemoteCharset", 1)]);
        assert_eq!(run(&mut f, b"a\x1b(0b\x0ec").0, b"abc");
    }

    #[test]
    fn delete_from_the_host() {
        assert_eq!(run(&mut filter(&[]), b"\x7f").0, b"\x08 \x08");
        assert_eq!(run(&mut filter(&[("NoDBackspace", 1)]), b"\x7f").0, b"\x08");
    }

    fn printer() -> Filter {
        let mut s = Settings::default();
        s.set("Printer", Value::Str("P".into()));
        Filter::new(FilterConfig::new(&s))
    }

    fn printed(a: &[Action]) -> Vec<u8> {
        a.iter().flat_map(|x| if let Action::Print(b) = x { b.clone() } else { Vec::new() }).collect()
    }

    #[test]
    fn printing_needs_a_printer() {
        let (out, a) = run(&mut filter(&[]), b"\x1b[5ihello\x1b[4i");
        assert!(a.is_empty());
        assert_eq!(out, b"\x1b[5ihello\x1b[4i");
    }

    #[test]
    fn a_print_job_goes_to_the_printer_not_the_screen() {
        let mut f = printer();
        let (out, a) = run(&mut f, b"a\x1b[5ipri");
        assert_eq!((out, printed(&a)), (b"a".to_vec(), b"pri".to_vec()));
        let (out, a) = run(&mut f, b"nt\x1b[3m\x1b[4");
        assert!(out.is_empty());
        assert_eq!(printed(&a), b"nt\x1b[3m");
        let (out, a) = run(&mut f, b"ib");
        assert_eq!(a, [Action::PrintEnd]);
        assert_eq!(out, b"b");
    }

    #[test]
    fn auto_print_also_shows() {
        let mut f = printer();
        let (out, a) = run(&mut f, b"\x1b[?5ihi\x1b[?4i!");
        assert_eq!(out, b"hi!");
        assert_eq!(printed(&a), b"hi");
        assert!(a.contains(&Action::PrintEnd));
    }

    #[test]
    fn bell_overload_silences_then_recovers() {
        let mut s = Settings::default();
        s.set("BellOverloadN", Value::Int(3));
        let mut b = BellLimit::new(&s);
        assert!(b.ring(0) && b.ring(10));
        assert!(!b.ring(20), "third bell in the window trips it");
        assert!(!b.ring(4000), "bells during the silence extend it");
        assert!(b.ring(10_000));
    }
}
