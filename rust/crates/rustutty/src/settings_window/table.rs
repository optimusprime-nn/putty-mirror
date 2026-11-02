//! The configuration box's control table: a port of `setup_config_box` in
//! PuTTY's config.c and `win_setup_config_box` in windows/config.c, in the same
//! order, using the same insertion rules for control sets. Each control says
//! which saved keyword it edits.

use deck_core::options::{Kind as OptionKind, OPTIONS};

pub const NONE: char = '\0';
pub const APP: &str = "RUSTuTTY";

#[derive(Clone, Debug)]
pub enum Kind {
    Columns(Vec<i32>),
    TabDelay(usize),
    Text(String),
    Edit { label: Option<String>, sc: char, pct: i32, combo: bool, password: bool },
    Radio { label: Option<String>, sc: char, across: i32, buttons: Vec<(String, char)> },
    Check { label: String, sc: char },
    Button { label: String, sc: char, default: bool, cancel: bool },
    /// `height` 0 is a drop-down list.
    List { label: Option<String>, sc: char, height: i32, drag: bool, multi: bool, pct: i32, tabs: Vec<i32> },
    File { label: String, sc: char, title: &'static str, filter: &'static str, write: bool },
    Font { label: String, sc: char },
}

/// Groups of controls that work together, each control being a numbered part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Group {
    Proto,
    Saved,
    Class,
    Colour,
    Tty,
    Env,
    Fwd,
    Hk,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prefs {
    Cipher,
    Kex,
    Host,
    Gss,
}

