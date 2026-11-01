//! SSH sessions end to end, against a real server on localhost.

#[path = "support/server.rs"]
mod server;

use deck_core::config::{Protocol, Target};
use deck_core::hostkeys::{self, MemoryStore};
use deck_core::session::{spawn, Command, Event};
use deck_core::settings::Value;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::Algorithm;
use russh::MethodKind;
use server::{new_key, PASSWORD, USER};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

struct Client {
    tx: UnboundedSender<Command>,
    rx: UnboundedReceiver<Event>,
    output: String,
    closed: Option<String>,
}

impl Client {
    fn open(port: u16, user: Option<&str>, store: Arc<MemoryStore>) -> Client {
        let mut target = Target::new(Protocol::Ssh, "127.0.0.1", port);
        target.user = user.map(str::to_string);
        Client::open_target(target, store)
    }

    fn open_target(target: Target, store: Arc<MemoryStore>) -> Client {
        let (etx, rx) = unbounded_channel();
        let sink = Arc::new(move |e| {
            let _ = etx.send(e);
        });
        let tx = spawn(&tokio::runtime::Handle::current(), target, store, (80, 24), sink);
        Client { tx, rx, output: String::new(), closed: None }
    }

    async fn pump(&mut self) -> bool {
        match self.rx.recv().await {
            Some(Event::Output(b) | Event::Local(b)) => self.output.push_str(&String::from_utf8_lossy(&b)),
            Some(Event::Closed(why)) => self.closed = Some(why),
            None => return false,
        }
        true
    }

    async fn expect(&mut self, text: &str) {
        let got = tokio::time::timeout(Duration::from_secs(20), async {
            while !self.output.contains(text) && self.closed.is_none() {
                if !self.pump().await {
                    break;
                }
            }
        })
        .await;
        assert!(got.is_ok() && self.output.contains(text), "waiting for {text:?}, have {:?} closed {:?}", self.output, self.closed);
    }

    async fn closed(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(20), async {
            while self.closed.is_none() && self.pump().await {}
        })
        .await
        .expect("session should close");
        self.closed.clone().unwrap()
    }

    fn send(&self, text: &str) {
        self.tx.send(Command::Input(text.as_bytes().to_vec())).unwrap();
    }

    async fn login(&mut self) {
        self.expect("Trust this host").await;
        self.send("y\r");
        self.expect("password: ").await;
        self.send(&format!("{PASSWORD}\r"));
        self.expect("welcome").await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn logs_in_and_runs_a_session() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    let mut c = Client::open(port, Some(USER), store.clone());
    c.login().await;
    assert!(!c.output.contains(PASSWORD), "a password must not be echoed");

    let pty = seen.lock().unwrap().pty.clone();
    assert_eq!(pty, Some(("xterm".to_string(), 80, 24)));

    c.send("hi there");
    c.expect("hi there").await;

    c.tx.send(Command::Resize { cols: 100, rows: 30 }).unwrap();
    for _ in 0..200 {
        if !seen.lock().unwrap().resizes.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(seen.lock().unwrap().resizes, vec![(100, 30)]);

    c.send("\rexit\r");
    assert!(c.closed().await.contains("exit status 3"));
}

#[tokio::test(flavor = "multi_thread")]
async fn accepted_key_is_trusted_next_time() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    let mut c = Client::open(port, Some(USER), store.clone());
    c.login().await;
    assert_eq!(store.deck.lock().unwrap().len(), 1);

    let mut c = Client::open(port, Some(USER), store);
    c.expect("password: ").await;
    assert!(!c.output.contains("Trust this host"), "{:?}", c.output);
}

