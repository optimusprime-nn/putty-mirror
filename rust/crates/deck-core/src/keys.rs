//! What keys do: the bytes they send, and the copy and paste rules.

use crate::settings::Settings;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Backspace,
    Tab,
    Enter,
    Escape,
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

const FUNKY_LINUX: u32 = 1;
const FUNKY_XTERM: u32 = 2;
const FUNKY_VT400: u32 = 3;
const FUNKY_VT100P: u32 = 4;
const FUNKY_SCO: u32 = 5;
const FUNKY_XTERM_216: u32 = 6;

/// The keyboard settings, and the key formats from PuTTY's `terminal.c`.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyConfig {
    funky: u32,
    bitmap_arrows: bool,
    rxvt_homeend: bool,
    no_app_cursor: bool,
    no_app_keypad: bool,
    nethack: bool,
    backspace_is_delete: bool,
}

impl Default for KeyConfig {
    fn default() -> KeyConfig {
        KeyConfig::new(&Settings::default())
    }
}

fn shift_bitmap(m: Mods) -> u32 {
    match m.shift as u32 + 2 * m.alt as u32 + 4 * m.ctrl as u32 {
        0 => 0,
        n => n + 1,
    }
}

impl KeyConfig {
    pub fn new(s: &Settings) -> KeyConfig {
        KeyConfig {
            funky: s.i("LinuxFunctionKeys", 0),
            bitmap_arrows: s.i("ShiftedArrowKeys", 0) == 1,
            rxvt_homeend: s.b("RXVTHomeEnd", false),
            no_app_cursor: s.b("NoApplicationCursors", false),
            no_app_keypad: s.b("NoApplicationKeys", false),
            nethack: s.b("NetHackKeypad", false),
            backspace_is_delete: s.b("BackspaceIsDelete", true),
        }
    }