/// What a control reads and writes.
#[derive(Clone, Debug, PartialEq)]
pub enum Bind {
    None,
    Str(&'static str, &'static str),
    Int(&'static str, u32),
    /// Shown divided by the denominator.
    Fixed(&'static str, u32, u32),
    Flag(&'static str, bool),
    NotFlag(&'static str, bool),
    /// The stored value of each button.
    Radio(&'static str, u32, &'static [u32]),
    Drop(&'static str, u32, &'static [(&'static str, u32)]),
    Clip(&'static str, &'static str),
    Prefs(Prefs),
    Font(&'static str),
    Host,
    Port,
    Ping,
    Keypad,
    Printer,
    Codepage,
    VarPitch,
    HostCas,
    About,
    Help,
    Part(Group, u8),
}

#[derive(Clone, Debug)]
pub struct Ctl {
    pub kind: Kind,
    pub bind: Bind,
    /// First column and span.
    pub col: (usize, usize),
    pub delay: bool,
    pub align: Option<usize>,
}

impl Ctl {
    fn at(&mut self, column: usize) -> &mut Ctl {
        self.col = (column, 1);
        self
    }
}

#[derive(Clone, Debug)]
pub struct Set {
    pub path: String,
    /// None for a panel title.
    pub name: Option<String>,
    pub title: Option<String>,
    pub ctrls: Vec<Ctl>,
    /// Columns in force, which a new control spans unless told otherwise.
    columns: usize,
}

#[derive(Default)]
pub struct Cbox {
    pub sets: Vec<Set>,
}

/// `ctrl_path_compare`: matching path elements, or `i32::MAX` for the same path.
pub fn path_compare(a: &str, b: &str) -> i32 {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut i = 0;
    let mut n = 0;
    while i < a.len() || i < b.len() {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if matches!(x, b'/' | 0) && matches!(y, b'/' | 0) {
            n += 1;
        }
        if x != y {
            return n;
        }
        i += 1;
    }
    i32::MAX
}

fn opt(key: &str) -> Option<OptionKind> {
    OPTIONS.iter().find(|o| o.key == key).map(|o| o.kind)
}

/// A number setting's default: what the deck does when it's unset, or PuTTY's.
fn num(key: &'static str, putty: u32) -> u32 {
    match opt(key) {
        Some(OptionKind::Number(d) | OptionKind::Choice(d, _)) => d,
        _ => putty,
    }
}

fn int(key: &'static str, putty: u32) -> Bind {
    Bind::Int(key, num(key, putty))
}

fn flag(key: &'static str, putty: bool) -> Bind {
    match opt(key) {
        Some(OptionKind::Flag(d)) => Bind::Flag(key, d),
        _ => Bind::Flag(key, putty),
    }
}

fn radio(key: &'static str, putty: u32, values: &'static [u32]) -> Bind {
    Bind::Radio(key, num(key, putty), values)
}

fn strs(key: &'static str) -> Bind {
    Bind::Str(key, "")
}

const ASK: u32 = u32::MAX;
const AUTO_BUGS: &[(&str, u32)] = &[("Auto", 0), ("Off", 1), ("On", 2)];
const MANUAL_BUGS: &[(&str, u32)] = &[("Off", 1), ("On", 2)];
const PARITIES: &[(&str, u32)] = &[("None", 0), ("Odd", 1), ("Even", 2), ("Mark", 3), ("Space", 4)];
const FLOWS: &[(&str, u32)] = &[("None", 0), ("XON/XOFF", 1), ("RTS/CTS", 2), ("DSR/DTR", 3)];
const PROXIES: &[(&str, u32)] = &[
    ("None", 0),
    ("SOCKS 5", 2),
    ("SOCKS 4", 1),
    ("HTTP CONNECT", 3),
    ("SSH to proxy and use port forwarding", 6),
    ("SSH to proxy and execute a command", 7),
    ("SSH to proxy and invoke a subsystem", 8),
    ("Local (run a subprogram to connect)", 5),
    ("'Telnet' (send an ad-hoc command)", 4),
];

const ALL_FILES: &str = "All Files (*.*)\0*\0";
const KEY_FILES: &str = "PuTTY Private Key Files (*.ppk)\0*.ppk\0All Files (*.*)\0*\0";
const SOUND_FILES: &str = "Wave Files (*.WAV)\0*.WAV\0All Files (*.*)\0*\0";
const DYNLIB_FILES: &str = "Dynamic Library Files (*.dll)\0*.dll\0All Files (*.*)\0*\0";

impl Cbox {
    fn find_set(&self, path: &str, start: bool) -> usize {
        let mut last = 0;
        for (i, s) in self.sets.iter().enumerate() {
            let this = path_compare(path, &s.path);
            if (start && this == i32::MAX) || this < last {
                return i;
            }
            last = this;
        }
        self.sets.len()
    }

    fn settitle(&mut self, path: &str, title: &str) {
        let i = self.find_set(path, true);
        self.sets.insert(i, Set { path: path.into(), name: None, title: Some(title.into()), ctrls: Vec::new(), columns: 1 });
    }

    fn getset(&mut self, path: &str, name: &str, title: Option<&str>) -> usize {
        let mut i = self.find_set(path, true);
        while i < self.sets.len() && self.sets[i].path == path {
            if self.sets[i].name.as_deref() == Some(name) {
                return i;
            }
            i += 1;
        }
        self.sets.insert(i, Set { path: path.into(), name: Some(name.into()), title: title.map(String::from), ctrls: Vec::new(), columns: 1 });
        i
    }

    fn push(&mut self, s: usize, kind: Kind, bind: Bind) -> &mut Ctl {
        let set = &mut self.sets[s];
        if let Kind::Columns(widths) = &kind {
            set.columns = widths.len();
        }
        set.ctrls.push(Ctl { kind, bind, col: (0, set.columns), delay: false, align: None });
        set.ctrls.last_mut().unwrap()
    }

    fn columns(&mut self, s: usize, widths: &[i32]) {
        self.push(s, Kind::Columns(widths.to_vec()), Bind::None);
    }

    fn text(&mut self, s: usize, text: &str) -> &mut Ctl {
        self.push(s, Kind::Text(text.into()), Bind::None)
    }

    fn edit(&mut self, s: usize, label: &str, sc: char, pct: i32, bind: Bind) -> &mut Ctl {
        self.push(s, Kind::Edit { label: Some(label.into()), sc, pct, combo: false, password: false }, bind)
    }

    fn combo(&mut self, s: usize, label: &str, sc: char, bind: Bind) -> &mut Ctl {
        self.push(s, Kind::Edit { label: Some(label.into()), sc, pct: 100, combo: true, password: false }, bind)
    }

    fn radio(&mut self, s: usize, label: Option<&str>, sc: char, across: i32, buttons: &[(&str, char)], bind: Bind) -> &mut Ctl {
        let buttons = buttons.iter().map(|(t, c)| (t.to_string(), *c)).collect();
        self.push(s, Kind::Radio { label: label.map(String::from), sc, across, buttons }, bind)
    }

    /// Radio buttons with no shortcuts of their own.
    fn radios(&mut self, s: usize, label: &str, sc: char, across: i32, buttons: &[&str], bind: Bind) -> &mut Ctl {
        let b: Vec<_> = buttons.iter().map(|t| (*t, NONE)).collect();
        self.radio(s, Some(label), sc, across, &b, bind)
    }

    fn check(&mut self, s: usize, label: &str, sc: char, bind: Bind) -> &mut Ctl {
        self.push(s, Kind::Check { label: label.into(), sc }, bind)
    }

    fn button(&mut self, s: usize, label: &str, sc: char, bind: Bind) -> &mut Ctl {
        self.push(s, Kind::Button { label: label.into(), sc, default: false, cancel: false }, bind)
    }

    fn list(&mut self, s: usize, label: Option<&str>, sc: char, height: i32, tabs: &[i32], bind: Bind) -> &mut Ctl {
        let kind = Kind::List { label: label.map(String::from), sc, height, drag: false, multi: false, pct: 100, tabs: tabs.to_vec() };
        self.push(s, kind, bind)
    }

    fn drop(&mut self, s: usize, label: &str, sc: char, pct: i32, bind: Bind) -> &mut Ctl {
        let kind = Kind::List { label: Some(label.into()), sc, height: 0, drag: false, multi: false, pct, tabs: Vec::new() };
        self.push(s, kind, bind)
    }

    fn drag(&mut self, s: usize, label: &str, sc: char, height: i32, which: Prefs) -> &mut Ctl {
        let kind = Kind::List { label: Some(label.into()), sc, height, drag: true, multi: false, pct: 100, tabs: Vec::new() };
        self.push(s, kind, Bind::Prefs(which))
    }

    fn file(&mut self, s: usize, label: &str, sc: char, filter: &'static str, write: bool, title: &'static str, key: &'static str) -> &mut Ctl {
        self.push(s, Kind::File { label: label.into(), sc, title, filter, write }, strs(key))
    }

    fn bug(&mut self, s: usize, label: &str, sc: char, key: &'static str) {
        self.drop(s, label, sc, 20, Bind::Drop(key, 0, AUTO_BUGS));
    }

    fn manual_bug(&mut self, s: usize, label: &str, sc: char, key: &'static str) {
        self.drop(s, label, sc, 20, Bind::Drop(key, 1, MANUAL_BUGS));
    }

    fn last(&self, s: usize) -> usize {
        self.sets[s].ctrls.len() - 1
    }
}

/// `setup_config_box` and `win_setup_config_box` for the settings a session
/// holds. `protocol` is only used mid-session, where it decides the panels.
pub fn build(mid: bool, protocol: &str, user: &str) -> Cbox {
    let mut cb = Cbox::default();
    let b = &mut cb;
    let is = |p: &str| !mid || protocol == p;
    let ssh = !mid || protocol == "ssh" || protocol == "ssh-connection";
    let resize_forbidden = protocol == "supdup";

    // The standard panel at the bottom: Open, Cancel, About and Help
    let s = b.getset("", "", Some(""));
    b.columns(s, &[20, 20, 20, 20, 20]);
    let ok = if mid { ("Apply", 'a') } else { ("Open", 'o') };
    b.push(s, Kind::Button { label: ok.0.into(), sc: ok.1, default: true, cancel: false }, Bind::Part(Group::Saved, 5)).at(3);
    b.push(s, Kind::Button { label: "Cancel".into(), sc: 'c', default: false, cancel: true }, Bind::Part(Group::Saved, 6)).at(4);
    if !mid {
        b.button(s, "About", 'a', Bind::About).at(0);
        b.button(s, "Help", 'h', Bind::Help).at(1);
    }

    // Session
    b.settitle("Session", &format!("Basic options for your {APP} session"));
    if !mid {
        let s = b.getset("Session", "hostport", Some("Specify the destination you want to connect to"));
        b.columns(s, &[75, 25]);
        b.edit(s, "Host Name (or IP address)", 'n', 100, Bind::Host).at(0);
        b.edit(s, "Port", 'p', 100, Bind::Port).at(1);
        b.columns(s, &[100]);
        b.text(s, "Connection type:");
        b.columns(s, &[62, 38]);
        b.radio(s, None, NONE, 3, &[("SSH", 's'), ("Serial", 'r'), ("Other:", 't')], Bind::Part(Group::Proto, 0)).at(0);
        let radio_at = b.last(s);
        let c = b.list(s, None, NONE, 0, &[], Bind::Part(Group::Proto, 1)).at(1);
        c.align = Some(radio_at);
        b.columns(s, &[100]);
    }
    let title = if mid { "Save the current session settings" } else { "Load, save or delete a stored session" };
    let s = b.getset("Session", "savedsessions", Some(title));
    b.columns(s, &[75, 25]);
    b.edit(s, "Saved Sessions", 'e', 100, Bind::Part(Group::Saved, 0)).at(0);
    b.columns(s, &[100]);
    b.columns(s, &[75, 25]);
    b.list(s, None, NONE, 7, &[], Bind::Part(Group::Saved, 1)).at(0);
    if !mid {
        b.button(s, "Load", 'l', Bind::Part(Group::Saved, 2)).at(1);
    }
    b.button(s, "Save", 'v', Bind::Part(Group::Saved, 3)).at(1);
    if !mid {
        b.button(s, "Delete", 'd', Bind::Part(Group::Saved, 4)).at(1);
    }
    b.columns(s, &[100]);
    let s = b.getset("Session", "otheropts", None);
    b.radios(s, "Close window on exit:", 'x', 4, &["Always", "Never", "Only on clean exit"], radio("CloseOnExit", 1, &[2, 0, 1]));

    // Session/Logging
    b.settitle("Session/Logging", "Options controlling session logging");
    let s = b.getset("Session/Logging", "main", None);
    let mut log = vec![("None", 't'), ("Printable output", 'p'), ("All session output", 'l')];
    let mut logs: &'static [u32] = &[0, 1, 2];
    if ssh {
        log.extend([("SSH packets", 's'), ("SSH packets and raw data", 'r')]);
        logs = &[0, 1, 2, 3, 4];
    }
    b.radio(s, Some("Session logging:"), NONE, 2, &log, radio("LogType", 0, logs));
    b.file(s, "Log file name:", 'f', ALL_FILES, true, "Select session log file name", "LogFileName").bind = Bind::Str("LogFileName", "putty.log");
    b.text(s, "(Log file name can contain &Y, &M, &D for date, &T for time, &H for host name, and &P for port number)");
    b.radios(s, "What to do if the log file already exists:", 'e', 1, &["Always overwrite it", "Always append to the end of it", "Ask the user every time"], radio("LogFileClash", ASK, &[1, 0, ASK]));
    b.check(s, "Flush log file frequently", 'u', flag("LogFlush", true));
    b.check(s, "Include header", 'i', flag("LogHeader", true));
    if ssh {
        let s = b.getset("Session/Logging", "ssh", Some("Options specific to SSH packet logging"));
        b.check(s, "Omit known password fields", 'k', flag("SSHLogOmitPasswords", true));
        b.check(s, "Omit session data", 'd', flag("SSHLogOmitData", false));
    }

    // Terminal
    b.settitle("Terminal", "Options controlling the terminal emulation");
    let s = b.getset("Terminal", "general", Some("Set various terminal options"));
    b.check(s, "Auto wrap mode initially on", 'w', flag("AutoWrapMode", true));
    b.check(s, "DEC Origin Mode initially on", 'd', flag("DECOriginMode", false));
    b.check(s, "Implicit CR in every LF", 'r', flag("LFImpliesCR", false));
    b.check(s, "Implicit LF in every CR", 'f', flag("CRImpliesLF", false));
    b.check(s, "Use background colour to erase screen", 'e', flag("BCE", true));
    b.check(s, "Enable blinking text", 'n', flag("BlinkText", false));
    b.edit(s, "Answerback to ^E:", 's', 100, Bind::Str("Answerback", "PuTTY"));
    let s = b.getset("Terminal", "ldisc", Some("Line discipline options"));
    b.radios(s, "Local echo:", 'l', 3, &["Auto", "Force on", "Force off"], radio("LocalEcho", 2, &[2, 0, 1]));
    b.radios(s, "Local line editing:", 't', 3, &["Auto", "Force on", "Force off"], radio("LocalEdit", 2, &[2, 0, 1]));
    let s = b.getset("Terminal", "printing", Some("Remote-controlled printing"));
    b.combo(s, "Printer to send ANSI printer output to:", 'p', Bind::Printer);

    // Terminal/Keyboard
    b.settitle("Terminal/Keyboard", "Options controlling the effects of keys");
    let s = b.getset("Terminal/Keyboard", "mappings", Some("Change the sequences sent by:"));
    b.radios(s, "The Backspace key", 'b', 2, &["Control-H", "Control-? (127)"], Bind::Radio("BackspaceIsDelete", 1, &[0, 1]));
    b.radios(s, "The Home and End keys", 'e', 2, &["Standard", "rxvt"], Bind::Radio("RXVTHomeEnd", 0, &[0, 1]));
    b.radios(s, "The Function keys and keypad", 'f', 4, &["ESC[n~", "Linux", "Xterm R6", "VT400", "VT100+", "SCO", "Xterm 216+"], radio("LinuxFunctionKeys", 0, &[0, 1, 2, 3, 4, 5, 6]));
    b.radios(s, "Shift/Ctrl/Alt with the arrow keys", 'w', 2, &["Ctrl toggles app mode", "xterm-style bitmap"], radio("ShiftedArrowKeys", 0, &[0, 1]));
    let s = b.getset("Terminal/Keyboard", "appkeypad", Some("Application keypad settings:"));
    b.radios(s, "Initial state of cursor keys:", 'r', 3, &["Normal", "Application"], Bind::Radio("ApplicationCursorKeys", 0, &[0, 1]));
    b.radios(s, "Initial state of numeric keypad:", 'n', 3, &["Normal", "Application", "NetHack"], Bind::Keypad);
    let s = b.getset("Terminal/Keyboard", "features", Some("Enable extra keyboard features:"));
    b.check(s, "AltGr acts as Compose key", 't', flag("ComposeKey", false));
    b.check(s, "Control-Alt is different from AltGr", 'd', flag("CtrlAltKeys", true));

    // Terminal/Bell
    b.settitle("Terminal/Bell", "Options controlling the terminal bell");
    let s = b.getset("Terminal/Bell", "style", Some("Set the style of bell"));
    b.radios(
        s,
        "Action to happen when a bell occurs:",
        'b',
        1,
        &["None (bell disabled)", "Make default system alert sound", "Visual bell (flash window)", "Beep using the PC speaker", "Play a custom sound file"],
        radio("Beep", 1, &[0, 1, 2, 4, 3]),
    );
    b.file(s, "Custom sound file to play as a bell:", NONE, SOUND_FILES, false, "Select bell sound file", "BellWaveFile");
    b.radios(s, "Taskbar/caption indication on bell:", 'i', 3, &["Disabled", "Flashing", "Steady"], radio("BeepInd", 0, &[0, 1, 2]));
    let s = b.getset("Terminal/Bell", "overload", Some("Control the bell overload behaviour"));
    b.check(s, "Bell is temporarily disabled when over-used", 'd', flag("BellOverload", true));
    b.edit(s, "Over-use means this many bells...", 'm', 20, int("BellOverloadN", 5));
    b.edit(s, "... in this many seconds", 't', 20, Bind::Fixed("BellOverloadT", num("BellOverloadT", 2000), 1000));
    b.text(s, "The bell is re-enabled after a few seconds of silence.");
    b.edit(s, "Seconds of silence required", 's', 20, Bind::Fixed("BellOverloadS", num("BellOverloadS", 5000), 1000));

    // Terminal/Features
    b.settitle("Terminal/Features", "Enabling and disabling advanced terminal features");
    let s = b.getset("Terminal/Features", "main", None);
    b.check(s, "Disable application cursor keys mode", 'u', flag("NoApplicationCursors", false));
    b.check(s, "Disable application keypad mode", 'k', flag("NoApplicationKeys", false));
    b.check(s, "Disable xterm-style mouse reporting", 'x', flag("NoMouseReporting", false));
    b.check(s, "Disable remote-controlled terminal resizing", 's', flag("NoRemoteResize", false));
    b.check(s, "Disable switching to alternate terminal screen", 'w', flag("NoAltScreen", false));
    b.check(s, "Disable remote-controlled window title changing", 't', flag("NoRemoteWinTitle", false));
    b.radios(s, "Response to remote title query (SECURITY):", 'q', 3, &["None", "Empty string", "Window title"], radio("RemoteQTitleAction", 1, &[0, 1, 2]));
    b.check(s, "Disable remote-controlled clearing of scrollback", 'e', flag("NoRemoteClearScroll", false));
    b.check(s, "Disable destructive backspace on server sending ^?", 'b', flag("NoDBackspace", false));
    b.check(s, "Disable remote-controlled character set configuration", 'r', flag("NoRemoteCharset", false));
    b.check(s, "Disable Arabic text shaping", 'l', flag("DisableArabicShaping", false));
    b.check(s, "Disable bidirectional text display", 'd', flag("DisableBidi", false));
    b.check(s, "Disable bracketed paste mode", 'p', flag("DisableBracketedPaste", false));

    // Window
    b.settitle("Window", &format!("Options controlling {APP}'s window"));
    if !resize_forbidden || !mid {
        let s = b.getset("Window", "size", Some("Set the size of the window"));
        b.columns(s, &[50, 50]);
        b.edit(s, "Columns", 'm', 100, int("TermWidth", 80)).at(0);
        b.edit(s, "Rows", 'r', 100, int("TermHeight", 24)).at(1);
        b.columns(s, &[100]);
        b.radios(
            s,
            "When window is resized:",
            'z',
            1,
            &["Change the number of rows and columns", "Change the size of the font", "Change font size only when maximised", "Forbid resizing completely"],
            radio("LockSize", 0, &[0, 2, 3, 1]),
        );
    }
    let s = b.getset("Window", "scrollback", Some("Control the scrollback in the window"));
    b.edit(s, "Lines of scrollback", 's', 50, int("ScrollbackLines", 2000));
    b.check(s, "Display scrollbar", 'd', flag("ScrollBar", true));
    b.check(s, "Display scrollbar in full screen mode", 'i', flag("ScrollBarFullScreen", false));
    b.check(s, "Reset scrollback on keypress", 'k', flag("ScrollOnKey", false));
    b.check(s, "Reset scrollback on display activity", 'p', flag("ScrollOnDisp", true));
    b.check(s, "Push erased text into scrollback", 'e', flag("EraseToScrollback", true));

    // Window/Appearance
    b.settitle("Window/Appearance", &format!("Configure the appearance of {APP}'s window"));
    let s = b.getset("Window/Appearance", "cursor", Some("Adjust the use of the cursor"));
    b.radio(s, Some("Cursor appearance:"), NONE, 3, &[("Block", 'l'), ("Underline", 'u'), ("Vertical line", 'v')], radio("CurType", 0, &[0, 1, 2]));
    b.check(s, "Cursor blinks", 'b', flag("BlinkCur", false));
    let s = b.getset("Window/Appearance", "font", Some("Font settings"));
    b.push(s, Kind::Font { label: "Font used in the terminal window".into(), sc: 'n' }, Bind::Font("Font"));
    b.check(s, "Allow selection of variable-pitch fonts", NONE, Bind::VarPitch);
    b.radios(s, "Font quality:", 'q', 2, &["Antialiased", "Non-Antialiased", "ClearType", "Default"], radio("FontQuality", 0, &[1, 2, 3, 0]));
    let s = b.getset("Window/Appearance", "mouse", Some("Adjust the use of the mouse pointer"));
    b.check(s, "Hide mouse pointer when typing in window", 'p', flag("HideMousePtr", false));
    let s = b.getset("Window/Appearance", "border", Some("Adjust the window border"));
    b.edit(s, "Gap between text and window edge:", 'e', 20, int("WindowBorder", 1));
    b.check(s, "Sunken-edge border (slightly thicker)", 's', flag("SunkenEdge", false));

    // Window/Behaviour
    b.settitle("Window/Behaviour", &format!("Configure the behaviour of {APP}'s window"));
    let s = b.getset("Window/Behaviour", "title", Some("Adjust the behaviour of the window title"));
    b.edit(s, "Window title:", 't', 100, strs("WinTitle"));
    b.check(s, "Separate window and icon titles", 'i', Bind::NotFlag("WinNameAlways", true));
    let s = b.getset("Window/Behaviour", "main", None);
    b.check(s, "Warn before closing window", 'w', flag("WarnOnClose", true));
    b.check(s, "Window closes on ALT-F4", '4', flag("AltF4", true));
    b.check(s, "System menu appears on ALT-Space", 'y', flag("AltSpace", false));
    b.check(s, "System menu appears on ALT alone", 'l', flag("AltOnly", false));
    b.check(s, "Ensure window is always on top", 'e', flag("AlwaysOnTop", false));
    b.check(s, "Full screen on Alt-Enter", 'f', flag("FullScreenOnAltEnter", false));
    // RUSTuTTY's own
    b.check(s, "Warn before pasting several lines", 'p', flag("PasteWarnLines", true));
    b.check(s, "Resize the window to fit a new session", 'r', flag("FitWindow", false));

    // Window/Translation
    b.settitle("Window/Translation", "Options controlling character set translation");
    let s = b.getset("Window/Translation", "trans", Some("Character set translation"));
    b.combo(s, "Remote character set:", 'r', Bind::Codepage);
    b.text(s, "(Codepages supported by Windows but not listed here, such as CP866 on many systems, can be entered manually)");
    let s = b.getset("Window/Translation", "tweaks", None);
    b.check(s, "Treat CJK ambiguous characters as wide", 'w', flag("CJKAmbigWide", false));
    b.check(s, "Caps Lock acts as Cyrillic switch", 's', flag("CapsLockCyr", false));
    let s = b.getset("Window/Translation", "linedraw", Some(&format!("Adjust how {APP} handles line drawing characters")));
    b.radio(
        s,
        Some("Handling of line drawing characters:"),
        NONE,
        1,
        &[
            ("Use Unicode line drawing code points", 'u'),
            ("Poor man's line drawing (+, - and |)", 'p'),
            ("Font has XWindows encoding", 'x'),
            ("Use font in both ANSI and OEM modes", 'b'),
            ("Use font in OEM mode only", 'e'),
        ],
        Bind::Radio("FontVTMode", 4, &[4, 3, 0, 1, 2]),
    );
    b.check(s, "Copy and paste line drawing characters as lqqqk", 'd', flag("RawCNP", false));
    b.check(s, "Enable VT100 line drawing even in UTF-8 mode", '8', flag("UTF8linedraw", false));

    // Window/Selection
    b.settitle("Window/Selection", "Options controlling copy and paste");
    let s = b.getset("Window/Selection", "mouse", Some("Control use of mouse"));
    b.radios(
        s,
        "Action of mouse buttons:",
        'm',
        1,
        &["Windows (Middle extends, Right brings up menu)", "Compromise (Middle extends, Right pastes)", "xterm (Right extends, Middle pastes)"],
        radio("MouseIsXterm", 0, &[2, 0, 1]),
    );
    b.check(s, "Shift overrides application's use of mouse", 'p', flag("MouseOverride", true));
    b.radio(s, Some("Default selection mode (Alt+drag does the other one):"), NONE, 2, &[("Normal", 'n'), ("Rectangular block", 'r')], Bind::Radio("RectSelect", 0, &[0, 1]));
    let s = b.getset("Window/Selection", "clipboards", Some("Assign copy/paste actions to clipboards"));
    b.check(s, "Auto-copy selected text to system clipboard", NONE, flag("MouseAutocopy", true));
    b.drop(s, "Mouse paste action:", NONE, 60, Bind::Clip("MousePaste", "explicit"));
    b.drop(s, "{Ctrl,Shift} + Ins:", NONE, 60, Bind::Clip("CtrlShiftIns", "explicit"));
    b.drop(s, "Ctrl + Shift + {C,V}:", NONE, 60, Bind::Clip("CtrlShiftCV", "none"));
    let s = b.getset("Window/Selection", "paste", Some("Control pasting of text from clipboard to terminal"));
    b.check(s, "Permit control characters in pasted text", NONE, flag("PasteControls", false));

    // Window/Selection/Copy
    b.settitle("Window/Selection/Copy", "Options controlling copying from terminal to clipboard");
    let s = b.getset("Window/Selection/Copy", "charclass", Some("Classes of character that group together"));
    let kind = Kind::List { label: Some("Character classes:".into()), sc: 'e', height: 5, drag: false, multi: true, pct: 100, tabs: vec![15, 25, 20, 40] };
    b.push(s, kind, Bind::Part(Group::Class, 0));
    b.columns(s, &[67, 33]);
    b.edit(s, "Set to class", 't', 50, Bind::Part(Group::Class, 1)).at(0);
    b.button(s, "Set", 's', Bind::Part(Group::Class, 2)).at(1);
    b.columns(s, &[100]);
    let s = b.getset("Window/Selection/Copy", "format", Some("Formatting of copied characters"));
    b.check(s, "Copy to clipboard in RTF as well as plain text", 'f', flag("PasteRTF", false));

    // Window/Colours
    b.settitle("Window/Colours", "Options controlling use of colours");
    let s = b.getset("Window/Colours", "general", Some("General options for colour usage"));
    b.check(s, "Allow terminal to specify ANSI colours", 'i', flag("ANSIColour", true));
    b.check(s, "Allow terminal to use xterm 256-colour mode", '2', flag("Xterm256Colour", true));
    b.check(s, "Allow terminal to use 24-bit colours", '4', flag("TrueColour", true));
    b.radios(s, "Indicate bolded text by changing:", 'b', 3, &["The font", "The colour", "Both"], radio("BoldAsColour", 1, &[0, 1, 2]));
    b.check(s, "Attempt to use logical palettes", 'l', flag("TryPalette", false));
    b.check(s, "Use system colours", 's', flag("UseSystemColours", false));
    let s = b.getset("Window/Colours", "adjust", Some(&format!("Adjust the precise colours {APP} displays")));
    b.text(s, "Select a colour from the list, and then click the Modify button to change its appearance.");
    b.columns(s, &[67, 33]);
    b.list(s, Some("Select a colour to adjust:"), 'u', 7, &[], Bind::Part(Group::Colour, 0)).at(0);
    b.text(s, "RGB value:").at(1);
    b.edit(s, "Red", 'r', 50, Bind::Part(Group::Colour, 1)).at(1);
    b.edit(s, "Green", 'n', 50, Bind::Part(Group::Colour, 2)).at(1);
    b.edit(s, "Blue", 'e', 50, Bind::Part(Group::Colour, 3)).at(1);
    b.button(s, "Modify", 'm', Bind::Part(Group::Colour, 4)).at(1);
    b.columns(s, &[100]);

    // Connection
    b.settitle("Connection", "Options controlling the connection");
    let s = b.getset("Connection", "keepalive", Some("Sending of null packets to keep session active"));
    b.edit(s, "Seconds between keepalives (0 to turn off)", 'k', 20, Bind::Ping);
    if !mid {
        let s = b.getset("Connection", "tcp", Some("Low-level TCP connection options"));
        b.check(s, "Disable Nagle's algorithm (TCP_NODELAY option)", 'n', flag("TCPNoDelay", true));
        b.check(s, "Enable TCP keepalives (SO_KEEPALIVE option)", 'p', flag("TCPKeepalives", false));
        let s = b.getset("Connection", "ipversion", Some("Internet protocol version"));
        b.radio(s, None, NONE, 3, &[("Auto", 'u'), ("IPv4", '4'), ("IPv6", '6')], radio("AddressFamily", 0, &[0, 1, 2]));
        let s = b.getset("Connection", "identity", Some("Logical name of remote host"));
        b.edit(s, "Logical name of remote host (e.g. for SSH key lookup):", 'm', 100, strs("LogHost"));
        let s = b.getset("Connection", "hooks", Some("Command to run at connection event"));
        b.edit(s, "Command to run before connection", 'b', 100, strs("PreConnectCommand"));

        // Connection/Data
        b.settitle("Connection/Data", "Data to send to the server");
        let s = b.getset("Connection/Data", "login", Some("Login details"));
        b.edit(s, "Auto-login username", 'u', 50, strs("UserName"));
        let system = format!("Use system username ({user})");
        b.radios(s, "When username is not specified:", 'n', 4, &["Prompt", &system], Bind::Radio("UserNameFromEnvironment", 0, &[0, 1]));
        let s = b.getset("Connection/Data", "term", Some("Terminal details"));
        b.edit(s, "Terminal-type string", 't', 50, Bind::Str("TerminalType", "xterm"));
        b.edit(s, "Terminal speeds", 's', 50, Bind::Str("TerminalSpeed", "38400,38400"));
        let s = b.getset("Connection/Data", "env", Some("Environment variables"));
        b.columns(s, &[80, 20]);
        b.edit(s, "Variable", 'v', 60, Bind::Part(Group::Env, 0)).at(0);
        b.edit(s, "Value", 'l', 60, Bind::Part(Group::Env, 1)).at(0);
        b.button(s, "Add", 'd', Bind::Part(Group::Env, 2)).at(1);
        b.button(s, "Remove", 'r', Bind::Part(Group::Env, 3)).at(1);
        b.columns(s, &[100]);
        b.list(s, None, NONE, 3, &[30, 70], Bind::Part(Group::Env, 4));

        // Connection/Proxy
        b.settitle("Connection/Proxy", "Options controlling proxy usage");
        let s = b.getset("Connection/Proxy", "basics", None);
        b.drop(s, "Proxy type:", 't', 70, Bind::Drop("ProxyMethod", num("ProxyMethod", 0), PROXIES));
        b.columns(s, &[80, 20]);
        b.edit(s, "Proxy hostname", 'y', 100, Bind::Str("ProxyHost", "proxy")).at(0);
        b.edit(s, "Port", 'p', 100, int("ProxyPort", 80)).at(1);
        b.columns(s, &[100]);
        b.edit(s, "Exclude Hosts/IPs", 'e', 100, strs("ProxyExcludeList"));
        b.check(s, "Consider proxying local host connections", 'x', flag("ProxyLocalhost", false));
        b.radios(s, "Do DNS name lookup at proxy end:", 'd', 3, &["No", "Auto", "Yes"], radio("ProxyDNS", 1, &[0, 1, 2]));
        b.edit(s, "Username", 'u', 60, strs("ProxyUsername"));
        let c = b.edit(s, "Password", 'w', 60, strs("ProxyPassword"));
        if let Kind::Edit { password, .. } = &mut c.kind {
            *password = true;
        }
        b.edit(s, "Command to send to proxy (for some types)", 'm', 100, Bind::Str("ProxyTelnetCommand", "connect %host %port\\n"));
        b.radios(s, "Print proxy diagnostics in the terminal window", 'r', 5, &["No", "Yes", "Only until session starts"], radio("ProxyLogToTerm", 1, &[1, 0, 2]));
    }

    if ssh {
        // Connection/SSH
        b.settitle("Connection/SSH", "Options controlling SSH connections");
        if !mid {
            let s = b.getset("Connection/SSH", "data", Some("Data to send to the server"));
            b.edit(s, "Remote command:", 'r', 100, strs("RemoteCommand"));
            let s = b.getset("Connection/SSH", "protocol", Some("Protocol options"));
            b.check(s, "Don't start a shell or command at all", 'n', flag("SshNoShell", false));
        }
        let s = b.getset("Connection/SSH", "protocol", Some("Protocol options"));
        b.check(s, "Enable compression", 'e', flag("Compression", false));
        if !mid {
            let s = b.getset("Connection/SSH", "sharing", Some(&format!("Sharing an SSH connection between {APP} tools")));
            b.check(s, "Share SSH connections if possible", 's', flag("ConnectionSharing", false));
            b.text(s, "Permitted roles in a shared connection:");
            b.check(s, "Upstream (connecting to the real server)", 'u', flag("ConnectionSharingUpstream", true));
            b.check(s, &format!("Downstream (connecting to the upstream {APP})"), 'd', flag("ConnectionSharingDownstream", true));
            let s = b.getset("Connection/SSH", "protocol", Some("Protocol options"));
            b.radio(s, Some("SSH protocol version:"), NONE, 2, &[("2", '2'), ("1 (INSECURE)", '1')], Bind::Radio("SshProt", 3, &[3, 0]));
        }

        // Connection/SSH/Kex
        b.settitle("Connection/SSH/Kex", "Options controlling SSH key exchange");
        let s = b.getset("Connection/SSH/Kex", "main", Some("Key exchange algorithm options"));
        b.drag(s, "Algorithm selection policy:", 's', 10, Prefs::Kex);
        b.check(s, "Attempt GSSAPI key exchange", 'k', flag("AuthGSSAPIKEX", true));
        let s = b.getset("Connection/SSH/Kex", "repeat", Some("Options controlling key re-exchange"));
        b.edit(s, "Max minutes before rekey (0 for no limit)", 't', 20, int("RekeyTime", 60));
        b.edit(s, "Minutes between GSS checks (0 for never)", NONE, 20, int("GssapiRekey", 2));
        b.edit(s, "Max data before rekey (0 for no limit)", 'x', 20, Bind::Str("RekeyBytes", "1G"));
        b.text(s, "(Use 1M for 1 megabyte, 1G for 1 gigabyte etc)");

        // Connection/SSH/Host keys
        b.settitle("Connection/SSH/Host keys", "Options controlling SSH host keys");
        let s = b.getset("Connection/SSH/Host keys", "main", Some("Host key algorithm preference"));
        b.drag(s, "Algorithm selection policy:", 's', 5, Prefs::Host);
        b.check(s, "Prefer algorithms for which a host key is known", 'p', flag("PreferKnownHostKeys", true));
        if !mid {
            let s = b.getset("Connection/SSH/Host keys", "hostkeys", Some("Manually configure host keys for this connection"));
            b.columns(s, &[75, 25]);
            b.text(s, "Host keys or fingerprints to accept:").at(0);
            let c = b.button(s, "Remove", 'r', Bind::Part(Group::Hk, 0)).at(1);
            c.delay = true;
            let rem = b.last(s);
            b.list(s, None, NONE, 2, &[], Bind::Part(Group::Hk, 1));
            b.push(s, Kind::TabDelay(rem), Bind::None);
            b.edit(s, "Key", 'k', 80, Bind::Part(Group::Hk, 2)).at(0);
            b.button(s, "Add key", 'y', Bind::Part(Group::Hk, 3)).at(1);
            b.columns(s, &[100]);
        }
        let s = b.getset("Connection/SSH/Host keys", "ca", Some("Configure trusted certification authorities"));
        b.button(s, "Configure host CAs", NONE, Bind::HostCas);

        // Connection/SSH/Cipher
        b.settitle("Connection/SSH/Cipher", "Options controlling SSH encryption");
        let s = b.getset("Connection/SSH/Cipher", "encryption", Some("Encryption options"));
        b.drag(s, "Encryption cipher selection policy:", 's', 6, Prefs::Cipher);
        b.check(s, "Enable legacy use of single-DES in SSH-2", 'i', flag("SSH2DES", false));

        if !mid {
            // Connection/SSH/Auth
            b.settitle("Connection/SSH/Auth", "Options controlling SSH authentication");
            let s = b.getset("Connection/SSH/Auth", "main", None);
            b.check(s, "Display pre-authentication banner (SSH-2 only)", 'd', flag("SshBanner", true));
            b.check(s, "Bypass authentication entirely (SSH-2 only)", 'b', flag("SshNoAuth", false));
            b.check(s, "Disconnect if authentication succeeds trivially", 'n', flag("SshNoTrivialAuth", false));
            let s = b.getset("Connection/SSH/Auth", "methods", Some("Authentication methods"));
            b.check(s, "Attempt authentication using Pageant", 'p', flag("TryAgent", true));
            b.check(s, "Attempt TIS or CryptoCard auth (SSH-1)", 'm', flag("AuthTIS", false));
            b.check(s, "Attempt \"keyboard-interactive\" auth (SSH-2)", 'i', flag("AuthKI", true));
            let s = b.getset("Connection/SSH/Auth", "aux", Some("Other authentication-related options"));
            b.check(s, "Allow agent forwarding", 'f', flag("AgentFwd", false));
            b.check(s, "Allow attempted changes of username in SSH-2", NONE, flag("ChangeUsername", false));

            b.settitle("Connection/SSH/Auth/Credentials", "Credentials to authenticate with");
            let s = b.getset("Connection/SSH/Auth/Credentials", "publickey", Some("Public-key authentication"));
            b.file(s, "Private key file for authentication:", 'k', KEY_FILES, false, "Select private key file", "PublicKeyFile");
            b.file(s, "Certificate to use with the private key (optional):", 'e', ALL_FILES, false, "Select certificate file", "DetachedCertificate");
            let s = b.getset("Connection/SSH/Auth/Credentials", "plugin", Some("Plugin to provide authentication responses"));
            b.edit(s, "Plugin command to run", NONE, 100, strs("AuthPlugin"));

            b.settitle("Connection/SSH/Auth/GSSAPI", "Options controlling GSSAPI authentication");
            let s = b.getset("Connection/SSH/Auth/GSSAPI", "gssapi", None);
            b.check(s, "Attempt GSSAPI authentication (SSH-2 only)", 't', flag("AuthGSSAPI", true));
            b.check(s, "Attempt GSSAPI key exchange (SSH-2 only)", 'k', flag("AuthGSSAPIKEX", true));
            b.check(s, "Allow GSSAPI credential delegation", 'l', flag("GssapiFwd", false));
            b.drag(s, "Preference order for GSSAPI libraries:", 'p', 3, Prefs::Gss);
            b.file(s, "User-supplied GSSAPI library path:", 's', DYNLIB_FILES, false, "Select library file", "GSSCustom");

            // Connection/SSH/TTY
            b.settitle("Connection/SSH/TTY", "Remote terminal settings");
            let s = b.getset("Connection/SSH/TTY", "sshtty", None);
            b.check(s, "Don't allocate a pseudo-terminal", 'p', flag("NoPTY", false));
            let s = b.getset("Connection/SSH/TTY", "ttymodes", Some("Terminal modes"));
            b.text(s, "Terminal modes to send:");
            b.list(s, None, NONE, 8, &[40, 60], Bind::Part(Group::Tty, 0));
            b.columns(s, &[75, 25]);
            b.text(s, "For selected mode, send:").at(0);
            let c = b.button(s, "Set", 's', Bind::Part(Group::Tty, 1)).at(1);
            c.delay = true;
            let set = b.last(s);
            b.columns(s, &[100]);
            b.columns(s, &[75, 25]);
            b.radio(s, None, NONE, 3, &[("Auto", NONE), ("Nothing", NONE), ("This:", NONE)], Bind::Part(Group::Tty, 2)).at(0);
            let radio_at = b.last(s);
            let kind = Kind::Edit { label: None, sc: NONE, pct: 100, combo: false, password: false };
            let c = b.push(s, kind, Bind::Part(Group::Tty, 3)).at(1);
            c.align = Some(radio_at);
            b.push(s, Kind::TabDelay(set), Bind::None);

            // Connection/SSH/X11
            b.settitle("Connection/SSH/X11", "Options controlling SSH X11 forwarding");
            let s = b.getset("Connection/SSH/X11", "x11", Some("X11 forwarding"));
            b.check(s, "Enable X11 forwarding", 'e', flag("X11Forward", false));
            b.edit(s, "X display location", 'x', 50, strs("X11Display"));
            b.radios(s, "Remote X11 authentication protocol", 'u', 2, &["MIT-Magic-Cookie-1", "XDM-Authorization-1"], radio("X11AuthType", 1, &[1, 2]));
            b.file(s, "X authority file for local display", 't', ALL_FILES, false, "Select X authority file", "X11AuthFile");
        }

        // Connection/SSH/Tunnels
        b.settitle("Connection/SSH/Tunnels", "Options controlling SSH port forwarding");
        let s = b.getset("Connection/SSH/Tunnels", "portfwd", Some("Port forwarding"));
        b.check(s, "Local ports accept connections from other hosts", 't', flag("LocalPortAcceptAll", false));
        b.check(s, "Remote ports do the same (SSH-2 only)", 'p', flag("RemotePortAcceptAll", false));
        b.columns(s, &[55, 20, 25]);
        b.text(s, "Forwarded ports:").col = (0, 2);
        let c = b.button(s, "Remove", 'r', Bind::Part(Group::Fwd, 0)).at(2);
        c.delay = true;
        let rem = b.last(s);
        b.list(s, None, NONE, 3, &[20, 80], Bind::Part(Group::Fwd, 1));
        b.push(s, Kind::TabDelay(rem), Bind::None);
        b.text(s, "Add new forwarded port:");
        let c = b.button(s, "Add", 'd', Bind::Part(Group::Fwd, 2)).at(2);
        c.delay = true;
        let add = b.last(s);
        b.edit(s, "Source port", 's', 40, Bind::Part(Group::Fwd, 3)).at(0);
        b.edit(s, "Destination", 'i', 67, Bind::Part(Group::Fwd, 4));
        b.radio(s, None, NONE, 3, &[("Local", 'l'), ("Remote", 'm'), ("Dynamic", 'y')], Bind::Part(Group::Fwd, 5));
        b.radio(s, None, NONE, 3, &[("Auto", 'u'), ("IPv4", '4'), ("IPv6", '6')], Bind::Part(Group::Fwd, 6));
        b.push(s, Kind::TabDelay(add), Bind::None);
        b.columns(s, &[100]);

        if !mid {
            // Connection/SSH/Bugs
            b.settitle("Connection/SSH/Bugs", "Workarounds for SSH server bugs");
            let s = b.getset("Connection/SSH/Bugs", "main", Some("Detection of known bugs in SSH servers"));
            b.bug(s, "Chokes on SSH-2 ignore messages", '2', "BugIgnore2");
            b.bug(s, "Handles SSH-2 key re-exchange badly", 'k', "BugRekey2");
            b.bug(s, "Chokes on PuTTY's SSH-2 'winadj' requests", 'j', "BugWinadj");
            b.bug(s, "Replies to requests on closed channels", 'q', "BugChanReq");
            b.bug(s, "Ignores SSH-2 maximum packet size", 'x', "BugMaxPkt2");
            let s = b.getset("Connection/SSH/Bugs", "manual", Some("Manually enabled workarounds"));
            b.manual_bug(s, "Discards data sent before its greeting", 'd', "BugDropStart");
            b.manual_bug(s, "Chokes on PuTTY's full KEXINIT", 'p', "BugFilterKexinit");

            b.settitle("Connection/SSH/More bugs", "Further workarounds for SSH server bugs");
            let s = b.getset("Connection/SSH/More bugs", "main", Some("Detection of known bugs in SSH servers"));
            b.bug(s, "Old RSA/SHA2 cert algorithm naming", 'l', "BugRSASHA2CertUserauth");
            b.bug(s, "Requires padding on SSH-2 RSA signatures", 'p', "BugRSAPad2");
            b.bug(s, "Only supports pre-RFC4419 SSH-2 DH GEX", 'd', "BugOldGex2");
            b.bug(s, "Miscomputes SSH-2 HMAC keys", 'm', "BugHMAC2");
            b.bug(s, "Misuses the session ID in SSH-2 PK auth", 'n', "BugPKSessID2");
            b.bug(s, "Miscomputes SSH-2 encryption keys", 'e', "BugDeriveKey2");
            b.bug(s, "Chokes on SSH-1 ignore messages", 'i', "BugIgnore1");
            b.bug(s, "Refuses all SSH-1 password camouflage", 's', "BugPlainPW1");
            b.bug(s, "Chokes on SSH-1 RSA authentication", 'r', "BugRSA1");
        }
    }

    if is("serial") {
        // Connection/Serial
        b.settitle("Connection/Serial", "Options controlling local serial lines");
        if !mid {
            let s = b.getset("Connection/Serial", "serline", Some("Select a serial line"));
            b.edit(s, "Serial line to connect to", 'l', 40, strs("SerialLine"));
        }
        let s = b.getset("Connection/Serial", "sercfg", Some("Configure the serial line"));
        b.edit(s, "Speed (baud)", 's', 40, int("SerialSpeed", 9600));
        b.edit(s, "Data bits", 'b', 40, int("SerialDataBits", 8));
        b.edit(s, "Stop bits", 't', 40, Bind::Fixed("SerialStopHalfbits", num("SerialStopHalfbits", 2), 2));
        b.drop(s, "Parity", 'p', 40, Bind::Drop("SerialParity", 0, PARITIES));
        b.drop(s, "Flow control", 'f', 40, Bind::Drop("SerialFlowControl", 1, FLOWS));
    }

    if is("telnet") {
        // Connection/Telnet
        b.settitle("Connection/Telnet", "Options controlling Telnet connections");
        let s = b.getset("Connection/Telnet", "protocol", Some("Telnet protocol adjustments"));
        if !mid {
            b.radio(s, Some("Handling of OLD_ENVIRON ambiguity:"), NONE, 2, &[("BSD (commonplace)", 'b'), ("RFC 1408 (unusual)", 'f')], Bind::Radio("RFCEnviron", 0, &[0, 1]));
            b.radios(s, "Telnet negotiation mode:", 't', 2, &["Passive", "Active"], Bind::Radio("PassiveTelnet", 0, &[1, 0]));
        }
        b.check(s, "Keyboard sends Telnet special commands", 'k', flag("TelnetKey", false));
        b.check(s, "Return key sends Telnet New Line instead of ^M", 'm', flag("TelnetRet", true));
    }

    if !mid {
        // Connection/Rlogin
        b.settitle("Connection/Rlogin", "Options controlling Rlogin connections");
        let s = b.getset("Connection/Rlogin", "data", Some("Data to send to the server"));
        b.edit(s, "Local username:", 'l', 50, strs("LocalUserName"));

        // Connection/SUPDUP
        b.settitle("Connection/SUPDUP", "Options controlling SUPDUP connections");
        let s = b.getset("Connection/SUPDUP", "main", None);
        b.edit(s, "Location string", 'l', 70, Bind::Str("SUPDUPLocation", "The Internet"));
        b.radios(s, "Extended ASCII Character set:", 'e', 4, &["None", "ITS", "WAITS"], radio("SUPDUPCharset", 0, &[0, 1, 2]));
        b.check(s, "**MORE** processing", 'm', flag("SUPDUPMoreProcessing", false));
        b.check(s, "Terminal scrolling", 's', flag("SUPDUPScrolling", false));
    }
    cb
}

/// One entry of a preference list: its saved name, label, and where it goes
/// if a session doesn't mention it (`keyvalwhere` in settings.c).
pub struct Pref(pub &'static str, pub &'static str, pub Option<&'static str>, pub i32);

pub const CIPHERS: &[Pref] = &[
    Pref("aes", "AES (SSH-2 only)", None, -1),
    Pref("chacha20", "ChaCha20 (SSH-2 only)", Some("aes"), 1),
    Pref("aesgcm", "AES-GCM (SSH-2 only)", Some("chacha20"), 1),
    Pref("3des", "3DES", None, -1),
    Pref("WARN", "-- warn below here --", None, -1),
    Pref("des", "DES", None, -1),
    Pref("blowfish", "Blowfish", None, -1),
    Pref("arcfour", "Arcfour (SSH-2 only)", None, -1),
];

pub const KEXES: &[Pref] = &[
    Pref("ntru-curve25519", "NTRU Prime / Curve25519 hybrid kex", None, 1),
    Pref("mlkem-curve25519", "ML-KEM / Curve25519 hybrid kex", Some("ntru-curve25519"), 1),
    Pref("mlkem-nist", "ML-KEM / NIST ECDH hybrid kex", Some("mlkem-curve25519"), 1),
    Pref("ecdh", "ECDH key exchange", None, 1),
    Pref("dh-gex-sha1", "Diffie-Hellman group exchange", None, -1),
    Pref("dh-group14-sha1", "Diffie-Hellman group 14 (2048-bit)", None, -1),
    Pref("dh-group1-sha1", "Diffie-Hellman group 1 (1024-bit)", Some("WARN"), 1),
    Pref("rsa", "RSA-based key exchange", Some("WARN"), -1),
    Pref("dh-group15-sha512", "Diffie-Hellman group 15 (3072-bit)", Some("dh-group14-sha1"), -1),
    Pref("dh-group16-sha512", "Diffie-Hellman group 16 (4096-bit)", Some("dh-group15-sha512"), -1),
    Pref("dh-group17-sha512", "Diffie-Hellman group 17 (6144-bit)", Some("dh-group16-sha512"), 1),
    Pref("dh-group18-sha512", "Diffie-Hellman group 18 (8192-bit)", Some("dh-group17-sha512"), 1),
    Pref("WARN", "-- warn below here --", None, -1),
];

pub const HOST_KEYS: &[Pref] = &[
    Pref("ed25519", "Ed25519", None, 1),
    Pref("ed448", "Ed448", None, 1),
    Pref("ecdsa", "ECDSA", None, -1),
    Pref("dsa", "DSA", None, -1),
    Pref("rsa", "RSA", None, -1),
    Pref("WARN", "-- warn below here --", None, -1),
];

pub const GSS_LIBS: &[Pref] = &[
    Pref("gssapi32", "MIT Kerberos GSSAPI64.DLL", None, -1),
    Pref("sspi", "Microsoft SSPI SECUR32.DLL", None, -1),
    Pref("custom", "User-specified GSSAPI DLL", None, -1),
];

/// What the deck uses for an unset list, so the box shows what really happens
/// (algorithms.rs), and PuTTY's own defaults elsewhere.
const DEFAULT_CIPHERS: &str = "aes,chacha20,aesgcm,3des,WARN";
const DEFAULT_KEXES: &str = "mlkem-curve25519,ecdh,dh-gex-sha1,dh-group18-sha512,dh-group17-sha512,dh-group16-sha512,dh-group15-sha512,dh-group14-sha1,WARN";
const DEFAULT_HOST_KEYS: &str = "ed25519,ecdsa,rsa,WARN";

impl Prefs {
    pub fn info(self) -> (&'static str, &'static str, &'static [Pref]) {
        match self {
            Prefs::Cipher => ("Cipher", DEFAULT_CIPHERS, CIPHERS),
            Prefs::Kex => ("KEX", DEFAULT_KEXES, KEXES),
            Prefs::Host => ("HostKey", DEFAULT_HOST_KEYS, HOST_KEYS),
            Prefs::Gss => ("GSSLibs", "", GSS_LIBS),
        }
    }
}

/// `gprefs_from_str`: the order from a saved list, with anything it leaves out
/// put where the table says.
pub fn prefs_order(text: &str, table: &'static [Pref]) -> Vec<usize> {
    let at = |name: &str| table.iter().position(|p| p.0 == name);
    let mut out: Vec<usize> = Vec::new();
    for word in text.split(',') {
        if let Some(i) = at(word) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    while out.len() < table.len() {
        for (i, p) in table.iter().enumerate() {
            if out.contains(&i) {
                continue;
            }
            let pos = match p.2 {
                None if p.3 < 0 => out.len(),
                None => 0,
                Some(rel) => match out.iter().position(|&j| Some(j) == at(rel)) {
                    Some(j) if p.3 < 0 => j,
                    Some(j) => j + 1,
                    None => continue,
                },
            };
            out.insert(pos, i);
        }
    }
    out
}

pub const TTY_MODES: &[&str] = &[
    "INTR", "QUIT", "ERASE", "KILL", "EOF", "EOL", "EOL2", "START", "STOP", "SUSP", "DSUSP", "REPRINT", "WERASE", "LNEXT", "FLUSH", "SWTCH", "STATUS",
    "DISCARD", "IGNPAR", "PARMRK", "INPCK", "ISTRIP", "INLCR", "IGNCR", "ICRNL", "IUCLC", "IXON", "IXANY", "IXOFF", "IMAXBEL", "IUTF8", "ISIG", "ICANON",
    "XCASE", "ECHO", "ECHOE", "ECHOK", "ECHONL", "NOFLSH", "TOSTOP", "IEXTEN", "ECHOCTL", "ECHOKE", "PENDIN", "OPOST", "OLCUC", "ONLCR", "OCRNL", "ONOCR",
    "ONLRET", "CS7", "CS8", "PARENB", "PARODD",
];

pub const COLOURS: &[(&str, &str)] = &[
    ("Default Foreground", "187,187,187"),
    ("Default Bold Foreground", "255,255,255"),
    ("Default Background", "0,0,0"),
    ("Default Bold Background", "85,85,85"),
    ("Cursor Text", "0,0,0"),
    ("Cursor Colour", "0,255,0"),
    ("ANSI Black", "0,0,0"),
    ("ANSI Black Bold", "85,85,85"),
    ("ANSI Red", "187,0,0"),
    ("ANSI Red Bold", "255,85,85"),
    ("ANSI Green", "0,187,0"),
    ("ANSI Green Bold", "85,255,85"),
    ("ANSI Yellow", "187,187,0"),
    ("ANSI Yellow Bold", "255,255,85"),
    ("ANSI Blue", "0,0,187"),
    ("ANSI Blue Bold", "85,85,255"),
    ("ANSI Magenta", "187,0,187"),
    ("ANSI Magenta Bold", "255,85,255"),
    ("ANSI Cyan", "0,187,187"),
    ("ANSI Cyan Bold", "85,255,255"),
    ("ANSI White", "187,187,187"),
    ("ANSI White Bold", "255,255,255"),
];

/// The first four `Wordness` keys cover ASCII, all the list shows.
pub const WORDNESS: [&str; 4] = [
    "0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0",
    "0,1,2,1,1,1,1,1,1,1,1,1,1,2,2,2,2,2,2,2,2,2,2,2,2,2,1,1,1,1,1,1",
    "1,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,1,1,1,1,2",
    "1,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,1,1,1,1,1",
];

pub const CODEPAGES: &[(&str, u32)] = &[
    ("UTF-8", 0),
    ("ISO-8859-1:1998 (Latin-1, West Europe)", 0),
    ("ISO-8859-2:1999 (Latin-2, East Europe)", 0),
    ("ISO-8859-3:1999 (Latin-3, South Europe)", 0),
    ("ISO-8859-4:1998 (Latin-4, North Europe)", 0),
    ("ISO-8859-5:1999 (Latin/Cyrillic)", 0),
    ("ISO-8859-6:1999 (Latin/Arabic)", 0),
    ("ISO-8859-7:1987 (Latin/Greek)", 0),
    ("ISO-8859-8:1999 (Latin/Hebrew)", 0),
    ("ISO-8859-9:1999 (Latin-5, Turkish)", 0),
    ("ISO-8859-10:1998 (Latin-6, Nordic)", 0),
    ("ISO-8859-11:2001 (Latin/Thai)", 0),
    ("ISO-8859-13:1998 (Latin-7, Baltic)", 0),
    ("ISO-8859-14:1998 (Latin-8, Celtic)", 0),
    ("ISO-8859-15:1999 (Latin-9, \"euro\")", 0),
    ("ISO-8859-16:2001 (Latin-10, Balkan)", 0),
    ("KOI8-U", 0),
    ("KOI8-R", 20866),
    ("HP-ROMAN8", 0),
    ("VSCII", 0),
    ("DEC-MCS", 0),
    ("Win1250 (Central European)", 1250),
    ("Win1251 (Cyrillic)", 1251),
    ("Win1252 (Western)", 1252),
    ("Win1253 (Greek)", 1253),
    ("Win1254 (Turkish)", 1254),
    ("Win1255 (Hebrew)", 1255),
    ("Win1256 (Arabic)", 1256),
    ("Win1257 (Baltic)", 1257),
    ("Win1258 (Vietnamese)", 1258),
    ("CP437", 437),
    ("CP620 (Mazovia)", 0),
    ("CP819", 28591),
    ("CP852", 852),
    ("CP878", 20866),
    ("Use font encoding", 0),
];

/// `cp_name(decode_codepage(text))`: a typed name becomes the list's spelling,
/// and CPnnn or IBMnnn stays a number.
pub fn codepage_name(text: &str) -> String {
    if text.is_empty() {
        return "UTF-8".into();
    }
    let plain = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ':').map(|c| c.to_ascii_lowercase()).collect::<String>();
    let wanted = plain(text);
    if let Some((name, _)) = CODEPAGES.iter().find(|(n, _)| plain(n).starts_with(&wanted)) {
        return name.to_string();
    }
    let low = text.to_ascii_lowercase();
    let digits = low.strip_prefix("cp").unwrap_or(&low);
    let digits = digits.strip_prefix("ibm").unwrap_or(digits);
    match digits.parse::<u32>() {
        Ok(n) if n > 0 && n < 65536 => match CODEPAGES.iter().find(|(_, c)| *c == n) {
            Some((name, _)) => name.to_string(),
            None => format!("CP{n:03}"),
        },
        _ => String::new(),
    }
}

pub struct Protocol {
    pub id: &'static str,
    pub name: &'static str,
    pub port: u32,
}

/// PuTTY's `backends`, the first two having radio buttons.
pub const PROTOCOLS: &[Protocol] = &[
    Protocol { id: "ssh", name: "SSH", port: 22 },
    Protocol { id: "serial", name: "Serial", port: 0 },
    Protocol { id: "telnet", name: "Telnet", port: 23 },
    Protocol { id: "rlogin", name: "Rlogin", port: 513 },
    Protocol { id: "supdup", name: "SUPDUP", port: 95 },
    Protocol { id: "raw", name: "Raw", port: 0 },
    Protocol { id: "ssh-connection", name: "Bare ssh-connection", port: 0 },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const PUTTY: &str = include_str!("../../../deck-core/tests/putty_keywords.txt");
    const PANELS: &[&str] = &[
        "Session", "Session/Logging", "Terminal", "Terminal/Keyboard", "Terminal/Bell", "Terminal/Features", "Window", "Window/Appearance",
        "Window/Behaviour", "Window/Translation", "Window/Selection", "Window/Selection/Copy", "Window/Colours", "Connection", "Connection/Data",
        "Connection/Proxy", "Connection/SSH", "Connection/SSH/Kex", "Connection/SSH/Host keys", "Connection/SSH/Cipher", "Connection/SSH/Auth",
        "Connection/SSH/Auth/Credentials", "Connection/SSH/Auth/GSSAPI", "Connection/SSH/TTY", "Connection/SSH/X11", "Connection/SSH/Tunnels",
        "Connection/SSH/Bugs", "Connection/SSH/More bugs", "Connection/Serial", "Connection/Telnet", "Connection/Rlogin", "Connection/SUPDUP",
    ];

    fn paths(b: &Cbox) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in b.sets.iter().filter(|s| !s.path.is_empty()) {
            if out.last() != Some(&s.path.as_str()) {
                out.push(&s.path);
            }
        }
        out
    }

    fn keys(c: &Ctl) -> Vec<&'static str> {
        match &c.bind {
            Bind::Str(k, _) | Bind::Int(k, _) | Bind::Fixed(k, ..) | Bind::Flag(k, _) | Bind::NotFlag(k, _) | Bind::Radio(k, ..) | Bind::Drop(k, ..) | Bind::Clip(k, _) | Bind::Font(k) => vec![k],
            Bind::Prefs(p) => vec![p.info().0],
            Bind::Host => vec!["HostName", "SerialLine"],
            Bind::Port => vec!["PortNumber", "SerialSpeed"],
            Bind::Ping => vec!["PingInterval", "PingIntervalSecs"],
            Bind::Keypad => vec!["ApplicationKeypad", "NetHackKeypad"],
            Bind::Printer => vec!["Printer"],
            Bind::Codepage => vec!["LineCodePage"],
            Bind::Part(Group::Proto, _) => vec!["Protocol"],
            Bind::Part(Group::Tty, _) => vec!["TerminalModes"],
            Bind::Part(Group::Env, _) => vec!["Environment"],
            Bind::Part(Group::Fwd, _) => vec!["PortForwardings"],
            Bind::Part(Group::Hk, _) => vec!["SSHManualHostKeys"],
            _ => vec![],
        }
    }

    #[test]
    fn every_panel_of_config_c_is_there_in_order() {
        assert_eq!(paths(&build(false, "ssh", "u")), PANELS);
    }

    #[test]
    fn every_panel_has_a_title_first() {
        let b = build(false, "ssh", "u");
        for p in PANELS {
            let first = b.sets.iter().find(|s| s.path == *p).unwrap();
            assert!(first.name.is_none() && first.title.is_some(), "{p}");
        }
    }

    #[test]
    fn every_key_is_putty_s() {
        // settings.c builds or stores these itself, so the list doesn't have them
        let extra = ["FontHeight", "FontIsBold", "FontCharSet", "PasteWarnLines", "FitWindow"];
        let known: HashSet<&str> = PUTTY.split_whitespace().chain(extra).collect();
        let b = build(false, "ssh", "u");
        for k in b.sets.iter().flat_map(|s| &s.ctrls).flat_map(keys) {
            assert!(known.contains(k), "{k}");
        }
    }

    #[test]
    fn every_saved_keyword_the_dialog_should_edit_has_a_control() {
        let b = build(false, "ssh", "u");
        let used: HashSet<&str> = b.sets.iter().flat_map(|s| &s.ctrls).flat_map(keys).collect();
        // The ones with no control in PuTTY's Windows dialog either
        let no_control = [
            "BoldFont", "WideFont", "WideBoldFont", "ShadowBold", "ShadowBoldOffset", "ScrollbarOnLeft", "UTF8Override", "OSXCommandMeta", "OSXOptionMeta",
            "LoginShell", "StampUtmp", "WindowClass", "Present",
        ];
        for k in PUTTY.split_whitespace() {
            assert!(used.contains(k) || no_control.contains(&k), "{k} has no control");
        }
    }

    #[test]
    fn radios_and_lists_match_their_values() {
        let b = build(false, "ssh", "u");
        for c in b.sets.iter().flat_map(|s| &s.ctrls) {
            if let (Kind::Radio { buttons, .. }, Bind::Radio(k, d, values)) = (&c.kind, &c.bind) {
                assert_eq!(buttons.len(), values.len(), "{k}");
                assert!(values.contains(d), "{k}");
            }
        }
    }

    #[test]
    fn mid_session_keeps_what_config_c_keeps() {
        let b = build(true, "ssh", "u");
        let want = [
            "Session", "Session/Logging", "Terminal", "Terminal/Keyboard", "Terminal/Bell", "Terminal/Features", "Window", "Window/Appearance",
            "Window/Behaviour", "Window/Translation", "Window/Selection", "Window/Selection/Copy", "Window/Colours", "Connection", "Connection/SSH",
            "Connection/SSH/Kex", "Connection/SSH/Host keys", "Connection/SSH/Cipher", "Connection/SSH/Tunnels",
        ];
        assert_eq!(paths(&b), want);
        let all: Vec<_> = b.sets.iter().flat_map(|s| &s.ctrls).collect();
        assert!(!all.iter().any(|c| c.bind == Bind::Host || c.bind == Bind::About));
        assert!(!all.iter().any(|c| c.bind == Bind::Part(Group::Saved, 2) || c.bind == Bind::Part(Group::Saved, 4)));
        assert!(all.iter().any(|c| matches!(&c.kind, Kind::Button { label, .. } if label == "Apply")));
        assert!(b.sets.iter().any(|s| s.title.as_deref() == Some("Save the current session settings")));

        assert!(paths(&build(true, "telnet", "u")).contains(&"Connection/Telnet"));
        assert!(!paths(&build(true, "telnet", "u")).contains(&"Connection/SSH"));
        assert!(paths(&build(true, "serial", "u")).contains(&"Connection/Serial"));
        let supdup = build(true, "supdup", "u");
        assert!(!supdup.sets.iter().any(|s| s.name.as_deref() == Some("size")));
    }

    #[test]
    fn prefs_fill_in_like_gprefs() {
        let names = |t: &str, table: &'static [Pref]| prefs_order(t, table).iter().map(|&i| table[i].0).collect::<Vec<_>>();
        assert_eq!(names("", CIPHERS), ["aes", "chacha20", "aesgcm", "3des", "WARN", "des", "blowfish", "arcfour"]);
        assert_eq!(names("aes,chacha20,aes,bogus", CIPHERS)[..3], ["aes", "chacha20", "aesgcm"]);
        assert_eq!(names("", GSS_LIBS), ["gssapi32", "sspi", "custom"]);
        assert_eq!(prefs_order("ed25519,ecdsa,rsa,dsa,WARN", HOST_KEYS).len(), HOST_KEYS.len());
        assert_eq!(names("WARN", KEXES).len(), KEXES.len());
    }

    #[test]
    fn codepages_normalise() {
        assert_eq!(codepage_name(""), "UTF-8");
        assert_eq!(codepage_name("utf8"), "UTF-8");
        assert_eq!(codepage_name("cp866"), "CP866");
        assert_eq!(codepage_name("ibm437"), "CP437");
        assert_eq!(codepage_name("iso-8859-1"), "ISO-8859-1:1998 (Latin-1, West Europe)");
    }
}
