//! The session settings the deck can edit, and an account of the rest. Every
//! keyword PuTTY saves is in exactly one of `OPTIONS`, `AUTOMATIC` or `NOT_DONE`,
//! which a test checks against PuTTY's own list, so nothing is silently dropped.
//! A setting that isn't done is still loaded and saved unchanged.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Text,
    Number(u32),
    Flag(bool),
    Choice(u32, &'static [(&'static str, u32)]),
}

#[derive(Clone, Copy, Debug)]
pub struct Option_ {
    pub category: &'static str,
    pub label: &'static str,
    pub key: &'static str,
    pub kind: Kind,
}

const fn o(category: &'static str, label: &'static str, key: &'static str, kind: Kind) -> Option_ {
    Option_ { category, label, key, kind }
}

const PROXIES: &[(&str, u32)] = &[
    ("None", 0), ("SOCKS 4", 1), ("SOCKS 5", 2), ("HTTP", 3), ("Telnet", 4), ("Local command", 5),
    ("SSH: port forward", 6), ("SSH: command", 7), ("SSH: subsystem", 8),
];
const CLOSE: &[(&str, u32)] = &[("Never", 0), ("Only on clean exit", 1), ("Always", 2)];
const LOG: &[(&str, u32)] = &[("None", 0), ("Printable output", 1), ("All session output", 2)];
const CLASH: &[(&str, u32)] = &[("Append", 0), ("Overwrite", 1)];
const PARITY: &[(&str, u32)] = &[("None", 0), ("Odd", 1), ("Even", 2)];
const SUPDUP: &[(&str, u32)] = &[("ASCII", 0), ("ITS", 1), ("WAITS", 2)];
const TRISTATE: &[(&str, u32)] = &[("Auto", 2), ("On", 0), ("Off", 1)];
const TITLE_QUERY: &[(&str, u32)] = &[("None", 0), ("Empty string", 1), ("Window title", 2)];
const FUNCTION_KEYS: &[(&str, u32)] = &[("ESC[n~", 0), ("Linux", 1), ("Xterm R6", 2), ("VT400", 3), ("VT100+", 4), ("SCO", 5), ("Xterm 216+", 6)];
const ARROWS: &[(&str, u32)] = &[("Ctrl toggles application mode", 0), ("xterm-style bitmap", 1)];
const MOUSE: &[(&str, u32)] = &[("Right pastes, middle extends", 0), ("Middle pastes, right extends", 1), ("Right opens a menu", 2)];
const BEEP: &[(&str, u32)] = &[("None", 0), ("Windows default", 1), ("Flash the window", 2), ("Sound file", 3), ("PC speaker", 4)];
const BEEP_IND: &[(&str, u32)] = &[("Nothing", 0), ("Flash", 1), ("Light up", 2)];
const QUALITY: &[(&str, u32)] = &[("Default", 0), ("Antialiased", 1), ("Not antialiased", 2), ("ClearType", 3)];
const RESIZE: &[(&str, u32)] = &[("Follows the window", 0), ("Stays fixed", 1)];
const CURSOR: &[(&str, u32)] = &[("Block", 0), ("Underline", 1), ("Vertical line", 2)];
const BOLD: &[(&str, u32)] = &[("Bold font", 0), ("Brighter colour", 1), ("Both", 2)];
const FAMILY: &[(&str, u32)] = &[("Auto", 0), ("IPv4", 1), ("IPv6", 2)];
const DNS: &[(&str, u32)] = &[("No", 0), ("Auto", 1), ("Yes", 2)];
const X11_AUTH: &[(&str, u32)] = &[("MIT-MAGIC-COOKIE-1", 1), ("XDM-AUTHORIZATION-1", 2)];
const FLOW: &[(&str, u32)] = &[("None", 0), ("XON/XOFF", 1), ("RTS/CTS", 2)];

pub const CATEGORIES: &[&str] = &["Session", "Logging", "Terminal", "Keyboard", "Mouse", "Bell", "Window", "Colours", "Connection", "Proxy", "SSH", "Auth", "Forwarding", "Serial"];

