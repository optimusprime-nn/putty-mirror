//! Session logging, using PuTTY's `Log*` settings and file name codes.

use crate::config::Target;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const PRINTABLE: u32 = 1;

pub struct Log {
    file: File,
    printable: bool,
    flush: bool,
    in_escape: Escape,
}

#[derive(PartialEq)]
enum Escape {
    None,
    Started,
    Csi,
    Osc,
}

/// Year, month, day, hour, minute and second, in UTC.
fn civil(unix: u64) -> (i64, u32, u32, u32, u32, u32) {
    // Howard Hinnant's days-to-civil algorithm
    let days = (unix / 86400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    let secs = unix % 86400;
    (year, month, day, (secs / 3600) as u32, (secs / 60 % 60) as u32, (secs % 60) as u32)
}

/// Expands `&Y &M &D &T &H &P`, and `&&` for a single `&` (`xlatlognam`).
pub fn expand_name(pattern: &str, host: &str, port: u16, unix: u64) -> String {
    let (y, mo, d, h, mi, s) = civil(unix);
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y' | 'y') => out += &format!("{y:04}"),
            Some('M' | 'm') => out += &format!("{mo:02}"),
            Some('D' | 'd') => out += &format!("{d:02}"),
            Some('T' | 't') => out += &format!("{h:02}{mi:02}{s:02}"),
            Some('H' | 'h') => out += host,
            Some('P' | 'p') => out += &port.to_string(),
            Some('&') => out.push('&'),
            Some(other) => {
                out.push('&');
                out.push(other);
            }
            None => out.push('&'),
        }
    }
    out
}

impl Log {
    pub fn open(target: &Target) -> anyhow::Result<Option<Log>> {
        let s = &target.settings;
        let kind = s.i("LogType", 0);
        if kind == 0 {
            return Ok(None);
        }
        let unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let name = expand_name(&s.s("LogFileName", "putty.log"), &target.host, target.port, unix);
        // LogFileClash: 1 is overwrite, anything else appends, so that the
        // "ask" setting can't lose an old log
        let overwrite = s.i("LogFileClash", u32::MAX) == 1;
        let mut file = OpenOptions::new().create(true).write(true).append(!overwrite).truncate(overwrite).open(&name)?;
        if s.b("LogHeader", true) {
            let (y, mo, d, h, mi, sec) = civil(unix);
            writeln!(file, "=~=~=~=~=~=~=~=~=~=~=~= PuTTYDeck log {y:04}.{mo:02}.{d:02} {h:02}:{mi:02}:{sec:02} UTC =~=~=~=~=~=~=~=~=~=~=~=")?;
        }
        Ok(Some(Log { file, printable: kind == PRINTABLE, flush: s.b("LogFlush", true), in_escape: Escape::None }))
    }

    /// The bytes of `data` a person reads, without terminal control sequences.
    fn filter(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        for &b in data {
            match self.in_escape {
                Escape::None => match b {
                    0x1b => self.in_escape = Escape::Started,
                    b'\n' | b'\t' => out.push(b),
                    b'\r' | 0x07 | 0x08 => {}
                    b => out.push(b),
                },
                Escape::Started => {
                    self.in_escape = match b {
                        b'[' => Escape::Csi,
                        b']' => Escape::Osc,
                        _ => Escape::None,
                    }
                }
                Escape::Csi if (0x40..=0x7e).contains(&b) => self.in_escape = Escape::None,
                Escape::Csi => {}
                Escape::Osc if b == 0x07 || b == 0x1b => self.in_escape = Escape::None,
                Escape::Osc => {}
            }
        }
        out
    }

    pub fn write(&mut self, data: &[u8]) {
        let filtered;
        let bytes = if self.printable {
            filtered = self.filter(data);
            &filtered[..]
        } else {
            data
        };
        // a full disk shouldn't take the session down with it
        let _ = self.file.write_all(bytes);
        if self.flush {
            let _ = self.file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_expand_like_xlatlognam() {
        // 2024-03-05 06:07:08 UTC
        let unix = 1_709_618_828;
        assert_eq!(expand_name("&H-&P-&Y&M&D-&T.log", "box", 22, unix), "box-22-20240305-060708.log");
        assert_eq!(expand_name("a&&b&x", "h", 1, unix), "a&b&x");
    }

    #[test]
    fn printable_log_drops_control_sequences() {
        let dir = std::env::temp_dir().join(format!("deck-log-{}", std::process::id()));
        let path = dir.with_extension("log");
        let mut t = Target::new(crate::config::Protocol::Ssh, "h", 22);
        t.settings.set("LogType", crate::settings::Value::Int(1));
        t.settings.set("LogHeader", crate::settings::Value::Int(0));
        t.settings.set("LogFileClash", crate::settings::Value::Int(1));
        t.settings.set("LogFileName", crate::settings::Value::Str(path.display().to_string()));
        let mut log = Log::open(&t).unwrap().unwrap();
        log.write(b"\x1b[1;31mred\x1b[0m\r\n\x1b]0;title\x07pl");
        log.write(b"ain\n");
        drop(log);
        let got = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(got, b"red\nplain\n");
    }
}
