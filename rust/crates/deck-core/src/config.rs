//! Connection targets, and saved sessions. PuTTY's own are only read: PuTTYDeck
//! never changes PuTTY's settings. A session saved here goes in PuTTYDeck's own
//! key, and takes the place of a PuTTY session of the same name.

use crate::settings::{Settings, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Protocol {
    Ssh,
    Telnet,
    Raw,
    Rlogin,
    Supdup,
    Serial,
    /// A protocol neither PuTTY nor PuTTYDeck knows, kept so it can be refused by name.
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub key_file: Option<String>,
    /// Why this can't be opened faithfully. Connecting anyway could bypass a
    /// proxy the user relies on, so a session with this set is refused.
    pub refuse: Option<String>,
    /// Settings that are ignored, shown to the user when the session opens.
    pub notes: Vec<String>,
    pub settings: Settings,
}

impl Target {
    pub fn new(protocol: Protocol, host: &str, port: u16) -> Target {
        Target {
            protocol,
            host: host.to_string(),
            port,
            user: None,
            key_file: None,
            refuse: None,
            notes: Vec::new(),
            settings: Settings::default(),
        }
    }

    /// Parses what is typed into the connect box: `[proto://][user@]host[:port]`.
    pub fn parse(text: &str) -> Option<Target> {
        let text = text.trim();
        let (protocol, rest) = match text.split_once("://") {
            Some(("ssh", r)) => (Protocol::Ssh, r),
            Some(("telnet", r)) => (Protocol::Telnet, r),
            Some(("raw", r)) => (Protocol::Raw, r),
            Some(("rlogin", r)) => (Protocol::Rlogin, r),
            Some(("supdup", r)) => (Protocol::Supdup, r),
            Some(("serial", r)) => (Protocol::Serial, r),
            Some(_) => return None,
            None => (Protocol::Ssh, text),
        };
        let (user, hostport) = match rest.rsplit_once('@') {
            Some((u, h)) if !u.is_empty() => (Some(u.to_string()), h),
            _ => (None, rest),
        };
        let default_port = match protocol {
            Protocol::Telnet => 23,
            Protocol::Rlogin => 513,
            Protocol::Supdup => 95,
            _ => 22,
        };
        let (host, port) = split_port(hostport, default_port)?;
        if host.is_empty() || host.contains(char::is_whitespace) {
            return None;
        }
        Some(Target { user, ..Target::new(protocol, host, port) })
    }

    /// The label for the session list.
    pub fn label(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

fn split_port(s: &str, default: u16) -> Option<(&str, u16)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        return match tail.strip_prefix(':') {
            Some(p) => Some((host, p.parse().ok()?)),
            None if tail.is_empty() => Some((host, default)),
            None => None,
        };
    }
    match s.split_once(':') {
        Some((h, p)) if !p.contains(':') => Some((h, p.parse().ok()?)),
        Some(_) => Some((s, default)), // a bare IPv6 address
        None => Some((s, default)),
    }
}

/// Builds a target from a saved session's settings.
pub fn target_from(settings: Settings) -> Target {
    let name = settings.s("Protocol", "ssh");
    let serial = name == "serial";
    let protocol = match name.as_str() {
        "ssh" => Protocol::Ssh,
        "telnet" => Protocol::Telnet,
        "raw" => Protocol::Raw,
        "rlogin" => Protocol::Rlogin,
        "supdup" => Protocol::Supdup,
        "serial" => Protocol::Serial,
        other => Protocol::Unsupported(other.to_string()),
    };
    let host = if serial { settings.s("SerialLine", "") } else { settings.s("HostName", "") };
    let mut t = Target::new(protocol, &host, settings.i("PortNumber", 22).min(65535) as u16);
    let user = settings.s("UserName", "");
    if !user.is_empty() {
        t.user = Some(user);
    }
    let key = settings.s("PublicKeyFile", "");
    if !key.is_empty() {
        t.key_file = Some(key);
    }
    t.settings = settings;
    if host.is_empty() {
        t.refuse = Some("this saved session has no host name".into());
    } else if let Protocol::Unsupported(p) = &t.protocol {
        t.refuse = Some(format!("PuTTYDeck doesn't support the {p} protocol"));
    }
    t
}

/// An SSH proxy host: the name of a saved session, or a plain host name.
pub(crate) fn named_or_host(name: &str, port: u16, user: &str) -> Target {
    #[cfg(windows)]
    if let Some((_, t)) = load_sessions().into_iter().find(|(n, _)| n == name) {
        return t;
    }
    let mut t = Target::new(Protocol::Ssh, name, if port == 80 { 22 } else { port });
    if !user.is_empty() {
        t.user = Some(user.to_string());
    }
    t
}

/// Reverses PuTTY's escaping of registry key names.
pub fn unescape_key(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// PuTTY's escaping of registry key names (`escape_registry_key` in storage.c).
pub fn escape_key(s: &str) -> String {
    let mut out = String::new();
    for (i, &c) in s.as_bytes().iter().enumerate() {
        let bad = matches!(c, b' ' | b'\\' | b'*' | b'?' | b'%') || c < b' ' || c > b'~' || (c == b'.' && i == 0);
        if bad {
            out.push_str(&format!("%{c:02X}"));
        } else {
            out.push(c as char);
        }
    }
    out
}

#[cfg(windows)]
pub const PUTTY_KEY: &str = r"Software\SimonTatham\PuTTY";
#[cfg(windows)]
pub const DECK_KEY: &str = r"Software\PuTTYDeck";

/// Saved session names and settings, sorted, without "Default Settings".
#[cfg(windows)]
pub fn load_sessions() -> Vec<(String, Target)> {
    let mut all = read_sessions(DECK_KEY);
    for (name, target) in read_sessions(PUTTY_KEY) {
        if !all.iter().any(|(n, _)| *n == name) {
            all.push((name, target));
        }
    }
    all.sort_by_key(|(n, _)| n.to_lowercase());
    all
}

/// Saves a session in PuTTYDeck's key. Text and numbers keep PuTTY's types.
#[cfg(windows)]
pub fn save_session(name: &str, settings: &Settings) -> anyhow::Result<()> {
    use winreg::{enums::*, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let path = format!(r"{DECK_KEY}\Sessions\{}", escape_key(name));
    let (key, _) = hkcu.create_subkey_with_flags(path, KEY_SET_VALUE)?;
    for (k, v) in settings.iter() {
        match v {
            Value::Str(s) => key.set_value(k, s)?,
            Value::Int(i) => key.set_value(k, i)?,
        }
    }
    Ok(())
}

#[cfg(windows)]
fn read_sessions(parent: &str) -> Vec<(String, Target)> {
    use winreg::{enums::*, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(root) = hkcu.open_subkey_with_flags(format!(r"{parent}\Sessions"), KEY_READ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for key in root.enum_keys().flatten() {
        let name = unescape_key(&key);
        if name == "Default Settings" {
            continue;
        }
        let Ok(k) = root.open_subkey_with_flags(&key, KEY_READ) else { continue };
        let mut settings = Settings::default();
        for (value_name, data) in k.enum_values().flatten() {
            match data.vtype {
                REG_DWORD => {
                    if let Ok(v) = k.get_value::<u32, _>(&value_name) {
                        settings.set(&value_name, Value::Int(v));
                    }
                }
                _ => {
                    if let Ok(v) = k.get_value::<String, _>(&value_name) {
                        settings.set(&value_name, Value::Str(v));
                    }
                }
            }
        }
        out.push((name, target_from(settings)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run by hand. Reads the real registry and prints only counts, not hosts.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn real_saved_sessions_load() {
        let all = load_sessions();
        let refused = all.iter().filter(|(_, t)| t.refuse.is_some()).count();
        let noted = all.iter().filter(|(_, t)| !t.notes.is_empty()).count();
        let with_host = all.iter().filter(|(_, t)| !t.host.is_empty()).count();
        let keyed = all.iter().filter(|(_, t)| t.key_file.is_some()).count();
        let ppk = all.iter().filter(|(_, t)| t.key_file.as_deref().is_some_and(|k| k.to_lowercase().ends_with(".ppk"))).count();
        let ssh = all.iter().filter(|(_, t)| t.protocol == Protocol::Ssh).count();
        println!("sessions={} with_host={with_host} ssh={ssh} refused={refused} noted={noted} key_files={keyed} ppk={ppk}", all.len());
        assert!(!all.is_empty());
    }

    #[test]
    fn parses_connect_box() {
        let t = Target::parse("nick@example.org:2222").unwrap();
        assert_eq!((t.user.as_deref(), t.host.as_str(), t.port), (Some("nick"), "example.org", 2222));
        assert_eq!(Target::parse("example.org").unwrap().port, 22);
        let t = Target::parse("telnet://box").unwrap();
        assert_eq!((t.protocol, t.port), (Protocol::Telnet, 23));
        let t = Target::parse("[::1]:2200").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 2200));
        assert_eq!(Target::parse("fe80::1").unwrap().host, "fe80::1");
        assert!(Target::parse("").is_none());
        assert!(Target::parse("ftp://x").is_none());
        assert!(Target::parse("host:notaport").is_none());
    }

    #[test]
    fn key_escaping_round_trips() {
        assert_eq!(unescape_key("deck%20test"), "deck test");
        assert_eq!(escape_key("deck test"), "deck%20test");
        assert_eq!(escape_key(".hidden"), "%2Ehidden");
        assert_eq!(unescape_key(&escape_key("a*b?c%d")), "a*b?c%d");
        assert_eq!(unescape_key("trail%"), "trail%");
        assert_eq!(unescape_key("trail%2"), "trail%2");
    }

    fn saved(pairs: &[(&str, &str)]) -> Settings {
        let mut s = Settings::default();
        for (k, v) in pairs {
            s.set(k, Value::Str(v.to_string()));
        }
        s
    }

    #[test]
    fn saved_sessions_become_targets() {
        let t = target_from(saved(&[("HostName", "h"), ("Protocol", "rlogin"), ("UserName", "u")]));
        assert_eq!((t.protocol, t.user.as_deref(), t.refuse), (Protocol::Rlogin, Some("u"), None));
        let t = target_from(saved(&[("Protocol", "serial"), ("SerialLine", "COM3")]));
        assert_eq!((t.protocol, t.host.as_str()), (Protocol::Serial, "COM3"));
        assert!(target_from(saved(&[("Protocol", "ssh")])).refuse.unwrap().contains("no host"));
        assert!(target_from(saved(&[("HostName", "h"), ("Protocol", "odd")])).refuse.unwrap().contains("odd"));
    }
}