pub const OPTIONS: &[Option_] = &[
    o("Session", "Host name", "HostName", Kind::Text),
    o("Session", "Port", "PortNumber", Kind::Number(22)),
    o("Session", "Protocol (ssh, telnet, rlogin, raw, supdup, serial)", "Protocol", Kind::Text),
    o("Session", "Close window on exit", "CloseOnExit", Kind::Choice(1, CLOSE)),
    o("Session", "Warn before closing", "WarnOnClose", Kind::Flag(true)),
    o("Session", "Warn before pasting several lines", "PasteWarnLines", Kind::Flag(true)),
    o("Logging", "What to log", "LogType", Kind::Choice(0, LOG)),
    o("Logging", "Log file (&H host, &P port, &Y&M&D date, &T time)", "LogFileName", Kind::Text),
    o("Logging", "If the file exists", "LogFileClash", Kind::Choice(0, CLASH)),
    o("Logging", "Flush after every write", "LogFlush", Kind::Flag(true)),
    o("Logging", "Write a header line", "LogHeader", Kind::Flag(true)),
    o("Terminal", "Scrollback lines", "ScrollbackLines", Kind::Number(10000)),
    o("Terminal", "Terminal type", "TerminalType", Kind::Text),
    o("Terminal", "Terminal speed", "TerminalSpeed", Kind::Text),
    o("Terminal", "Character set of the line", "LineCodePage", Kind::Text),
    o("Terminal", "Auto wrap", "AutoWrapMode", Kind::Flag(true)),
    o("Terminal", "Origin mode at start", "DECOriginMode", Kind::Flag(false)),
    o("Terminal", "Each LF also does a CR", "LFImpliesCR", Kind::Flag(false)),
    o("Terminal", "Each CR also does a LF", "CRImpliesLF", Kind::Flag(false)),
    o("Terminal", "Local echo", "LocalEcho", Kind::Choice(2, TRISTATE)),
    o("Terminal", "Local line editing", "LocalEdit", Kind::Choice(2, TRISTATE)),
    o("Terminal", "Answerback to ^E", "Answerback", Kind::Text),
    o("Terminal", "Reply to a title query", "RemoteQTitleAction", Kind::Choice(1, TITLE_QUERY)),
    o("Terminal", "Clearing the screen keeps it in scrollback", "EraseToScrollback", Kind::Flag(true)),
    o("Terminal", "Ignore requests to clear scrollback", "NoRemoteClearScroll", Kind::Flag(false)),
    o("Terminal", "Ignore the alternate screen", "NoAltScreen", Kind::Flag(false)),
    o("Terminal", "Ignore character set changes", "NoRemoteCharset", Kind::Flag(false)),
    o("Terminal", "Backspace from the host doesn't erase", "NoDBackspace", Kind::Flag(false)),
    o("Terminal", "Paste control characters", "PasteControls", Kind::Flag(false)),
    o("Terminal", "Printer for host print jobs", "Printer", Kind::Text),
    o("Terminal", "Copy as rich text too", "PasteRTF", Kind::Flag(false)),
    o("Terminal", "Ignore title changes from the host", "NoRemoteWinTitle", Kind::Flag(false)),
    o("Terminal", "Ignore mouse reporting requests", "NoMouseReporting", Kind::Flag(false)),
    o("Terminal", "Disable bracketed paste", "DisableBracketedPaste", Kind::Flag(false)),
    o("Keyboard", "Backspace sends Control-? (not Control-H)", "BackspaceIsDelete", Kind::Flag(true)),
    o("Keyboard", "Function keys", "LinuxFunctionKeys", Kind::Choice(0, FUNCTION_KEYS)),
    o("Keyboard", "Shifted arrow keys", "ShiftedArrowKeys", Kind::Choice(0, ARROWS)),
    o("Keyboard", "rxvt-style Home and End", "RXVTHomeEnd", Kind::Flag(false)),
    o("Keyboard", "Application cursor keys at start", "ApplicationCursorKeys", Kind::Flag(false)),
    o("Keyboard", "Application keypad at start", "ApplicationKeypad", Kind::Flag(false)),
    o("Keyboard", "Refuse application cursor keys", "NoApplicationCursors", Kind::Flag(false)),
    o("Keyboard", "Refuse application keypad", "NoApplicationKeys", Kind::Flag(false)),
    o("Keyboard", "NetHack keypad", "NetHackKeypad", Kind::Flag(false)),
    o("Keyboard", "AltGr is a compose key", "ComposeKey", Kind::Flag(false)),
    o("Keyboard", "Ctrl+Alt isn't AltGr", "CtrlAltKeys", Kind::Flag(true)),
    o("Keyboard", "Caps Lock means Cyrillic", "CapsLockCyr", Kind::Flag(false)),
    o("Keyboard", "Alt+F4 closes the window", "AltF4", Kind::Flag(true)),
    o("Keyboard", "Alt+Space opens the window menu", "AltSpace", Kind::Flag(false)),
    o("Keyboard", "Alt by itself opens the window menu", "AltOnly", Kind::Flag(false)),
    o("Keyboard", "Alt+Enter goes full screen", "FullScreenOnAltEnter", Kind::Flag(false)),
    o("Mouse", "Mouse buttons", "MouseIsXterm", Kind::Choice(0, MOUSE)),
    o("Mouse", "Shift overrides mouse reporting", "MouseOverride", Kind::Flag(true)),
    o("Mouse", "Selecting copies automatically", "MouseAutocopy", Kind::Flag(true)),
    o("Mouse", "Rectangular selection by default", "RectSelect", Kind::Flag(false)),
    o("Mouse", "Mouse paste (none turns it off)", "MousePaste", Kind::Text),
    o("Mouse", "Ctrl+Insert and Shift+Insert (none turns them off)", "CtrlShiftIns", Kind::Text),
    o("Mouse", "Hide the pointer while typing", "HideMousePtr", Kind::Flag(false)),
    o("Bell", "Bell", "Beep", Kind::Choice(1, BEEP)),
    o("Bell", "Sound file for the bell", "BellWaveFile", Kind::Text),
    o("Bell", "Taskbar", "BeepInd", Kind::Choice(0, BEEP_IND)),
    o("Bell", "Silence bursts of bells", "BellOverload", Kind::Flag(true)),
    o("Bell", "Bells in a burst", "BellOverloadN", Kind::Number(5)),
    o("Bell", "Burst length, in ms", "BellOverloadT", Kind::Number(2000)),
    o("Bell", "Silence for, in ms", "BellOverloadS", Kind::Number(5000)),
    o("Window", "Window title", "WinTitle", Kind::Text),
    o("Window", "Font", "Font", Kind::Text),
    o("Window", "Font size", "FontHeight", Kind::Number(10)),
    o("Window", "Bold font", "BoldFont", Kind::Text),
    o("Window", "Wide font", "WideFont", Kind::Text),
    o("Window", "Wide bold font", "WideBoldFont", Kind::Text),
    o("Window", "Font quality", "FontQuality", Kind::Choice(3, QUALITY)),
    o("Window", "Draw bold by overprinting", "ShadowBold", Kind::Flag(false)),
    o("Window", "Overprint offset", "ShadowBoldOffset", Kind::Number(1)),
    o("Window", "Columns", "TermWidth", Kind::Number(80)),
    o("Window", "Rows", "TermHeight", Kind::Number(24)),
    o("Window", "Session size", "LockSize", Kind::Choice(0, RESIZE)),
    o("Window", "Fit window to Columns, Rows", "FitWindow", Kind::Flag(false)),
    o("Window", "Cursor", "CurType", Kind::Choice(0, CURSOR)),
    o("Window", "Cursor blinks", "BlinkCur", Kind::Flag(false)),
    o("Window", "Scrollbar", "ScrollBar", Kind::Flag(true)),
    o("Window", "Scrollbar on the left", "ScrollbarOnLeft", Kind::Flag(false)),
    o("Window", "Scrollbar in full screen", "ScrollBarFullScreen", Kind::Flag(false)),
    o("Window", "Output scrolls to the bottom", "ScrollOnDisp", Kind::Flag(true)),
    o("Window", "A key press scrolls to the bottom", "ScrollOnKey", Kind::Flag(false)),
    o("Window", "Always on top", "AlwaysOnTop", Kind::Flag(false)),
    o("Window", "Gap around the text, in pixels", "WindowBorder", Kind::Number(1)),
    o("Window", "Sunken edge", "SunkenEdge", Kind::Flag(false)),
    o("Colours", "Bold text", "BoldAsColour", Kind::Choice(1, BOLD)),
    o("Colours", "Use system colours", "UseSystemColours", Kind::Flag(false)),
    o("Colours", "ANSI colours", "ANSIColour", Kind::Flag(true)),
    o("Colours", "256 colours", "Xterm256Colour", Kind::Flag(true)),
    o("Colours", "True colour", "TrueColour", Kind::Flag(true)),
    o("Connection", "Seconds between keepalives (0 for none)", "PingIntervalSecs", Kind::Number(0)),
    o("Connection", "Disable Nagle's algorithm", "TCPNoDelay", Kind::Flag(true)),
    o("Connection", "Internet protocol", "AddressFamily", Kind::Choice(0, FAMILY)),
    o("Connection", "Terminal modes (ERASE=A,INTR=V^C)", "TerminalModes", Kind::Text),
    o("Connection", "Telnet: wait for the server to negotiate", "PassiveTelnet", Kind::Flag(false)),
    o("Connection", "Telnet: keys send Telnet commands", "TelnetKey", Kind::Flag(false)),
    o("Connection", "Telnet: Return sends a Telnet new line", "TelnetRet", Kind::Flag(true)),
    o("Connection", "Telnet: RFC 1408 environment", "RFCEnviron", Kind::Flag(false)),
    o("Connection", "TCP keepalives", "TCPKeepalives", Kind::Flag(false)),
    o("Connection", "Logical host name for host keys", "LogHost", Kind::Text),
    o("Connection", "Command to run before connecting", "PreConnectCommand", Kind::Text),
    o("Connection", "User name", "UserName", Kind::Text),
    o("Connection", "Use system user name", "UserNameFromEnvironment", Kind::Flag(false)),
    o("Connection", "Local user name (rlogin)", "LocalUserName", Kind::Text),
    o("Connection", "Environment (NAME=value,NAME=value)", "Environment", Kind::Text),
    o("Proxy", "Proxy type", "ProxyMethod", Kind::Choice(0, PROXIES)),
    o("Proxy", "Proxy host, or a saved session for SSH", "ProxyHost", Kind::Text),
    o("Proxy", "Proxy port", "ProxyPort", Kind::Number(80)),
    o("Proxy", "Do not use the proxy for", "ProxyExcludeList", Kind::Text),
    o("Proxy", "Use the proxy for local addresses", "ProxyLocalhost", Kind::Flag(false)),
    o("Proxy", "Proxy looks up names", "ProxyDNS", Kind::Choice(1, DNS)),
    o("Proxy", "User name", "ProxyUsername", Kind::Text),
    o("Proxy", "Telnet or local command", "ProxyTelnetCommand", Kind::Text),
    o("SSH", "Remote command", "RemoteCommand", Kind::Text),
    o("SSH", "Share connections to the same host", "ConnectionSharing", Kind::Flag(false)),
    o("SSH", "Let others share this connection", "ConnectionSharingUpstream", Kind::Flag(true)),
    o("SSH", "Join a connection to share", "ConnectionSharingDownstream", Kind::Flag(true)),
    o("SSH", "Don't start a shell or command", "SshNoShell", Kind::Flag(false)),
    o("SSH", "Don't allocate a terminal", "NoPTY", Kind::Flag(false)),
    o("SSH", "Enable compression", "Compression", Kind::Flag(false)),
    o("SSH", "Cipher order", "Cipher", Kind::Text),
    o("SSH", "Key exchange order", "KEX", Kind::Text),
    o("SSH", "Host key order", "HostKey", Kind::Text),
    o("SSH", "Prefer host keys already known", "PreferKnownHostKeys", Kind::Flag(true)),
    o("SSH", "Rekey after minutes (0 for never)", "RekeyTime", Kind::Number(60)),
    o("SSH", "Rekey after data (1G, 100M)", "RekeyBytes", Kind::Text),
    o("SSH", "Show the server's banner", "SshBanner", Kind::Flag(true)),
    o("SSH", "Host keys or fingerprints to trust", "SSHManualHostKeys", Kind::Text),
    o("Auth", "Private key file", "PublicKeyFile", Kind::Text),
    o("Auth", "Try Pageant", "TryAgent", Kind::Flag(true)),
    o("Auth", "Try keyboard-interactive", "AuthKI", Kind::Flag(true)),
    o("Auth", "Disconnect if login is not needed", "SshNoTrivialAuth", Kind::Flag(false)),
    o("Auth", "Skip login altogether", "SshNoAuth", Kind::Flag(false)),
    o("Auth", "Certificate file for the key", "DetachedCertificate", Kind::Text),
    o("Auth", "Authentication plugin command", "AuthPlugin", Kind::Text),
    o("Auth", "Allow changing user name between attempts", "ChangeUsername", Kind::Flag(false)),
    o("Forwarding", "Port forwards (L8080=host:80,R2222=host:22,D1080=)", "PortForwardings", Kind::Text),
    o("Forwarding", "Local ports accept other hosts", "LocalPortAcceptAll", Kind::Flag(false)),
    o("Forwarding", "Remote ports do the same", "RemotePortAcceptAll", Kind::Flag(false)),
    o("Forwarding", "Forward Pageant", "AgentFwd", Kind::Flag(false)),
    o("Forwarding", "Forward X11", "X11Forward", Kind::Flag(false)),
    o("Forwarding", "X display", "X11Display", Kind::Text),
    o("Forwarding", "X authority file", "X11AuthFile", Kind::Text),
    o("Forwarding", "X login type", "X11AuthType", Kind::Choice(1, X11_AUTH)),
    o("Serial", "SUPDUP location", "SUPDUPLocation", Kind::Text),
    o("Serial", "SUPDUP character set", "SUPDUPCharset", Kind::Choice(0, SUPDUP)),
    o("Serial", "SUPDUP **MORE** processing", "SUPDUPMoreProcessing", Kind::Flag(false)),
    o("Serial", "SUPDUP terminal scrolling", "SUPDUPScrolling", Kind::Flag(false)),
    o("Serial", "Serial line", "SerialLine", Kind::Text),
    o("Serial", "Speed", "SerialSpeed", Kind::Number(9600)),
    o("Serial", "Data bits", "SerialDataBits", Kind::Number(8)),
    o("Serial", "Stop bits, doubled", "SerialStopHalfbits", Kind::Number(2)),
    o("Serial", "Parity", "SerialParity", Kind::Choice(0, PARITY)),
    o("Serial", "Flow control", "SerialFlowControl", Kind::Choice(1, FLOW)),
];

