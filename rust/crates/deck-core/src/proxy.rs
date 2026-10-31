//! Reaching a host, directly or through a proxy. Covers PuTTY's proxy types:
//! SOCKS 4 and 5, HTTP CONNECT, telnet, a local command, and SSH.

use crate::config::Target;
use crate::hostkeys::HostKeyStore;
use crate::session::Io;
use crate::settings::Settings;
use md5::Md5;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, SocketAddr};
use std::os::windows::process::CommandExt;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// The most an HTTP proxy's answer can be.
const MAX_HEADER_LINE: u64 = 8192;
const MAX_HEADERS: usize = 256;

pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub(crate) type Conn = Box<dyn Stream>;

/// Set by the Rlogin session on its own copy of the settings.
pub(crate) const PRIVILEGED: &str = "DeckPrivilegedPort";

const SOCKS4: u32 = 1;
const SOCKS5: u32 = 2;
const HTTP: u32 = 3;
const TELNET: u32 = 4;
const CMD: u32 = 5;
const SSH_TCPIP: u32 = 6;
const SSH_EXEC: u32 = 7;
const SSH_SUBSYSTEM: u32 = 8;

/// The host's addresses, limited by `AddressFamily`: 1 is IPv4, 2 is IPv6.
async fn resolve(host: &str, port: u16, s: &Settings) -> anyhow::Result<Vec<SocketAddr>> {
    let family = s.i("AddressFamily", 0);
    let found = tokio::net::lookup_host((host, port)).await.map_err(|e| anyhow::anyhow!("Could not find {host}: {e}"))?;
    let all: Vec<_> = found.filter(|a| family == 0 || (family == 1) == a.is_ipv4()).collect();
    anyhow::ensure!(!all.is_empty(), "{host} has no address of the kind this session is set to use.");
    Ok(all)
}

/// Rlogin servers want the client on a port below 1024. This tries each one,
/// as PuTTY does, and a port that can't be had just moves on to the next.
async fn privileged(addresses: &[SocketAddr]) -> Option<TcpStream> {
    for local in (512..1024u16).rev() {
        let target = *addresses.first()?;
        let socket = if target.is_ipv4() { TcpSocket::new_v4() } else { TcpSocket::new_v6() }.ok()?;
        let any: IpAddr = if target.is_ipv4() { "0.0.0.0" } else { "::" }.parse().ok()?;
        if socket.bind(SocketAddr::new(any, local)).is_ok() {
            match tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(target)).await {
                Ok(Ok(s)) => return Some(s),
                // this port is taken; any other failure isn't about the port
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::AddrInUse => {}
                _ => return None,
            }
        }
    }
    None
}

async fn tcp(host: &str, port: u16, s: &Settings) -> anyhow::Result<TcpStream> {
    let addresses = resolve(host, port, s).await?;
    if s.b(PRIVILEGED, false) {
        if let Some(stream) = privileged(&addresses).await {
            return Ok(stream);
        }
    }
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addresses[..]))
        .await
        .map_err(|_| anyhow::anyhow!("Timed out connecting to {host}."))?
        .map_err(|e| anyhow::anyhow!("Could not connect to {host}: {e}"))?;
    stream.set_nodelay(s.b("TCPNoDelay", true)).ok();
    if s.b("TCPKeepalives", false) {
        socket2::SockRef::from(&stream).set_keepalive(true).ok();
    }
    Ok(stream)
}

/// `ProxyExcludeList` entries are separated by commas or spaces. `*` wildcards
/// come first in a name, as in `*.example.org`; a `/n` entry is a network.
pub fn excluded(list: &str, host: &str) -> bool {
    list.split([',', ' ']).filter(|e| !e.is_empty()).any(|entry| {
        if let Some((net, bits)) = entry.split_once('/') {
            let (Ok(net), Ok(bits), Ok(ip)) = (net.parse::<IpAddr>(), bits.parse::<u32>(), host.parse::<IpAddr>()) else {
                return false;
            };
            return match (net, ip) {
                (IpAddr::V4(n), IpAddr::V4(i)) if bits <= 32 => {
                    let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
                    u32::from(n) & mask == u32::from(i) & mask
                }
                _ => false,
            };
        }
        match entry.strip_prefix('*') {
            Some(tail) => host.to_lowercase().ends_with(&tail.to_lowercase()),
            None => entry.eq_ignore_ascii_case(host),
        }
    })
}