    fn arrow(&self, c: u8, m: Mods, app_cursor: bool) -> Vec<u8> {
        let mut app = app_cursor && !self.no_app_cursor;
        let mut bitmap = 0;
        if self.bitmap_arrows {
            bitmap = shift_bitmap(m);
        } else if m.ctrl {
            app = !app;
        }
        let c = c as char;
        if app {
            format!("\x1bO{c}").into_bytes()
        } else if bitmap != 0 {
            format!("\x1b[1;{bitmap}{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    }

    fn function(&self, number: u8, m: Mods) -> Vec<u8> {
        const CODES: [u32; 21] = [0, 11, 12, 13, 14, 15, 17, 18, 19, 20, 21, 23, 24, 25, 26, 28, 29, 31, 32, 33, 34];
        const SCO: &[u8] = b"MNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz@[\\]^_`{";
        let f = self.funky;
        let mut index = number as usize;
        let mut m = m;
        if f != FUNKY_XTERM_216 && f != FUNKY_SCO && m.shift && index <= 10 {
            m.shift = false;
            index += 10;
        }
        let Some(&code) = CODES.get(index).filter(|c| **c != 0) else { return Vec::new() };
        let letter = |base: u32, off: u32| char::from_u32(code + base - 11 - off).unwrap_or(' ');
        if f == FUNKY_SCO {
            let i = if (1..=12).contains(&number) { number as usize - 1 } else { 0 } + 12 * m.shift as usize + 24 * m.ctrl as usize;
            vec![0x1b, b'[', SCO[i]]
        } else if f == FUNKY_VT100P && (11..=24).contains(&code) {
            let off = (code > 15) as u32 + (code > 21) as u32;
            format!("\x1bO{}", letter('P' as u32, off)).into_bytes()
        } else if f == FUNKY_LINUX && (11..=15).contains(&code) {
            format!("\x1b[[{}", letter('A' as u32, 0)).into_bytes()
        } else if (f == FUNKY_XTERM || f == FUNKY_XTERM_216) && (11..=14).contains(&code) {
            let bitmap = if f == FUNKY_XTERM_216 { shift_bitmap(m) } else { 0 };
            let c = letter('P' as u32, 0);
            if bitmap != 0 { format!("\x1b[1;{bitmap}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
        } else {
            let bitmap = if f == FUNKY_XTERM_216 { shift_bitmap(m) } else { 0 };
            if bitmap != 0 { format!("\x1b[{code};{bitmap}~").into_bytes() } else { format!("\x1b[{code}~").into_bytes() }
        }
    }

    fn small_keypad(&self, code: u8, m: Mods) -> Vec<u8> {
        let code = if self.funky == FUNKY_VT400 { [0, 2, 1, 4, 5, 3, 6][code as usize] } else { code };
        if self.funky == FUNKY_SCO {
            return match code {
                3 => vec![0x7f],
                c => vec![0x1b, b'[', b"HL.FIG"[c as usize - 1]],
            };
        }
        if self.rxvt_homeend && (code == 1 || code == 4) {
            return if code == 1 { b"\x1b[H".to_vec() } else { b"\x1bOw".to_vec() };
        }
        let bitmap = if self.funky == FUNKY_XTERM_216 { shift_bitmap(m) } else { 0 };
        if bitmap != 0 { format!("\x1b[{code};{bitmap}~").into_bytes() } else { format!("\x1b[{code}~").into_bytes() }
    }

    /// Whether `encode` puts Alt into this key's sequence. Otherwise Alt is
    /// sent as an Escape first.
    pub fn alt_included(&self, key: Key) -> bool {
        match key {
            Key::Up | Key::Down | Key::Left | Key::Right => self.bitmap_arrows,
            Key::Home | Key::End | Key::Insert | Key::Delete | Key::PageUp | Key::PageDown | Key::F(_) => self.funky == FUNKY_XTERM_216,
            _ => false,
        }
    }

    /// The bytes a key sends.
    pub fn encode(&self, key: Key, m: Mods, app_cursor: bool) -> Vec<u8> {
        match key {
            Key::Up => self.arrow(b'A', m, app_cursor),
            Key::Down => self.arrow(b'B', m, app_cursor),
            Key::Right => self.arrow(b'C', m, app_cursor),
            Key::Left => self.arrow(b'D', m, app_cursor),
            Key::Home => self.small_keypad(1, m),
            Key::Insert => self.small_keypad(2, m),
            Key::Delete => self.small_keypad(3, m),
            Key::End => self.small_keypad(4, m),
            Key::PageUp => self.small_keypad(5, m),
            Key::PageDown => self.small_keypad(6, m),
            // Shift gives the other backspace, as in PuTTY
            Key::Backspace => vec![if self.backspace_is_delete != m.shift { 0x7f } else { 0x08 }],
            Key::Tab if m.shift => b"\x1b[Z".to_vec(),
            Key::Tab => vec![b'\t'],
            Key::Enter => vec![b'\r'],
            Key::Escape => vec![0x1b],
            Key::F(n) => self.function(n, m),
        }
    }

    /// A numeric keypad key, as its digit or symbol, or `\r` for Enter. Empty
    /// means it's an ordinary character. 'G' is the Num Lock key.
    pub fn keypad(&self, key: u8, m: Mods, app_keypad: bool) -> Vec<u8> {
        let app = app_keypad && !self.no_app_keypad;
        if self.nethack && (b'1'..=b'9').contains(&key) {
            let c = b"bjnh.lyku"[(key - b'1') as usize];
            return vec![match c {
                b'.' => c,
                c if m.ctrl => c & 0x1f,
                c if m.shift => c - 32,
                c => c,
            }];
        }
        let f = self.funky;
        let mut x = 0;
        if f == FUNKY_VT400 || (f <= FUNKY_LINUX && app) {
            x = match key {
                b'G' => b'P',
                b'/' => b'Q',
                b'*' => b'R',
                b'-' => b'S',
                _ => 0,
            };
        }
        if app {
            let xterm = f == FUNKY_XTERM;
            x = match key {
                b'0'..=b'9' => key - b'0' + b'p',
                b'.' => b'n',
                b'\r' => b'M',
                b'+' if xterm => if m.shift { b'l' } else { b'k' },
                b'+' => if m.shift { b'm' } else { b'l' },
                b'/' if xterm => b'o',
                b'*' if xterm => b'j',
                b'-' if xterm => b'm',
                _ => x,
            };
        }
        if x == 0 { Vec::new() } else { vec![0x1b, b'O', x] }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shortcut {
    Copy,
    Paste,
    NewSession,
    Duplicate,
    CloseSession,
    GoTo(usize),
    /// Not a shortcut: the key goes to the session.
    PassThrough,
}

/// The same rules as the C PuTTYDeck. Ctrl+C copies only with text selected,
/// and Ctrl+V leaves full-screen programs alone. `key` is the Windows
/// virtual-key code, which is the upper-case letter or the digit.
pub fn shortcut(key: u8, ctrl: bool, shift: bool, has_selection: bool, alt_screen: bool) -> Shortcut {
    if !ctrl {
        return Shortcut::PassThrough;
    }
    match (key, shift) {
        (b'C', true) => Shortcut::Copy,
        (b'V', true) => Shortcut::Paste,
        (b'T', true) => Shortcut::NewSession,
        (b'D', true) => Shortcut::Duplicate,
        (b'W', true) => Shortcut::CloseSession,
        (b'1'..=b'9', true) => Shortcut::GoTo((key - b'1') as usize),
        (b'C', false) if has_selection => Shortcut::Copy,
        (b'V', false) if !alt_screen => Shortcut::Paste,
        _ => Shortcut::PassThrough,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(key: Key, shift: bool, app: bool) -> Vec<u8> {
        KeyConfig::default().encode(key, Mods { shift, ..Mods::default() }, app)
    }

    fn with(key: &str, v: u32) -> KeyConfig {
        let mut s = Settings::default();
        s.set(key, crate::settings::Value::Int(v));
        KeyConfig::new(&s)
    }

    const NONE: Mods = Mods { shift: false, ctrl: false, alt: false };
    const CTRL: Mods = Mods { shift: false, ctrl: true, alt: false };
    const SHIFT: Mods = Mods { shift: true, ctrl: false, alt: false };

    #[test]
    fn cursor_keys_follow_mode() {
        assert_eq!(encode(Key::Up, false, false), b"\x1b[A");
        assert_eq!(encode(Key::Up, false, true), b"\x1bOA");
    }

    #[test]
    fn application_cursors_can_be_refused_and_ctrl_flips_them() {
        assert_eq!(with("NoApplicationCursors", 1).encode(Key::Up, NONE, true), b"\x1b[A");
        assert_eq!(KeyConfig::default().encode(Key::Up, CTRL, false), b"\x1bOA");
        assert_eq!(with("ShiftedArrowKeys", 1).encode(Key::Up, CTRL, false), b"\x1b[1;5A");
    }

    #[test]
    fn function_key_styles() {
        assert_eq!(with("LinuxFunctionKeys", 1).encode(Key::F(1), NONE, false), b"\x1b[[A");
        assert_eq!(with("LinuxFunctionKeys", 2).encode(Key::F(1), NONE, false), b"\x1bOP");
        assert_eq!(with("LinuxFunctionKeys", 4).encode(Key::F(12), NONE, false), b"\x1bO[");
        assert_eq!(with("LinuxFunctionKeys", 5).encode(Key::F(1), NONE, false), b"\x1b[M");
        assert_eq!(with("LinuxFunctionKeys", 6).encode(Key::F(5), SHIFT, false), b"\x1b[15;2~");
        assert_eq!(KeyConfig::default().encode(Key::F(1), SHIFT, false), b"\x1b[23~", "Shift+F1 is F11");
    }

    #[test]
    fn home_end_and_backspace_settings() {
        assert_eq!(with("RXVTHomeEnd", 1).encode(Key::End, NONE, false), b"\x1bOw");
        assert_eq!(with("LinuxFunctionKeys", 3).encode(Key::Home, NONE, false), b"\x1b[2~");
        assert_eq!(with("BackspaceIsDelete", 0).encode(Key::Backspace, NONE, false), [8]);
        assert_eq!(with("BackspaceIsDelete", 0).encode(Key::Backspace, SHIFT, false), [0x7f]);
    }

    #[test]
    fn keypad_modes() {
        let k = KeyConfig::default();
        assert!(k.keypad(b'5', NONE, false).is_empty());
        assert_eq!(k.keypad(b'5', NONE, true), b"\x1bOu");
        assert_eq!(k.keypad(b'+', SHIFT, true), b"\x1bOm");
        assert_eq!(with("LinuxFunctionKeys", 2).keypad(b'+', NONE, true), b"\x1bOk");
        assert!(with("NoApplicationKeys", 1).keypad(b'5', NONE, true).is_empty());
        assert_eq!(with("NetHackKeypad", 1).keypad(b'4', SHIFT, false), b"H");
        assert_eq!(with("NetHackKeypad", 1).keypad(b'4', CTRL, false), [8]);
    }

    #[test]
    fn function_keys_match_putty() {
        assert_eq!(encode(Key::F(1), false, false), b"\x1b[11~");
        assert_eq!(encode(Key::F(5), false, false), b"\x1b[15~");
        assert_eq!(encode(Key::F(6), false, false), b"\x1b[17~");
        assert_eq!(encode(Key::F(10), false, false), b"\x1b[21~");
        assert_eq!(encode(Key::F(11), false, false), b"\x1b[23~");
        assert_eq!(encode(Key::F(12), false, false), b"\x1b[24~");
        assert_eq!(encode(Key::Backspace, false, false), b"\x7f");
    }

    #[test]
    fn ctrl_c_only_copies_a_selection() {
        assert_eq!(shortcut(b'C', true, false, true, false), Shortcut::Copy);
        assert_eq!(shortcut(b'C', true, false, false, false), Shortcut::PassThrough);
        assert_eq!(shortcut(b'C', true, true, false, false), Shortcut::Copy);
    }

    #[test]
    fn ctrl_v_stays_out_of_full_screen_programs() {
        assert_eq!(shortcut(b'V', true, false, false, false), Shortcut::Paste);
        assert_eq!(shortcut(b'V', true, false, false, true), Shortcut::PassThrough);
        assert_eq!(shortcut(b'V', true, true, false, true), Shortcut::Paste);
    }

    #[test]
    fn numbered_sessions() {
        assert_eq!(shortcut(b'1', true, true, false, false), Shortcut::GoTo(0));
        assert_eq!(shortcut(b'9', true, true, false, false), Shortcut::GoTo(8));
        assert_eq!(shortcut(b'1', true, false, false, false), Shortcut::PassThrough);
        assert_eq!(shortcut(b'T', false, true, false, false), Shortcut::PassThrough);
    }
}
