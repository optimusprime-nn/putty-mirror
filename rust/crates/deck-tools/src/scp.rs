//! The old SCP protocol, for servers with no SFTP. It's a conversation with
//! `scp -t` (receive) or `scp -f` (send) running on the server.

use crate::safe_local_name;
use anyhow::{anyhow, bail, Context};
use deck_core::session::clean;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_LINE: usize = 8192;

/// Quotes a path for the remote shell, which reads the whole command line.
pub fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

pub fn receive_command(path: &str, recurse: bool) -> String {
    format!("scp {}-t -- {}", if recurse { "-r " } else { "" }, shell_quote(path))
}

pub fn send_command(path: &str, recurse: bool) -> String {
    format!("scp {}-f -- {}", if recurse { "-r " } else { "" }, shell_quote(path))
}

async fn read_line<S: AsyncRead + Unpin>(s: &mut S) -> anyhow::Result<String> {
    let mut line = Vec::new();
    loop {
        match s.read_u8().await.context("the server stopped mid-transfer")? {
            b'\n' => return Ok(String::from_utf8_lossy(&line).into_owned()),
            b if line.len() < MAX_LINE => line.push(b),
            _ => bail!("the server sent an overlong line"),
        }
    }
}

/// Reads the server's answer: zero is fine, anything else comes with a message.
async fn ack<S: AsyncRead + Unpin>(s: &mut S) -> anyhow::Result<()> {
    match s.read_u8().await.context("the server stopped mid-transfer")? {
        0 => Ok(()),
        _ => bail!("the server said: {}", clean(&read_line(s).await?)),
    }
}

async fn put_file<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, from: &Path, name: &str) -> anyhow::Result<()> {
    let data = std::fs::read(from).with_context(|| format!("reading {}", from.display()))?;
    s.write_all(format!("C0644 {} {name}\n", data.len()).as_bytes()).await?;
    s.flush().await?;
    ack(s).await?;
    s.write_all(&data).await?;
    s.write_all(&[0]).await?;
    s.flush().await?;
    ack(s).await?;
    println!("{} -> {name} ({} bytes)", from.display(), data.len());
    Ok(())
}

pub fn upload<'a, S: AsyncRead + AsyncWrite + Unpin>(s: &'a mut S, from: &'a Path, recurse: bool) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + 'a>> {
    Box::pin(async move {
        let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| anyhow!("{} has no name", from.display()))?;
        anyhow::ensure!(!name.contains('\n'), "a file name can't contain a line end in SCP");
        if !from.is_dir() {
            return put_file(s, from, &name).await;
        }
        anyhow::ensure!(recurse, "{} is a folder: use -r", from.display());
        s.write_all(format!("D0755 0 {name}\n").as_bytes()).await?;
        s.flush().await?;
        ack(s).await?;
        for entry in std::fs::read_dir(from)? {
            upload(s, &entry?.path(), recurse).await?;
        }
        s.write_all(b"E\n").await?;
        s.flush().await?;
        ack(s).await
    })
}

/// Parses `C0644 size name` or `D0755 0 name`. A name from the server has to
/// stay inside the destination, so anything like a path is refused.
fn parse_entry(line: &str) -> anyhow::Result<(u64, String)> {
    let mut parts = line[1..].splitn(3, ' ');
    let (_mode, size, name) = (parts.next(), parts.next(), parts.next());
    let (Some(size), Some(name)) = (size, name) else { bail!("the server sent a bad entry: {line}") };
    anyhow::ensure!(safe_local_name(name), "the server sent an unsafe file name: {}", clean(name));
    Ok((size.parse().context("the server sent a bad size")?, name.to_string()))
}

/// Copies a file's bytes to disk as they come, however large the server says it is.
async fn receive_file<S: AsyncRead + Unpin>(s: &mut S, target: &Path, size: u64) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(target).with_context(|| format!("writing {}", target.display()))?;
    let mut left = size;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        s.read_exact(&mut buf[..want]).await.context("the server stopped mid-transfer")?;
        file.write_all(&buf[..want])?;
        left -= want as u64;
    }
    Ok(())
}

