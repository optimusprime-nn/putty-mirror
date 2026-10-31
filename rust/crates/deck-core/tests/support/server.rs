//! A small real SSH server on localhost, so sessions can be tested end to end
//! without going near a real host. It echoes input back and exits on "exit".

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Msg, Response, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const USER: &str = "tester";
pub const PASSWORD: &str = "s3cret-pass";

#[derive(Default)]
pub struct Seen {
    pub passwords: Vec<String>,
    pub pty: Option<(String, u32, u32)>,
    pub resizes: Vec<(u32, u32)>,
    pub input: Vec<u8>,
    pub exec: Vec<String>,
    pub env: Vec<(String, String)>,
    pub no_pty_shell: bool,
}

#[derive(Clone)]
pub struct Options {
    pub methods: Vec<MethodKind>,
    pub seen: Arc<Mutex<Seen>>,
    pub banner: Option<&'static str>,
    /// The one client key accepted, or any key when empty and publickey is on.
    pub allowed: Option<russh::keys::PublicKey>,
    pub sftp: bool,
}

struct Conn {
    opts: Options,
    line: Vec<u8>,
    shell: Option<ChannelId>,
    pending: Option<Channel<Msg>>,
}

/// An SFTP server whose files are held in memory.
#[derive(Default)]
struct MemoryFs {
    files: HashMap<String, Vec<u8>>,
    handles: HashMap<String, String>,
    version: u32,
}

impl MemoryFs {
    fn ok(id: u32) -> Status {
        Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en".into() }
    }

    fn attrs(&self, path: &str) -> Result<FileAttributes, StatusCode> {
        let size = self.files.get(path).ok_or(StatusCode::NoSuchFile)?.len();
        Ok(FileAttributes { size: Some(size as u64), ..Default::default() })
    }
}

impl russh_sftp::server::Handler for MemoryFs {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, version: u32, _: HashMap<String, String>) -> Result<Version, Self::Error> {
        self.version = version;
        Ok(Version::new())
    }

    async fn open(&mut self, id: u32, path: String, flags: OpenFlags, _: FileAttributes) -> Result<Handle, Self::Error> {
        if flags.contains(OpenFlags::CREATE) {
            self.files.insert(path.clone(), Vec::new());
        }
        if !self.files.contains_key(&path) {
            return Err(StatusCode::NoSuchFile);
        }
        let handle = format!("h{id}");
        self.handles.insert(handle.clone(), path);
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(Self::ok(id))
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, Self::Error> {
        let path = self.handles.get(&handle).ok_or(StatusCode::Failure)?;
        let file = &self.files[path];
        let start = offset as usize;
        if start >= file.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(file.len());
        Ok(Data { id, data: file[start..end].to_vec().into() })
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> Result<Status, Self::Error> {
        let path = self.handles.get(&handle).ok_or(StatusCode::Failure)?.clone();
        let file = self.files.get_mut(&path).ok_or(StatusCode::Failure)?;
        let end = offset as usize + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[offset as usize..end].copy_from_slice(&data);
        Ok(Self::ok(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        Ok(Attrs { id, attrs: self.attrs(&path)? })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        Ok(Attrs { id, attrs: self.attrs(&path)? })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let path = self.handles.get(&handle).ok_or(StatusCode::Failure)?.clone();
        Ok(Attrs { id, attrs: self.attrs(&path)? })
    }

    async fn realpath(&mut self, id: u32, _: String) -> Result<Name, Self::Error> {
        Ok(Name { id, files: vec![File::dummy("/")] })
    }
}

impl server::Handler for Conn {
    type Error = russh::Error;

    async fn authentication_banner(&mut self) -> Result<Option<String>, Self::Error> {
        Ok(self.opts.banner.map(str::to_string))
    }

    async fn auth_publickey(&mut self, user: &str, key: server::VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        let fine = user == USER && self.opts.allowed.as_ref().is_none_or(|k| k == key.public_key);
        Ok(if fine { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        self.opts.seen.lock().unwrap().passwords.push(password.to_string());
        if user == USER && password == PASSWORD {
            return Ok(Auth::Accept);
        }
        // like OpenSSH, go on offering the same methods after a wrong password
        Ok(Auth::Reject {
            proceed_with_methods: Some(MethodSet::from(self.opts.methods.as_slice())),
            partial_success: false,
        })
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _: &str,
        response: Option<Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        let Some(mut r) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed("Two-step login"),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Code: "), false)]),
            });
        };
        let answer = r.next().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        self.opts.seen.lock().unwrap().passwords.push(answer.clone());
        Ok(if user == USER && answer == PASSWORD { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        self.pending = Some(channel);
        Ok(())
    }

    async fn subsystem_request(&mut self, channel: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        match (name, self.opts.sftp, self.pending.take()) {
            ("sftp", true, Some(c)) => {
                session.channel_success(channel)?;
                tokio::spawn(russh_sftp::server::run(c.into_stream(), MemoryFs::default()));
                Ok(())
            }
            _ => session.channel_failure(channel),
        }
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.opts.seen.lock().unwrap().pty = Some((term.to_string(), cols, rows));
        session.channel_success(channel)
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.shell = Some(channel);
        session.channel_success(channel)?;
        session.data(channel, b"welcome\r\n$ ".to_vec())
    }

    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.opts.seen.lock().unwrap().resizes.push((cols, rows));
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.opts.seen.lock().unwrap().exec.push(String::from_utf8_lossy(data).into_owned());
        session.channel_success(channel)?;
        session.data(channel, b"ran it\r\n".to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.close(channel)
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.opts.seen.lock().unwrap().env.push((name.to_string(), value.to_string()));
        session.channel_success(channel)
    }

    /// Connects to the destination for real, as sshd would.
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match tokio::net::TcpStream::connect((host, port as u16)).await {
            Ok(mut tcp) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut ch = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut ch, &mut tcp).await;
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    /// Listens on the requested port and opens a channel for each connection.
    async fn tcpip_forward(&mut self, address: &str, port: &mut u32, session: &mut Session) -> Result<bool, Self::Error> {
        let Ok(listener) = tokio::net::TcpListener::bind((address, *port as u16)).await else {
            return Ok(false);
        };
        let (handle, address, port) = (session.handle(), address.to_string(), *port);
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let opened = handle.channel_open_forwarded_tcpip(address.clone(), port, "127.0.0.1", 1).await;
                if let Ok(channel) = opened {
                    tokio::spawn(async move {
                        let mut ch = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut ch, &mut tcp).await;
                    });
                }
            }
        });
        Ok(true)
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        // forwarded channels are piped elsewhere; only the shell echoes
        if self.shell != Some(channel) {
            return Ok(());
        }
        self.opts.seen.lock().unwrap().input.extend_from_slice(data);
        session.data(channel, data.to_vec())?;
        self.line.extend_from_slice(data);
        while let Some(i) = self.line.iter().position(|&b| b == b'\r') {
            if &self.line[..i] == b"exit" {
                session.exit_status_request(channel, 3)?;
                session.close(channel)?;
            }
            self.line.drain(..=i);
        }
        Ok(())
    }
}