/// Acted on, with no entry in `OPTIONS`. The proxy password is typed when needed,
/// or kept as saved, and the settings window's masked box edits it. SSH logs hold no passwords
/// or packets, so the `SSHLogOmit` settings are always met.
pub const AUTOMATIC: &[&str] = &["ProxyPassword", "PingInterval", "ProxyLogToTerm", "Present", "SSHLogOmitData", "SSHLogOmitPasswords", "NoRemoteResize", "WinNameAlways"];

/// Settings for things that don't exist on Windows or in a Unicode terminal.
pub const NOT_APPLICABLE: &[&str] = &["OSXCommandMeta", "OSXOptionMeta", "LoginShell", "StampUtmp", "WindowClass", "TryPalette", "AuthTIS", "RawCNP", "FontVTMode"];

/// PuTTY settings the deck keeps but doesn't act on, and why.
pub const NOT_DONE: &[(&str, &[&str])] = &[
    ("GSSAPI / Kerberos login: russh has no support", &["AuthGSSAPI", "AuthGSSAPIKEX", "GSSCustom", "GSSLibs", "GssapiFwd", "GssapiRekey"]),
    ("Single DES, which russh doesn't have", &["SSH2DES"]),
    ("Workarounds for old servers: russh decides these itself", &["BugChanReq", "BugDeriveKey2", "BugDropStart", "BugFilterKexinit", "BugHMAC2", "BugIgnore1", "BugIgnore2", "BugMaxPkt2", "BugOldGex2", "BugPKSessID2", "BugPlainPW1", "BugRSA1", "BugRSAPad2", "BugRSASHA2CertUserauth", "BugRekey2", "BugWinadj"]),
    ("SSH-1, which russh doesn't have", &["SshProt"]),
    ("alacritty_terminal has no way to do these", &["BCE", "BlinkText", "CJKAmbigWide", "DisableArabicShaping", "DisableBidi", "UTF8Override", "UTF8linedraw"]),
    ("The deck always keeps its own Ctrl+Shift+C and V", &["CtrlShiftCV"]),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // settings.c's and conf.h's keywords, from PuTTY 0.85.
    const PUTTY: &str = include_str!("../tests/putty_keywords.txt");

    #[test]
    fn every_putty_keyword_is_accounted_for_once() {
        let mut seen = HashSet::new();
        let not_done = NOT_DONE.iter().flat_map(|(_, keys)| keys.iter().copied());
        for key in OPTIONS.iter().map(|o| o.key).chain(AUTOMATIC.iter().copied()).chain(NOT_APPLICABLE.iter().copied()).chain(not_done) {
            assert!(seen.insert(key), "{key} is listed twice");
        }
        let putty: HashSet<&str> = PUTTY.split_whitespace().collect();
        let mut missing: Vec<_> = putty.difference(&seen).collect();
        missing.sort();
        assert!(missing.is_empty(), "not accounted for: {missing:?}");
        let mut extra: Vec<_> = seen.difference(&putty).collect();
        extra.retain(|k| !["FontHeight", "RemotePortAcceptAll", "BellOverloadT", "BellOverloadS", "FitWindow", "PasteWarnLines"].contains(*k));
        assert!(extra.is_empty(), "not PuTTY keywords: {extra:?}");
    }

    #[test]
    fn every_option_has_a_known_category() {
        assert!(OPTIONS.iter().all(|o| CATEGORIES.contains(&o.category)));
    }
}