fn is_local(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Expands `%host`, `%port`, `%user`, `%pass`, `%proxyhost`, `%proxyport` and
/// `%%`, then `\n`, `\r`, `\t`, `\\` and `\xNN`.
pub fn expand_command(template: &str, target: &Target, pass: &str) -> Vec<u8> {
    let s = &target.settings;
    let vars = [
        ("%proxyhost", s.s("ProxyHost", "proxy")),
        ("%proxyport", s.i("ProxyPort", 80).to_string()),
        ("%host", target.host.clone()),
        ("%port", target.port.to_string()),
        ("%user", s.s("ProxyUsername", "")),
        ("%pass", pass.to_string()),
        ("%%", "%".to_string()),
    ];
    let mut out = Vec::new();
    let mut rest = template;
    'outer: while let Some(c) = rest.chars().next() {
        if c == '%' {
            for (name, value) in &vars {
                if let Some(tail) = rest.strip_prefix(name) {
                    out.extend_from_slice(value.as_bytes());
                    rest = tail;
                    continue 'outer;
                }
            }
        }
        if c == '\\' {
            let mut it = rest[1..].chars();
            let escaped = match it.next() {
                Some('n') => Some((b'\n', 2)),
                Some('r') => Some((b'\r', 2)),
                Some('t') => Some((b'\t', 2)),
                Some('\\') => Some((b'\\', 2)),
                Some('%') => Some((b'%', 2)),
                Some('x') => rest.get(2..4).and_then(|h| u8::from_str_radix(h, 16).ok()).map(|b| (b, 4)),
                _ => None,
            };
            if let Some((b, len)) = escaped {
                out.push(b);
                rest = &rest[len..];
                continue;
            }
        }
        out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        rest = &rest[c.len_utf8()..];
    }
    out
}

pub(crate) fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let n = chunk.iter().fold(0u32, |a, &b| a << 8 | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

async fn socks4(c: &mut TcpStream, target: &Target, user: &str) -> anyhow::Result<()> {
    let mut msg = vec![4, 1];
    msg.extend(target.port.to_be_bytes());
    let v4 = target.host.parse::<std::net::Ipv4Addr>().ok();
    // SOCKS 4a: 0.0.0.1 means the proxy resolves the name after the user
    msg.extend(v4.map_or([0, 0, 0, 1], |ip| ip.octets()));
    msg.extend(user.as_bytes());
    msg.push(0);
    if v4.is_none() {
        msg.extend(target.host.as_bytes());
        msg.push(0);
    }
    c.write_all(&msg).await?;
    let mut reply = [0u8; 8];
    c.read_exact(&mut reply).await?;
    match reply[1] {
        90 => Ok(()),
        n => anyhow::bail!("The SOCKS 4 proxy refused the connection (code {n})."),
    }
}

async fn socks5(c: &mut TcpStream, target: &Target, user: &str, pass: &str) -> anyhow::Result<()> {
    // SOCKS 5 has one byte for each of these lengths
    anyhow::ensure!(user.len() <= 255 && pass.len() <= 255, "SOCKS 5 can't carry a user name or password over 255 bytes.");
    anyhow::ensure!(target.host.len() <= 255, "SOCKS 5 can't carry a host name over 255 bytes.");
    let methods: &[u8] = if user.is_empty() { &[5, 1, 0] } else { &[5, 2, 0, 2] };
    c.write_all(methods).await?;
    let mut hello = [0u8; 2];
    c.read_exact(&mut hello).await?;
    match hello {
        [5, 0] => {}
        [5, 2] => {
            let mut msg = vec![1, user.len() as u8];
            msg.extend(user.as_bytes());
            msg.push(pass.len() as u8);
            msg.extend(pass.as_bytes());
            c.write_all(&msg).await?;
            let mut status = [0u8; 2];
            c.read_exact(&mut status).await?;
            anyhow::ensure!(status[1] == 0, "The SOCKS 5 proxy rejected the username or password.");
        }
        _ => anyhow::bail!("The SOCKS 5 proxy offers no login method RustuTTY supports."),
    }
    let mut msg = vec![5, 1, 0];
    match target.host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            msg.push(1);
            msg.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            msg.push(4);
            msg.extend(ip.octets());
        }
        Err(_) => {
            msg.extend([3, target.host.len() as u8]);
            msg.extend(target.host.as_bytes());
        }
    }
    msg.extend(target.port.to_be_bytes());
    c.write_all(&msg).await?;
    let mut head = [0u8; 4];
    c.read_exact(&mut head).await?;
    anyhow::ensure!(head[1] == 0, "The SOCKS 5 proxy refused the connection (code {}).", head[1]);
    let bound = match head[3] {
        1 => 4,
        4 => 16,
        3 => c.read_u8().await? as usize,
        n => anyhow::bail!("The SOCKS 5 proxy sent an unknown address type {n}."),
    };
    let mut skip = vec![0u8; bound + 2];
    c.read_exact(&mut skip).await?;
    Ok(())
}