#[tokio::test(flavor = "multi_thread")]
async fn declining_a_new_key_sends_nothing() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    let mut c = Client::open(port, Some(USER), store.clone());
    c.expect("Trust this host").await;
    c.send("n\r");
    c.closed().await;
    assert!(seen.lock().unwrap().passwords.is_empty());
    assert!(store.deck.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_key_needs_the_whole_word_yes() {
    let key = new_key(Algorithm::Ed25519);
    let store = Arc::new(MemoryStore::default());
    let (port, seen) = server::start(key, vec![MethodKind::Password]).await;

    let other = new_key(Algorithm::Ed25519);
    hostkeys::remember(store.as_ref(), "127.0.0.1", port, other.public_key()).unwrap();

    let mut c = Client::open(port, Some(USER), store.clone());
    c.expect("HAS CHANGED").await;
    c.send("y\r");
    c.closed().await;
    assert!(seen.lock().unwrap().passwords.is_empty(), "no password may reach a changed host");

    let mut c = Client::open(port, Some(USER), store);
    c.expect("HAS CHANGED").await;
    c.send("yes\r");
    c.expect("password: ").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn rsa_key_stored_by_putty_is_trusted_without_asking() {
    let key = new_key(Algorithm::Rsa { hash: None });
    let KeyData::Rsa(rsa) = key.public_key().key_data().clone() else { panic!() };
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>().trim_start_matches('0').to_string();
    let value = format!("0x{},0x{}", hex(rsa.e().as_bytes()), hex(rsa.n().as_bytes()));

    let (port, _) = server::start(key, vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    store.putty.lock().unwrap().insert(format!("rsa2@{port}:127.0.0.1"), value);

    let mut c = Client::open(port, Some(USER), store.clone());
    c.expect("password: ").await;
    assert!(!c.output.contains("Trust this host"), "{:?}", c.output);
    assert!(store.deck.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn asks_for_a_user_name_like_putty() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open(port, None, Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("login as: ").await;
    c.send(&format!("{USER}\r"));
    c.expect(&format!("{USER}@127.0.0.1's password: ")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn three_wrong_passwords_end_the_session() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open(port, Some(USER), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    for i in 1..=3 {
        tokio::time::timeout(Duration::from_secs(20), async {
            while c.output.matches("password: ").count() < i && c.closed.is_none() {
                c.pump().await;
            }
        })
        .await
        .expect("password prompt");
        assert!(c.closed.is_none(), "closed early: {:?} after {:?}", c.closed, c.output);
        c.send("wrong\r");
    }
    assert!(c.closed().await.contains("Too many"));
    assert_eq!(seen.lock().unwrap().passwords.len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn keyboard_interactive_login() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::KeyboardInteractive]).await;
    let mut c = Client::open(port, Some(USER), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("Two-step login").await;
    c.expect("Code: ").await;
    c.send(&format!("{PASSWORD}\r"));
    c.expect("welcome").await;
    assert!(!c.output.contains(PASSWORD));
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_at_a_prompt_ends_the_session() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open(port, Some(USER), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.tx.send(Command::Close).unwrap();
    c.closed().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_target_never_connects() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut target = Target::new(Protocol::Ssh, "127.0.0.1", port);
    target.refuse = Some("this saved session uses a proxy".into());
    let (etx, mut rx) = unbounded_channel();
    let sink = Arc::new(move |e| {
        let _ = etx.send(e);
    });
    let _tx = spawn(&tokio::runtime::Handle::current(), target, Arc::new(MemoryStore::default()), (80, 24), sink);
    let e = rx.recv().await.unwrap();
    assert!(matches!(e, Event::Closed(ref s) if s.contains("proxy")), "{e:?}");
    let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(accepted.is_err(), "nothing may connect");
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_listening_is_reported() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut c = Client::open(port, Some(USER), Arc::new(MemoryStore::default()));
    assert!(c.closed().await.contains("Could not connect"));
}

fn user_target(port: u16, pairs: &[(&str, Value)]) -> Target {
    let mut target = Target::new(Protocol::Ssh, "127.0.0.1", port);
    target.user = Some(USER.to_string());
    for (k, v) in pairs {
        target.settings.set(k, v.clone());
    }
    target
}

fn text(s: &str) -> Value {
    Value::Str(s.to_string())
}

async fn free_port() -> u16 {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    l.local_addr().unwrap().port()
}

/// A server that answers each connection with `upper` of what it gets.
async fn upper_server() -> u16 {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                while let Ok(n) = c.read(&mut buf).await {
                    if n == 0 || c.write_all(&buf[..n].to_ascii_uppercase()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

async fn round_trip(c: &mut TcpStream) -> Vec<u8> {
    c.write_all(b"ping").await.unwrap();
    let mut got = [0u8; 4];
    tokio::time::timeout(Duration::from_secs(10), c.read_exact(&mut got)).await.expect("no reply").unwrap();
    got.to_vec()
}

async fn connect_retry(port: u16) -> TcpStream {
    for _ in 0..100 {
        if let Ok(c) = TcpStream::connect(("127.0.0.1", port)).await {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("nothing listening on {port}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_banner_does_not_hang_the_login() {
    let methods = vec![MethodKind::Password];
    let (port, _) = server::start_with_banner(new_key(Algorithm::Ed25519), methods, Some("Authorised users only\r\n")).await;
    let mut c = Client::open(port, Some(USER), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("password: ").await;
    c.send(&format!("{PASSWORD}\r"));
    c.expect("welcome").await;
    assert!(c.output.contains("Authorised users only"));
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_a_remote_command_with_environment() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let target = user_target(port, &[("RemoteCommand", text("uptime")), ("Environment", text("LANG=C")), ("TerminalType", text("vt220"))]);
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("password: ").await;
    c.send(&format!("{PASSWORD}\r"));
    c.expect("ran it").await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.exec, ["uptime"]);
    assert_eq!(seen.env, [("LANG".to_string(), "C".to_string())]);
    assert_eq!(seen.pty.as_ref().unwrap().0, "vt220");
}

#[tokio::test(flavor = "multi_thread")]
async fn local_forward_reaches_the_far_side() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let (dest, local) = (upper_server().await, free_port().await);
    let fwd = format!("L{local}=127.0.0.1:{dest}");
    let mut c = Client::open_target(user_target(port, &[("PortForwardings", text(&fwd))]), Arc::new(MemoryStore::default()));
    c.login().await;
    assert_eq!(round_trip(&mut connect_retry(local).await).await, b"PING");
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_forward_reaches_this_side() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let (dest, remote) = (upper_server().await, free_port().await);
    let fwd = format!("R{remote}=127.0.0.1:{dest}");
    let mut c = Client::open_target(user_target(port, &[("PortForwardings", text(&fwd))]), Arc::new(MemoryStore::default()));
    c.login().await;
    assert_eq!(round_trip(&mut connect_retry(remote).await).await, b"PING");
}

#[tokio::test(flavor = "multi_thread")]
async fn dynamic_forward_is_a_socks_proxy_a_session_can_use() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let (dest, socks) = (upper_server().await, free_port().await);
    let store = Arc::new(MemoryStore::default());
    let fwd = format!("D{socks}=");
    let mut first = Client::open_target(user_target(port, &[("PortForwardings", text(&fwd))]), store.clone());
    first.login().await;

    // by hand, as a browser would
    let mut c = connect_retry(socks).await;
    c.write_all(&[5, 1, 0]).await.unwrap();
    let mut hello = [0u8; 2];
    c.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello, [5, 0]);
    let mut req = vec![5, 1, 0, 1, 127, 0, 0, 1];
    req.extend(dest.to_be_bytes());
    c.write_all(&req).await.unwrap();
    let mut reply = [0u8; 10];
    c.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 0);
    assert_eq!(round_trip(&mut c).await, b"PING");

    // and a second session, connecting through the first with PuTTY's settings
    let via = user_target(port, &[
        ("ProxyMethod", Value::Int(2)),
        ("ProxyHost", text("127.0.0.1")),
        ("ProxyPort", Value::Int(socks as u32)),
        ("ProxyLocalhost", Value::Int(1)),
    ]);
    let mut second = Client::open_target(via, store);
    second.expect("password: ").await;
    second.send(&format!("{PASSWORD}\r"));
    second.expect("welcome").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_proxy_tunnels_a_session() {
    let (jump, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let via = user_target(port, &[
        ("ProxyMethod", Value::Int(6)),
        ("ProxyHost", text("127.0.0.1")),
        ("ProxyPort", Value::Int(jump as u32)),
        ("ProxyUsername", text(USER)),
        ("ProxyLocalhost", Value::Int(1)),
    ]);
    let mut c = Client::open_target(via, Arc::new(MemoryStore::default()));
    for _ in 0..2 {
        let before = c.output.len();
        c.expect("Trust this host").await;
        c.send("y\r");
        c.expect("password: ").await;
        c.send(&format!("{PASSWORD}\r"));
        tokio::time::timeout(Duration::from_secs(20), async {
            while c.output.len() == before {
                c.pump().await;
            }
        })
        .await
        .unwrap();
        c.output.clear();
    }
    c.expect("welcome").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_failure_never_falls_back_to_a_direct_connection() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let dead = free_port().await;
    let via = user_target(port, &[
        ("ProxyMethod", Value::Int(2)),
        ("ProxyHost", text("127.0.0.1")),
        ("ProxyPort", Value::Int(dead as u32)),
        ("ProxyLocalhost", Value::Int(1)),
    ]);
    let mut c = Client::open_target(via, Arc::new(MemoryStore::default()));
    assert!(c.closed().await.contains("Could not connect"));
    assert!(seen.lock().unwrap().passwords.is_empty());
}

async fn logs_in(target: Target) -> Client {
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    c.login().await;
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn chosen_algorithms_connect() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let target = user_target(port, &[
        ("Cipher", text("aesgcm,WARN,aes")),
        ("KEX", text("dh-group14-sha1,ecdh,WARN")),
        ("HostKey", text("ed25519,WARN")),
        ("Compression", Value::Int(1)),
    ]);
    logs_in(target).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cipher_list_with_nothing_in_common_is_refused() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let target = user_target(port, &[("Cipher", text("3des,WARN,aes"))]);
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    let why = c.closed().await;
    assert!(why.contains("SSH connection failed"), "{why}");
    assert!(seen.lock().unwrap().passwords.is_empty());
}

/// Renewing the keys presents the host key again while the session holds its
/// terminal. That used to wait on itself for ever.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_carries_on_through_a_rekey() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = logs_in(user_target(port, &[("RekeyBytes", text("2K"))])).await;
    for i in 0..30 {
        let line = format!("{i:03}{}", "x".repeat(400));
        c.send(&line);
        c.expect(&line).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_can_refuse_to_log_in_at_all() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open_target(user_target(port, &[("SshNoAuth", Value::Int(1))]), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    assert!(c.closed().await.contains("bypass"));
    assert!(seen.lock().unwrap().passwords.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh1_only_sessions_are_refused_by_name() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open_target(user_target(port, &[("SshProt", Value::Int(0))]), Arc::new(MemoryStore::default()));
    assert!(c.closed().await.contains("SSH-1"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ipv6_only_refuses_an_ipv4_host() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open_target(user_target(port, &[("AddressFamily", Value::Int(2))]), Arc::new(MemoryStore::default()));
    assert!(c.closed().await.contains("no address"));
}

#[tokio::test(flavor = "multi_thread")]
async fn local_echo_and_editing_on_a_raw_session() {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, mut got) = unbounded_channel();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut b = [0u8; 64];
        while let Ok(n) = s.read(&mut b).await {
            if n == 0 || tx.send(b[..n].to_vec()).is_err() {
                return;
            }
        }
    });
    let mut target = Target::new(Protocol::Raw, "127.0.0.1", port);
    target.settings.set("LocalEcho", Value::Int(2));
    target.settings.set("LocalEdit", Value::Int(2));
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    c.send("abx\x7f");
    c.expect("abx").await;
    assert!(tokio::time::timeout(Duration::from_millis(100), got.recv()).await.is_err(), "nothing leaves until Return");
    c.send("c\r");
    assert_eq!(got.recv().await.unwrap(), b"abc\r");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_can_share_a_connection() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    let share = [("ConnectionSharing", Value::Int(1))];
    let mut first = Client::open_target(user_target(port, &share), store.clone());
    first.login().await;
    let mut second = Client::open_target(user_target(port, &share), store.clone());
    second.expect("Sharing an existing connection").await;
    second.expect("welcome").await;
    assert_eq!(seen.lock().unwrap().passwords.len(), 1, "only one login");

    // the connection stays up for whoever is left
    first.tx.send(Command::Close).unwrap();
    first.closed().await;
    second.send("still here");
    second.expect("still here").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sharing_needs_turning_on() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    let mut first = Client::open_target(user_target(port, &[]), store.clone());
    first.login().await;
    let mut second = Client::open_target(user_target(port, &[]), store);
    second.expect("password: ").await;
    second.send(&format!("{PASSWORD}\r"));
    second.expect("welcome").await;
    assert_eq!(seen.lock().unwrap().passwords.len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_auth_plugin_answers_the_server() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::KeyboardInteractive]).await;
    let plugin = env!("CARGO_MANIFEST_DIR").to_string() + r"\..\..\target\debug\examples\test_auth_plugin.exe";
    let mut c = Client::open_target(user_target(port, &[("AuthPlugin", text(&plugin))]), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("Plugin word: ").await;
    let reversed: String = PASSWORD.chars().rev().collect();
    c.send(&format!("{reversed}\r"));
    c.expect("welcome").await;
    assert_eq!(seen.lock().unwrap().passwords, [PASSWORD]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_user_name_can_be_changed() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut target = user_target(port, &[("ChangeUsername", Value::Int(1))]);
    target.user = Some("wrong".to_string());
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("password: ").await;
    c.send(&format!("{PASSWORD}\r"));
    c.expect("login as: ").await;
    c.send(&format!("{USER}\r"));
    c.output.clear();
    c.expect("password: ").await;
    c.send(&format!("{PASSWORD}\r"));
    c.expect("welcome").await;
}

/// A proxy that wants a Digest login, then tunnels to `dest`.
async fn digest_proxy(dest: u16) -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                while !head.ends_with(b"\r\n\r\n") {
                    let mut b = [0u8; 1];
                    if c.read_exact(&mut b).await.is_err() {
                        return;
                    }
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                log.lock().unwrap().push(head.clone());
                if !head.contains("Proxy-Authorization: Digest") {
                    let answer = "HTTP/1.1 407 Login\r\nProxy-Authenticate: Basic realm=\"r\"\r\nProxy-Authenticate: Digest realm=\"r\", nonce=\"abc\", qop=\"auth\"\r\n\r\n";
                    let _ = c.write_all(answer.as_bytes()).await;
                    return;
                }
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
                let mut far = TcpStream::connect(("127.0.0.1", dest)).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut c, &mut far).await;
            });
        }
    });
    (port, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn digest_proxy_login_never_sends_the_password() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let (proxy, seen) = digest_proxy(port).await;
    let via = user_target(port, &[
        ("ProxyMethod", Value::Int(3)),
        ("ProxyHost", text("127.0.0.1")),
        ("ProxyPort", Value::Int(proxy as u32)),
        ("ProxyUsername", text("px")),
        ("ProxyPassword", text("proxy-secret")),
        ("ProxyLocalhost", Value::Int(1)),
    ]);
    logs_in(via).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one try without a login, one with");
    assert!(!seen[0].contains("Proxy-Authorization"));
    assert!(!seen.iter().any(|h| h.contains("proxy-secret") || h.contains("Basic")), "{seen:?}");
}

/// Sends the host's first bytes in the same write as the end of its headers.
async fn eager_proxy(dest: u16) -> u16 {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut c, _) = l.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            c.read_exact(&mut b).await.unwrap();
            head.push(b[0]);
        }
        let mut far = TcpStream::connect(("127.0.0.1", dest)).await.unwrap();
        let mut banner = vec![0u8; 4096];
        let n = far.read(&mut banner).await.unwrap();
        let mut answer = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        answer.extend_from_slice(&banner[..n]);
        c.write_all(&answer).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut c, &mut far).await;
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn data_arriving_with_the_http_proxy_answer_is_kept() {
    let (port, _) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let proxy = eager_proxy(port).await;
    let via = user_target(port, &[
        ("ProxyMethod", Value::Int(3)),
        ("ProxyHost", text("127.0.0.1")),
        ("ProxyPort", Value::Int(proxy as u32)),
        ("ProxyLocalhost", Value::Int(1)),
    ]);
    logs_in(via).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pre_connect_command_runs_first() {
    let dest = upper_server().await;
    let mark = std::env::temp_dir().join(format!("deck-preconnect-{}", std::process::id()));
    let _ = std::fs::remove_file(&mark);
    let mut target = Target::new(Protocol::Raw, "127.0.0.1", dest);
    target.settings.set("PreConnectCommand", text(&format!("echo %host > \"{}\"", mark.display())));
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.send("abc");
    c.expect("ABC").await;
    assert!(std::fs::read_to_string(&mark).unwrap().contains("127.0.0.1"));
    std::fs::remove_file(&mark).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rlogin_connects_from_a_privileged_port() {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut s, peer) = l.accept().await.unwrap();
        let mut b = [0u8; 64];
        let n = s.read(&mut b).await.unwrap();
        s.write_all(b"\0hello\r\n").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        (peer.port(), b[..n].to_vec())
    });
    let mut target = Target::new(Protocol::Rlogin, "127.0.0.1", port);
    target.user = Some("me".to_string());
    target.settings.set("LocalUserName", text("local"));
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    c.expect("hello").await;
    let (from, start) = server.await.unwrap();
    assert!((512..1024).contains(&from), "came from {from}");
    assert_eq!(start, b"\0local\0me\0xterm/38400\0");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_rlogin_fails_quickly() {
    let dead = free_port().await;
    let mut c = Client::open_target(Target::parse(&format!("rlogin://me@127.0.0.1:{dead}")).unwrap(), Arc::new(MemoryStore::default()));
    let start = std::time::Instant::now();
    assert!(c.closed().await.contains("Could not connect"));
    assert!(start.elapsed() < Duration::from_secs(8));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_outside_the_manual_list_is_refused_without_asking() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let other = new_key(Algorithm::Ed25519).public_key().to_openssh().unwrap();
    let blob = other.split_whitespace().nth(1).unwrap().to_string();
    let store = Arc::new(MemoryStore::default());
    let mut settings = user_target(port, &[]);
    settings.settings.set("SSHManualHostKeys", text(&blob));
    let mut c = Client::open_target(settings, store);
    c.expect("list of keys").await;
    assert!(c.closed().await.contains("SSH connection failed"));
    assert!(!c.output.contains("Trust this host"), "{}", c.output);
    assert!(seen.lock().unwrap().passwords.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_refuses_an_unknown_host_key_instead_of_asking() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open_target(user_target(port, &[(deck_core::session::BATCH, Value::Int(1))]), Arc::new(MemoryStore::default()));
    c.send("y\r");
    c.expect("because of -batch").await;
    c.closed().await;
    assert!(seen.lock().unwrap().passwords.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn terminal_answers_never_reach_a_password_prompt() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut c = Client::open_target(user_target(port, &[]), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("password: ").await;
    c.tx.send(Command::Reply(b"PuTTY".to_vec())).unwrap();
    c.send(&format!("{PASSWORD}\r"));
    c.expect("welcome").await;
    assert_eq!(seen.lock().unwrap().passwords, [PASSWORD]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_banner_cannot_send_escape_sequences() {
    let (port, _) = server::start_with_banner(new_key(Algorithm::Ed25519), vec![MethodKind::Password], Some("hello\x1b[2Jthere\n")).await;
    let mut c = Client::open_target(user_target(port, &[]), Arc::new(MemoryStore::default()));
    c.expect("Trust this host").await;
    c.send("y\r");
    c.expect("hello[2Jthere").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_through_different_proxies_do_not_share() {
    let (port, seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let store = Arc::new(MemoryStore::default());
    let share = [("ConnectionSharing", Value::Int(1))];
    let mut first = Client::open_target(user_target(port, &share), store.clone());
    first.login().await;
    let other = [("ConnectionSharing", Value::Int(1)), ("LogHost", text(&format!("127.0.0.1:{port}"))), ("ProxyHost", text("elsewhere"))];
    let mut second = Client::open_target(user_target(port, &other), store);
    second.expect("password: ").await;
    second.send(&format!("{PASSWORD}\r"));
    second.expect("welcome").await;
    assert_eq!(seen.lock().unwrap().passwords.len(), 2);
}

/// Answers every CONNECT with a bare 407, and records what it was sent.
async fn bare_407_proxy() -> (u16, Arc<std::sync::Mutex<Vec<String>>>) {
    let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut c, _)) = l.accept().await {
            let mut b = vec![0u8; 4096];
            let n = c.read(&mut b).await.unwrap_or(0);
            log.lock().unwrap().push(String::from_utf8_lossy(&b[..n]).into_owned());
            let _ = c.write_all(b"HTTP/1.1 407 Login\r\n\r\n").await;
        }
    });
    (port, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bare_407_gets_no_password() {
    let (proxy, seen) = bare_407_proxy().await;
    let via = user_target(22, &[
        ("ProxyMethod", Value::Int(3)),
        ("ProxyHost", text("127.0.0.1")),
        ("ProxyPort", Value::Int(proxy as u32)),
        ("ProxyUsername", text("px")),
        ("ProxyPassword", text("proxy-secret")),
        ("ProxyLocalhost", Value::Int(1)),
    ]);
    let mut c = Client::open_target(via, Arc::new(MemoryStore::default()));
    assert!(c.closed().await.contains("wasn't sent"));
    assert!(!seen.lock().unwrap().iter().any(|h| h.contains("Authorization")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_proxy_refuses_a_host_that_would_be_a_command() {
    let mut target = Target::new(Protocol::Raw, "a&calc", 1);
    target.settings.set("ProxyMethod", Value::Int(5));
    target.settings.set("ProxyTelnetCommand", text("nc %host %port"));
    let mut c = Client::open_target(target, Arc::new(MemoryStore::default()));
    assert!(c.closed().await.contains("isn't safe"));
}
