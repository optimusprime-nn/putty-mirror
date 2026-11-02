//! Rlogin sessions, after `otherbackends/rlogin.c`.

use crate::config::Target;
use crate::proxy::Conn;
use crate::session::{Command, Event, Io};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn startup(local: &str, remote: &str, term: &str, speed: &str) -> Vec<u8> {
    let mut msg = vec![0];
    for field in [local, remote] {
        msg.extend(field.as_bytes());
        msg.push(0);
    }
    msg.extend(format!("{term}/{speed}").as_bytes());
    msg.push(0);
    msg
}

fn window_size(cols: u32, rows: u32) -> [u8; 12] {
    let (r, c) = (rows as u16, cols as u16);
    let [r1, r0] = r.to_be_bytes();
    let [c1, c0] = c.to_be_bytes();
    [0xff, 0xff, b's', b's', r1, r0, c1, c0, 0, 0, 0, 0]
}

pub(crate) async fn run(target: &Target, mut conn: Conn, mut io: Io) -> anyhow::Result<String> {
    let s = &target.settings;
    let local = s.s("LocalUserName", "");
    let local = if local.is_empty() { std::env::var("USERNAME").unwrap_or_default() } else { local };
    let remote = match target.user.clone() {
        Some(u) => u,
        None => io.read_line("rlogin username: ", true).await.ok_or_else(|| anyhow::anyhow!("Login cancelled."))?,
    };
    let speed = s.s("TerminalSpeed", "38400,38400");
    let speed = speed.split(',').next().unwrap_or("38400");
    conn.write_all(&startup(&local, &remote, &s.s("TerminalType", "xterm"), speed)).await?;

    io.logged_in();
    let mut first = true;
    let mut buf = [0u8; 8192];
    loop {
        tokio::select! {
            n = conn.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    return Ok("Connection closed by remote host.".into());
                }
                let mut data = &buf[..n];
                if first {
                    // the server's single zero byte says it accepted the login
                    first = false;
                    if data[0] == 0 {
                        data = &data[1..];
                    }
                    conn.write_all(&window_size(io.cols, io.rows)).await?;
                }
                if !data.is_empty() {
                    (io.events)(Event::Output(data.to_vec()));
                }
            }
            cmd = io.commands.recv() => match cmd {
                None | Some(Command::Close) => return Ok("Session closed.".into()),
                // answers reach here as Input, once the session is ready
                Some(Command::Reply(_) | Command::Special(_)) => {}
                Some(Command::Input(b)) => conn.write_all(&b).await?,
                Some(Command::Resize { cols, rows }) => {
                    (io.cols, io.rows) = (cols, rows);
                    if !first {
                        conn.write_all(&window_size(cols, rows)).await?;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_has_rfc1282_shape() {
        assert_eq!(startup("me", "you", "xterm", "38400"), b"\0me\0you\0xterm/38400\0");
    }

    #[test]
    fn window_size_is_big_endian() {
        assert_eq!(window_size(80, 24), [0xff, 0xff, b's', b's', 0, 24, 0, 80, 0, 0, 0, 0]);
    }
}