/// One value from a `Proxy-Authenticate: Digest` header, quoted or not.
fn digest_field(header: &str, name: &str) -> Option<String> {
    // ASCII only: other lowercasing can change the length, and so the offsets
    let lower = header.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&format!("{name}=")) {
        let at = from + i;
        let starts = at == 0 || !lower.as_bytes()[at - 1].is_ascii_alphanumeric();
        let rest = &header[at + name.len() + 1..];
        if starts {
            return Some(match rest.strip_prefix('"') {
                Some(q) => q.split('"').next().unwrap_or("").to_string(),
                None => rest.split([',', ' ']).next().unwrap_or("").trim().to_string(),
            });
        }
        from = at + name.len();
    }
    None
}

/// The `Proxy-Authorization` header for a Digest challenge (RFC 7616).
pub fn digest_answer(challenge: &str, user: &str, pass: &str, uri: &str, cnonce: &str) -> anyhow::Result<String> {
    let realm = digest_field(challenge, "realm").unwrap_or_default();
    let nonce = digest_field(challenge, "nonce").ok_or_else(|| anyhow::anyhow!("The HTTP proxy's challenge has no nonce."))?;
    let algorithm = digest_field(challenge, "algorithm").unwrap_or_else(|| "MD5".to_string());
    let hash: fn(&str) -> String = match algorithm.to_uppercase().as_str() {
        "MD5" => |s| Md5::digest(s).iter().map(|b| format!("{b:02x}")).collect(),
        "SHA-256" => |s| Sha256::digest(s).iter().map(|b| format!("{b:02x}")).collect(),
        a => anyhow::bail!("The HTTP proxy wants Digest with {a}, which isn't supported."),
    };
    let ha1 = hash(&format!("{user}:{realm}:{pass}"));
    let ha2 = hash(&format!("CONNECT:{uri}"));
    let qop = digest_field(challenge, "qop").is_some_and(|q| q.split([',', ' ']).any(|v| v == "auth"));
    let mut header = format!("Digest username=\"{user}\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", algorithm={algorithm}");
    let response = if qop {
        header += &format!(", qop=auth, nc=00000001, cnonce=\"{cnonce}\"");
        hash(&format!("{ha1}:{nonce}:00000001:{cnonce}:auth:{ha2}"))
    } else {
        hash(&format!("{ha1}:{nonce}:{ha2}"))
    };
    header += &format!(", response=\"{response}\"");
    if let Some(opaque) = digest_field(challenge, "opaque") {
        header += &format!(", opaque=\"{opaque}\"");
    }
    Ok(header)
}

/// Reads one line of a proxy's answer, which can't be endless.
async fn header_line(reader: &mut BufReader<TcpStream>, line: &mut String) -> anyhow::Result<usize> {
    line.clear();
    let n = (&mut *reader).take(MAX_HEADER_LINE).read_line(line).await?;
    anyhow::ensure!(n == 0 || line.ends_with('\n'), "The HTTP proxy sent an overlong line.");
    Ok(n)
}

/// Sends a CONNECT. Returns the status code, any Digest or Basic challenge
/// seen, and the stream.
async fn http_try(c: TcpStream, authority: &str, auth: Option<&str>) -> anyhow::Result<(String, Option<String>, BufReader<TcpStream>)> {
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(a) = auth {
        req += &format!("Proxy-Authorization: {a}\r\n");
    }
    req += "\r\n";
    let mut reader = BufReader::new(c);
    reader.get_mut().write_all(req.as_bytes()).await?;
    let mut line = String::new();
    header_line(&mut reader, &mut line).await?;
    let status = line.split_whitespace().nth(1).unwrap_or("").to_string();
    let first = crate::session::clean(line.trim());
    let mut challenge: Option<String> = None;
    for count in 0.. {
        anyhow::ensure!(count < MAX_HEADERS, "The HTTP proxy sent too many headers.");
        if header_line(&mut reader, &mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("proxy-authenticate") {
                let v = v.trim().to_string();
                // Digest is preferred: Basic would show the password to the network
                if challenge.is_none() || v.to_lowercase().starts_with("digest") {
                    challenge = Some(v);
                }
            }
        }
    }
    let challenge = if status.starts_with('2') { None } else { challenge.or(Some(String::new())).filter(|c| !c.is_empty() || status == "407") };
    let status = if status.starts_with('2') || status == "407" { status } else { first };
    Ok((status, challenge, reader))
}

/// PuTTY's order: ask with no login, then answer the proxy's challenge, which
/// takes a fresh connection as a proxy may close the first.
async fn http(host: &str, port: u16, s: &Settings, target: &Target, user: &str, pass: &str, io: &Arc<Mutex<Io>>) -> anyhow::Result<TcpStream> {
    let name = if target.host.contains(':') { format!("[{}]", target.host) } else { target.host.clone() };
    let authority = format!("{name}:{}", target.port);
    let first = http_try(tcp(host, port, s).await?, &authority, None);
    let (status, challenge, mut reader) = tokio::time::timeout(CONNECT_TIMEOUT, first).await.map_err(|_| anyhow::anyhow!("Timed out waiting for the HTTP proxy."))??;
    if status != "407" {
        anyhow::ensure!(status.starts_with('2'), "The HTTP proxy refused the connection: {status}");
    } else {
        anyhow::ensure!(!user.is_empty(), "The HTTP proxy wants a login, and this session has no proxy user name.");
        let challenge = challenge.unwrap_or_default();
        let scheme = challenge.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        let auth = if scheme == "digest" {
            let mut random = [0u8; 16];
            crate::ppk::random(&mut random)?;
            let cnonce: String = random.iter().map(|b| format!("{b:02x}")).collect();
            digest_answer(&challenge, user, pass, &authority, &cnonce)?
        } else if scheme == "basic" {
            io.lock().await.say("Warning: the HTTP proxy uses Basic login, which doesn't protect the proxy password on the network.\n");
            format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()))
        } else {
            anyhow::bail!("The HTTP proxy didn't offer a Basic or Digest login, so the password wasn't sent.");
        };
        let second = http_try(tcp(host, port, s).await?, &authority, Some(&auth));
        let (status, _, second) = tokio::time::timeout(CONNECT_TIMEOUT, second).await.map_err(|_| anyhow::anyhow!("Timed out waiting for the HTTP proxy."))??;
        anyhow::ensure!(status.starts_with('2'), "The HTTP proxy refused the connection: {status}");
        reader = second;
    }
    anyhow::ensure!(reader.buffer().is_empty(), "The HTTP proxy sent data before the connection was ready.");
    Ok(reader.into_inner())
}