/// Receives into `to`. A folder there gets the file by its own name.
/// `expected` is the name asked for: the server decides what it sends, so
/// anything else it offers at the top level is refused, and so is more than
/// the one thing asked for. `None` is `-unsafe`, for wildcards.
pub async fn download<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, to: &Path, recurse: bool, expected: Option<&str>) -> anyhow::Result<()> {
    s.write_all(&[0]).await?;
    s.flush().await?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut received = false;
    loop {
        let kind = match s.read_u8().await {
            Ok(k) => k,
            Err(_) if dirs.is_empty() => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        match kind {
            b'C' | b'D' => {
                let line = format!("{}{}", kind as char, read_line(s).await?);
                let (size, name) = parse_entry(&line)?;
                if let (Some(want), true) = (expected, dirs.is_empty()) {
                    anyhow::ensure!(!received && name == want, "the server sent {} when {} was asked for", clean(&name), clean(want));
                }
                received |= dirs.is_empty();
                let here = dirs.last().cloned().unwrap_or_else(|| to.to_path_buf());
                let target = if !dirs.is_empty() || here.is_dir() { here.join(&name) } else { here };
                if kind == b'D' {
                    anyhow::ensure!(recurse, "the server sent a folder without -r");
                    std::fs::create_dir_all(&target)?;
                    dirs.push(target);
                    s.write_all(&[0]).await?;
                } else {
                    s.write_all(&[0]).await?;
                    s.flush().await?;
                    receive_file(s, &target, size).await?;
                    ack(s).await?;
                    println!("{} -> {} ({size} bytes)", clean(&name), clean(&target.display().to_string()));
                    s.write_all(&[0]).await?;
                }
                s.flush().await?;
            }
            b'E' => {
                read_line(s).await?;
                dirs.pop();
                s.write_all(&[0]).await?;
                s.flush().await?;
            }
            // a time stamp, which isn't kept
            b'T' => {
                read_line(s).await?;
                s.write_all(&[0]).await?;
                s.flush().await?;
            }
            1 | 2 => bail!("the server said: {}", clean(&read_line(s).await?)),
            k => bail!("the server sent an unknown message {k}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("deck-scp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn paths_are_quoted_for_the_shell() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("x'; rm -rf ~; '"), r"'x'\''; rm -rf ~; '\'''");
        assert_eq!(receive_command("/tmp", true), "scp -r -t -- '/tmp'");
    }

    #[tokio::test]
    async fn upload_speaks_the_protocol() {
        let dir = temp("up");
        let file = dir.join("f.txt");
        std::fs::write(&file, b"hello").unwrap();
        let (mut near, mut far) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let line = read_line(&mut far).await.unwrap();
            far.write_all(&[0]).await.unwrap();
            let mut body = [0u8; 6];
            far.read_exact(&mut body).await.unwrap();
            far.write_all(&[0]).await.unwrap();
            (line, body)
        });
        upload(&mut near, &file, false).await.unwrap();
        let (line, body) = server.await.unwrap();
        assert_eq!(line, "C0644 5 f.txt");
        assert_eq!(&body, b"hello\0");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn download_writes_a_tree() {
        let dir = temp("down");
        let (mut near, mut far) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut ok = [0u8; 1];
            far.read_exact(&mut ok).await.unwrap();
            for step in [&b"D0755 0 sub\n"[..], b"C0644 2 a\n"] {
                far.write_all(step).await.unwrap();
                far.read_exact(&mut ok).await.unwrap();
            }
            far.write_all(b"hi\0").await.unwrap();
            far.read_exact(&mut ok).await.unwrap();
            far.write_all(b"E\n").await.unwrap();
            far.read_exact(&mut ok).await.unwrap();
        });
        download(&mut near, &dir, true, Some("sub")).await.unwrap();
        server.await.unwrap();
        assert_eq!(std::fs::read(dir.join("sub").join("a")).unwrap(), b"hi");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_server_cannot_write_outside_the_destination() {
        for evil in ["C0644 1 ../evil\n", "C0644 1 ..\n", "C0644 1 c:\\evil\n", "D0755 0 ..\n", "C0644 1 NUL\n", "C0644 1 COM1.txt\n"] {
            let dir = temp("evil");
            let (mut near, mut far) = tokio::io::duplex(4096);
            far.write_all(evil.as_bytes()).await.unwrap();
            let e = download(&mut near, &dir, true, None).await.unwrap_err();
            assert!(e.to_string().contains("unsafe"), "{evil}: {e}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    async fn the_servers_error_is_reported() {
        let (mut near, mut far) = tokio::io::duplex(4096);
        far.write_all(b"\x01no such file\n").await.unwrap();
        let e = download(&mut near, Path::new("."), false, Some("x")).await.unwrap_err();
        assert!(e.to_string().contains("no such file"));
    }

    #[tokio::test]
    async fn a_server_cannot_choose_what_lands_in_the_destination() {
        let dir = temp("names");
        let (mut near, mut far) = tokio::io::duplex(4096);
        far.write_all(b"C0644 1 update.exe\n").await.unwrap();
        let e = download(&mut near, &dir, false, Some("report.txt")).await.unwrap_err();
        assert!(e.to_string().contains("asked for"), "{e}");
        assert!(!dir.join("update.exe").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_server_cannot_send_more_than_was_asked_for() {
        let dir = temp("extra");
        let (mut near, mut far) = tokio::io::duplex(4096);
        far.write_all(b"C0644 2 report.txt\nhi\0C0644 1 report.txt\n").await.unwrap();
        let e = download(&mut near, &dir, false, Some("report.txt")).await.unwrap_err();
        assert!(e.to_string().contains("asked for"), "{e}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_huge_claimed_size_allocates_nothing() {
        let dir = temp("huge");
        let (mut near, mut far) = tokio::io::duplex(4096);
        far.write_all(b"C0644 18446744073709551615 big\nabc").await.unwrap();
        drop(far);
        assert!(download(&mut near, &dir, false, Some("big")).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