/// Starts a server and returns its port. Dropped with the runtime.
pub async fn start(key: PrivateKey, methods: Vec<MethodKind>) -> (u16, Arc<Mutex<Seen>>) {
    start_with_banner(key, methods, None).await
}

pub async fn start_with_banner(key: PrivateKey, methods: Vec<MethodKind>, banner: Option<&'static str>) -> (u16, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    start_options(key, Options { methods, seen: seen.clone(), banner, allowed: None, sftp: false }).await;
    let port = LAST_PORT.with(|p| p.get());
    (port, seen)
}

#[allow(dead_code)]
pub async fn start_with_sftp(key: PrivateKey, methods: Vec<MethodKind>) -> (u16, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    start_options(key, Options { methods, seen: seen.clone(), banner: None, allowed: None, sftp: true }).await;
    (LAST_PORT.with(|p| p.get()), seen)
}

#[allow(dead_code)]
pub async fn start_with_key(key: PrivateKey, allowed: russh::keys::PublicKey) -> (u16, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let methods = vec![MethodKind::PublicKey];
    start_options(key, Options { methods, seen: seen.clone(), banner: None, allowed: Some(allowed), sftp: false }).await;
    (LAST_PORT.with(|p| p.get()), seen)
}

thread_local! {
    static LAST_PORT: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

async fn start_options(key: PrivateKey, opts: Options) {
    let methods = opts.methods.clone();
    let config = Arc::new(server::Config {
        methods: MethodSet::from(methods.as_slice()),
        keys: vec![key],
        auth_rejection_time: std::time::Duration::from_millis(1),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(0)),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    LAST_PORT.with(|p| p.set(listener.local_addr().unwrap().port()));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let conn = Conn { opts: opts.clone(), line: Vec::new(), shell: None, pending: None };
            let config = config.clone();
            tokio::spawn(async move {
                if let Ok(s) = server::run_stream(config, stream, conn).await {
                    let _ = s.await;
                }
            });
        }
    });
}

pub fn new_key(alg: Algorithm) -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), alg).unwrap()
}