/// Refuses a value that cmd.exe would read as more than text, when it's going
/// into a command. Quoting can't be relied on there, as cmd has no complete way
/// to do it.
fn safe_in_command(template: &str, what: &str, name: &str, value: &str) -> anyhow::Result<()> {
    let risky = template.contains(what) && value.contains(['&', '|', '<', '>', '^', '"', '%', '(', ')', '\r', '\n']);
    anyhow::ensure!(!risky, "The {name} has a character that isn't safe in a command, so the command wasn't run.");
    Ok(())
}

fn command_is_safe(template: &str, target: &Target, pass: &str) -> anyhow::Result<()> {
    safe_in_command(template, "%host", "host name", &target.host)?;
    safe_in_command(template, "%user", "proxy user name", &target.settings.s("ProxyUsername", ""))?;
    safe_in_command(template, "%pass", "proxy password", pass)
}

/// A command line for cmd.exe. It's passed untouched: the usual quoting of
/// arguments would turn the line's own quotes into something cmd can't read.
pub(crate) fn shell(line: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("cmd.exe");
    c.as_std_mut().raw_arg(format!("/c {line}"));
    c
}

/// Spawns `ProxyTelnetCommand` locally, as the "Local" proxy type does.
fn command(line: &[u8]) -> anyhow::Result<Conn> {
    let line = String::from_utf8_lossy(line).trim().to_string();
    let mut child = shell(&line)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| anyhow::anyhow!("Could not start the proxy command: {e}"))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        anyhow::bail!("Could not connect to the proxy command.");
    };
    Ok(Box::new(Child { _child: child, io: tokio::io::join(stdout, stdin) }))
}

