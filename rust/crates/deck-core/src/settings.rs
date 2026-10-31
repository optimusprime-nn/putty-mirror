//! A saved session's settings, using PuTTY's own keywords and storage formats
//! (`conf.h` and `settings.c`), so nothing in a session is lost by loading it.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(u32),
}

#[derive(Clone, Default, PartialEq)]
pub struct Settings {
    values: BTreeMap<String, Value>,
}

// Written by hand so that a proxy password can never reach a log.
impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Settings({} values)", self.values.len())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Forward {
    /// `L`: listen here, connect from the server.
    Local { bind: Option<String>, port: u16, host: String, host_port: u16 },
    /// `R`: listen on the server, connect from here.
    Remote { bind: Option<String>, port: u16, host: String, host_port: u16 },
    /// `D`: a SOCKS proxy here, connecting from the server.
    Dynamic { bind: Option<String>, port: u16 },
}

impl Settings {
    pub fn set(&mut self, key: &str, value: Value) {
        self.values.insert(key.to_string(), value);
    }

    pub fn has(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.values.iter()
    }

    pub fn s(&self, key: &str, default: &str) -> String {
        match self.values.get(key) {
            Some(Value::Str(s)) => s.clone(),
            _ => default.to_string(),
        }
    }

    pub fn i(&self, key: &str, default: u32) -> u32 {
        match self.values.get(key) {
            Some(Value::Int(i)) => *i,
            _ => default,
        }
    }

    pub fn b(&self, key: &str, default: bool) -> bool {
        self.i(key, default as u32) != 0
    }

    /// The `key=value,key=value` form of `gppmap`, with its backslash escapes.
    pub fn map(&self, key: &str) -> Vec<(String, String)> {
        let text = self.s(key, "");
        let mut out = Vec::new();
        let mut chars = text.chars();
        let (mut k, mut v, mut in_value) = (String::new(), String::new(), false);
        loop {
            match chars.next() {
                None | Some(',') => {
                    if !k.is_empty() || in_value {
                        out.push((std::mem::take(&mut k), std::mem::take(&mut v)));
                    }
                    in_value = false;
                }
                Some(c) => {
                    let c = match c {
                        '=' if !in_value => {
                            in_value = true;
                            continue;
                        }
                        '\\' => chars.next().unwrap_or('\\'),
                        c => c,
                    };
                    if in_value {
                        v.push(c);
                    } else {
                        k.push(c);
                    }
                    continue;
                }
            }
            if chars.as_str().is_empty() {
                return out;
            }
        }
    }

    /// Seconds between keepalives. PuTTY adds its minutes and seconds keywords.
    pub fn ping_seconds(&self) -> u32 {
        self.i("PingInterval", 0) * 60 + self.i("PingIntervalSecs", 0)
    }

    pub fn forwards(&self) -> Vec<Forward> {
        self.map("PortForwardings").iter().filter_map(|(k, v)| parse_forward(k, v)).collect()
    }
}

/// A key is `[4|6]` then `L`, `R` or `D`, then `[bind:]port`. The value is
/// `host:port`, and a bracketed IPv6 host is allowed.
fn parse_forward(key: &str, value: &str) -> Option<Forward> {
    let key = key.trim_start_matches(['4', '6']);
    let kind = key.chars().next()?;
    let rest = &key[1..];
    let (bind, port) = match rest.rsplit_once(':') {
        Some((b, p)) => (Some(b.to_string()), p),
        None => (None, rest),
    };
    let port = port.parse().ok()?;
    if kind == 'D' || value == "D" {
        return Some(Forward::Dynamic { bind, port });
    }
    let (host, host_port) = value.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']).to_string();
    let host_port = host_port.parse().ok()?;
    match kind {
        'L' => Some(Forward::Local { bind, port, host, host_port }),
        'R' => Some(Forward::Remote { bind, port, host, host_port }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(key: &str, value: &str) -> Settings {
        let mut s = Settings::default();
        s.set(key, Value::Str(value.into()));
        s
    }

    #[test]
    fn maps_unescape_like_gppmap() {
        let s = with("Environment", r"A=1,B=x\,y,C=p\=q");
        let want = [("A", "1"), ("B", "x,y"), ("C", "p=q")].map(|(k, v)| (k.to_string(), v.to_string()));
        assert_eq!(s.map("Environment"), want);
        assert!(Settings::default().map("Environment").is_empty());
    }

    #[test]
    fn forwards_parse_in_every_form() {
        let s = with("PortForwardings", "L8080=web:80,4R2222=localhost:22,D1080=,6L127.0.0.1:9000=[::1]:90");
        assert_eq!(
            s.forwards(),
            vec![
                Forward::Local { bind: None, port: 8080, host: "web".into(), host_port: 80 },
                Forward::Remote { bind: None, port: 2222, host: "localhost".into(), host_port: 22 },
                Forward::Dynamic { bind: None, port: 1080 },
                Forward::Local { bind: Some("127.0.0.1".into()), port: 9000, host: "::1".into(), host_port: 90 },
            ]
        );
    }

    #[test]
    fn debug_never_shows_values() {
        assert!(!format!("{:?}", with("ProxyPassword", "hunter2")).contains("hunter2"));
    }

    #[test]
    fn ping_adds_both_keywords() {
        let mut s = Settings::default();
        s.set("PingInterval", Value::Int(2));
        s.set("PingIntervalSecs", Value::Int(5));
        assert_eq!(s.ping_seconds(), 125);
    }
}