struct Child {
    _child: tokio::process::Child,
    io: tokio::io::Join<tokio::process::ChildStdout, tokio::process::ChildStdin>,
}

impl AsyncRead for Child {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Child {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// `PreConnectCommand` runs to its end before connecting. As in PuTTY, its exit
/// status doesn't stop the connection, and it can't be given a password.
async fn pre_connect(target: &Target, io: &Arc<Mutex<Io>>) -> anyhow::Result<()> {
    let template = target.settings.s("PreConnectCommand", "");
    if template.is_empty() {
        return Ok(());
    }
    if template.contains("%user") || template.contains("%pass") {
        io.lock().await.say("The pre-connect command can't use %user or %pass, so it wasn't run.\n");
        return Ok(());
    }
    command_is_safe(&template, target, "")?;
    let line = expand_command(&template, target, "");
    let line = String::from_utf8_lossy(&line).trim().to_string();
    let run = shell(&line)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .await;
    if let Err(e) = run {
        io.lock().await.say(&format!("The pre-connect command didn't start: {e}\n"));
    }
    Ok(())
}

/// Connects to `target`, going through its proxy if it has one that applies.
pub(crate) async fn connect(target: &Target, store: &Arc<dyn HostKeyStore>, io: &Arc<Mutex<Io>>) -> anyhow::Result<Conn> {
    let s = &target.settings;
    pre_connect(target, io).await?;
    let method = s.i("ProxyMethod", 0);
    let skip = method == 0
        || (is_local(&target.host) && !s.b("ProxyLocalhost", false))
        || excluded(&s.s("ProxyExcludeList", ""), &target.host);
    if skip {
        return Ok(Box::new(tcp(&target.host, target.port, s).await?));
    }
    let user = s.s("ProxyUsername", "");
    let mut pass = Zeroizing::new(s.s("ProxyPassword", ""));
    if !user.is_empty() && pass.is_empty() && matches!(method, SOCKS5 | HTTP | TELNET) {
        *pass = io.lock().await.read_line("Proxy password: ", false).await.ok_or_else(|| anyhow::anyhow!("Login cancelled."))?;
    }
    let (proxy_host, proxy_port) = (s.s("ProxyHost", "proxy"), s.i("ProxyPort", 80).min(65535) as u16);
    // ProxyDNS "No": look the name up here, and give the proxy an address
    let mut local_dns;
    let target = if s.i("ProxyDNS", 1) == 0 && matches!(method, SOCKS4 | SOCKS5 | HTTP) {
        local_dns = target.clone();
        local_dns.host = resolve(&target.host, target.port, s).await?[0].ip().to_string();
        &local_dns
    } else {
        target
    };
    match method {
        SOCKS4 => {
            let mut c = tcp(&proxy_host, proxy_port, s).await?;
            socks4(&mut c, target, &user).await?;
            Ok(Box::new(c))
        }
        SOCKS5 => {
            let mut c = tcp(&proxy_host, proxy_port, s).await?;
            socks5(&mut c, target, &user, &pass).await?;
            Ok(Box::new(c))
        }
        HTTP => Ok(Box::new(http(&proxy_host, proxy_port, s, target, &user, &pass, io).await?)),
        TELNET => {
            let mut c = tcp(&proxy_host, proxy_port, s).await?;
            let line = expand_command(&s.s("ProxyTelnetCommand", "connect %host %port\\n"), target, &pass);
            c.write_all(&line).await?;
            Ok(Box::new(c))
        }
        CMD => {
            let template = s.s("ProxyTelnetCommand", "");
            command_is_safe(&template, target, &pass)?;
            command(&expand_command(&template, target, &pass))
        }
        SSH_TCPIP | SSH_EXEC | SSH_SUBSYSTEM => {
            let command = expand_command(&s.s("ProxyTelnetCommand", ""), target, &pass);
            let command = String::from_utf8_lossy(&command).trim().to_string();
            let tunnel = match method {
                SSH_TCPIP => crate::ssh::Tunnel::Tcp(target.host.clone(), target.port),
                SSH_EXEC => crate::ssh::Tunnel::Exec(command),
                _ => crate::ssh::Tunnel::Subsystem(command),
            };
            let mut jump = crate::config::named_or_host(&proxy_host, proxy_port, &user);
            jump.settings.set("ProxyPassword", crate::settings::Value::Str(String::new()));
            crate::ssh::tunnel(jump, tunnel, store.clone(), io.clone()).await
        }
        n => anyhow::bail!("Unknown proxy type {n}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Protocol, Target};

    #[test]
    fn exclude_list_matches_names_and_networks() {
        assert!(excluded("*.example.org, 10.0.0.0/8", "Box.Example.org"));
        assert!(excluded("*.example.org 10.0.0.0/8", "10.2.3.4"));
        assert!(excluded("exact", "EXACT"));
        assert!(!excluded("*.example.org,10.0.0.0/8", "example.com"));
        assert!(!excluded("10.0.0.0/8", "11.0.0.1"));
        assert!(!excluded("", "anything"));
    }

    #[test]
    fn commands_expand_like_putty() {
        let t = Target::new(Protocol::Ssh, "dest", 22);
        assert_eq!(expand_command("connect %host %port\\n", &t, ""), b"connect dest 22\n");
        assert_eq!(expand_command("100%% \\x41\\\\", &t, ""), b"100% A\\");
        assert_eq!(expand_command("%unknown", &t, ""), b"%unknown");
    }

    #[test]
    fn digest_matches_a_worked_example() {
        let challenge = r#"Digest realm="r", nonce="n", qop="auth,auth-int", opaque="o""#;
        let a = digest_answer(challenge, "u", "p", "h:22", "c").unwrap();
        assert!(a.contains(r#"username="u""#) && a.contains("qop=auth,") && a.contains("nc=00000001") && a.contains(r#"opaque="o""#), "{a}");
        // HA1 = MD5(u:r:p), HA2 = MD5(CONNECT:h:22), worked out independently
        let hex = |s: &str| -> String { Md5::digest(s).iter().map(|b| format!("{b:02x}")).collect() };
        let want = hex(&format!("{}:n:00000001:c:auth:{}", hex("u:r:p"), hex("CONNECT:h:22")));
        assert!(a.contains(&format!(r#"response="{want}""#)), "{a}");
        assert!(digest_answer(r#"Digest nonce="n", algorithm=SHA-512-256"#, "u", "p", "h", "c").is_err());
    }

    #[test]
    fn a_digest_header_that_changes_length_when_lowercased_is_harmless() {
        let evil = "Digest realm=\"\u{130}\u{130}\u{130}\u{130}\", nonce=\"\u{212a}\u{212a}\"";
        assert_eq!(digest_field(evil, "nonce").as_deref(), Some("\u{212a}\u{212a}"));
        assert_eq!(digest_field(&format!("{}nonce=x", "\u{130}".repeat(40)), "nonce").as_deref(), Some("x"));
    }

    #[test]
    fn unsafe_values_stay_out_of_commands() {
        let mut t = Target::new(Protocol::Ssh, "a&calc", 22);
        assert!(command_is_safe("nc %host %port", &t, "").is_err());
        assert!(command_is_safe("nc fixed.example 22", &t, "").is_ok());
        t.host = "good.example".into();
        assert!(command_is_safe("nc %host %port", &t, "").is_ok());
        assert!(command_is_safe("connect %pass", &t, "a&b").is_err());
    }

    #[test]
    fn digest_fields_are_not_confused_by_longer_names() {
        assert_eq!(digest_field(r#"Digest cnonce="x", nonce="y""#, "nonce").as_deref(), Some("y"));
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn loopback_is_local() {
        assert!(is_local("localhost") && is_local("127.0.0.1") && is_local("::1"));
        assert!(!is_local("example.org"));
    }
}
