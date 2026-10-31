//! PuTTY's authentication plugin protocol, version 2, from `doc/authplugin.but`.
//! A plugin is a local program that answers keyboard-interactive prompts.

use anyhow::{anyhow, bail, ensure};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout};

const INIT: u8 = 1;
const INIT_RESPONSE: u8 = 2;
const PROTOCOL: u8 = 3;
const PROTOCOL_ACCEPT: u8 = 4;
const PROTOCOL_REJECT: u8 = 5;
const AUTH_SUCCESS: u8 = 6;
const AUTH_FAILURE: u8 = 7;
const INIT_FAILURE: u8 = 8;
const KI_SERVER_REQUEST: u8 = 20;
const KI_SERVER_RESPONSE: u8 = 21;
const KI_USER_REQUEST: u8 = 22;
const KI_USER_RESPONSE: u8 = 23;
const VERSION: u32 = 2;
const MAX_MESSAGE: usize = 1 << 20;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) struct Prompts {
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<(String, bool)>,
}

pub(crate) enum Step {
    /// The plugin asks the person these.
    AskUser(Prompts),
    /// The plugin's answers for the server.
    Answer(Vec<String>),
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend((s.len() as u32).to_be_bytes());
    out.extend(s);
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn u32(&mut self) -> anyhow::Result<u32> {
        ensure!(self.0.len() >= 4, "the plugin sent a short message");
        let (head, rest) = self.0.split_at(4);
        self.0 = rest;
        Ok(u32::from_be_bytes(head.try_into()?))
    }

    fn string(&mut self) -> anyhow::Result<String> {
        let n = self.u32()? as usize;
        ensure!(self.0.len() >= n, "the plugin sent a short message");
        let (body, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(String::from_utf8_lossy(body).into_owned())
    }

    fn boolean(&mut self) -> anyhow::Result<bool> {
        let (&b, rest) = self.0.split_first().ok_or_else(|| anyhow!("the plugin sent a short message"))?;
        self.0 = rest;
        Ok(b != 0)
    }
}

fn prompts_from(body: &[u8]) -> anyhow::Result<Prompts> {
    let mut r = Reader(body);
    let (name, instructions) = (r.string()?, r.string()?);
    r.string()?;
    let n = r.u32()?;
    let mut prompts = Vec::new();
    for _ in 0..n.min(256) {
        prompts.push((r.string()?, r.boolean()?));
    }
    Ok(Prompts { name, instructions, prompts })
}

fn prompts_body(p: &Prompts) -> Vec<u8> {
    let mut b = Vec::new();
    put_string(&mut b, p.name.as_bytes());
    put_string(&mut b, p.instructions.as_bytes());
    put_string(&mut b, b"");
    b.extend((p.prompts.len() as u32).to_be_bytes());
    for (text, echo) in &p.prompts {
        put_string(&mut b, text.as_bytes());
        b.push(*echo as u8);
    }
    b
}

fn answers_from(body: &[u8]) -> anyhow::Result<Vec<String>> {
    let mut r = Reader(body);
    let n = r.u32()?;
    (0..n.min(256)).map(|_| r.string()).collect()
}

fn answers_body(answers: &[String]) -> Vec<u8> {
    let mut b = (answers.len() as u32).to_be_bytes().to_vec();
    for a in answers {
        put_string(&mut b, a.as_bytes());
    }
    b
}

pub(crate) struct Plugin {
    _child: Child,
    to: ChildStdin,
    from: ChildStdout,
}

impl Plugin {
    /// Starts a plugin. Returns it with the user name it suggests, if any.
    pub async fn start(command: &str, host: &str, port: u16, user: &str) -> anyhow::Result<(Plugin, Option<String>)> {
        let mut child = crate::proxy::shell(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| anyhow!("Could not start the authentication plugin: {e}"))?;
        let (Some(to), Some(from)) = (child.stdin.take(), child.stdout.take()) else {
            bail!("Could not connect to the authentication plugin.");
        };
        let mut p = Plugin { _child: child, to, from };
        let mut init = VERSION.to_be_bytes().to_vec();
        put_string(&mut init, host.as_bytes());
        init.extend((port as u32).to_be_bytes());
        put_string(&mut init, user.as_bytes());
        p.send(INIT, &init).await?;
        let (kind, body) = p.receive().await?;
        let mut r = Reader(&body);
        match kind {
            INIT_RESPONSE => {
                let version = r.u32()?;
                ensure!(version <= VERSION && version >= 2, "The authentication plugin wants protocol version {version}.");
                Ok((p, Some(r.string()?).filter(|u| !u.is_empty())))
            }
            INIT_FAILURE => bail!("The authentication plugin failed: {}", r.string()?),
            k => bail!("The authentication plugin sent message {k} at startup."),
        }
    }

    async fn send(&mut self, kind: u8, body: &[u8]) -> anyhow::Result<()> {
        let mut msg = ((body.len() + 1) as u32).to_be_bytes().to_vec();
        msg.push(kind);
        msg.extend(body);
        self.to.write_all(&msg).await?;
        self.to.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> anyhow::Result<(u8, Vec<u8>)> {
        let n = self.from.read_u32().await.map_err(|_| anyhow!("The authentication plugin stopped."))? as usize;
        ensure!((1..=MAX_MESSAGE).contains(&n), "The authentication plugin sent a bad message.");
        let mut msg = vec![0u8; n];
        self.from.read_exact(&mut msg).await?;
        Ok((msg[0], msg[1..].to_vec()))
    }

    /// Asks whether the plugin will help with keyboard-interactive. The reason
    /// for a refusal is returned for showing, if it gave one.
    pub async fn offer_keyboard(&mut self) -> anyhow::Result<Result<(), String>> {
        let mut body = Vec::new();
        put_string(&mut body, b"keyboard-interactive");
        self.send(PROTOCOL, &body).await?;
        let (kind, reply) = self.receive().await?;
        match kind {
            PROTOCOL_ACCEPT => Ok(Ok(())),
            PROTOCOL_REJECT => Ok(Err(Reader(&reply).string()?)),
            k => bail!("The authentication plugin sent message {k} instead of an answer."),
        }
    }

    pub async fn server_request(&mut self, p: &Prompts) -> anyhow::Result<Step> {
        self.send(KI_SERVER_REQUEST, &prompts_body(p)).await?;
        self.next_step().await
    }

    pub async fn user_response(&mut self, answers: &[String]) -> anyhow::Result<Step> {
        self.send(KI_USER_RESPONSE, &answers_body(answers)).await?;
        self.next_step().await
    }

    async fn next_step(&mut self) -> anyhow::Result<Step> {
        let (kind, body) = self.receive().await?;
        match kind {
            KI_USER_REQUEST => Ok(Step::AskUser(prompts_from(&body)?)),
            KI_SERVER_RESPONSE => Ok(Step::Answer(answers_from(&body)?)),
            k => bail!("The authentication plugin sent message {k} during login."),
        }
    }

    pub async fn finished(&mut self, success: bool) -> anyhow::Result<()> {
        self.send(if success { AUTH_SUCCESS } else { AUTH_FAILURE }, &[]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_round_trip_in_the_wire_format() {
        let p = Prompts { name: "n".into(), instructions: "i".into(), prompts: vec![("Code: ".into(), false), ("Name: ".into(), true)] };
        let got = prompts_from(&prompts_body(&p)).unwrap();
        assert_eq!((got.name.as_str(), got.instructions.as_str()), ("n", "i"));
        assert_eq!(got.prompts, p.prompts);
    }

    #[test]
    fn answers_round_trip() {
        let a = vec!["one".to_string(), String::new()];
        assert_eq!(answers_from(&answers_body(&a)).unwrap(), a);
    }

    #[test]
    fn truncated_messages_are_errors() {
        assert!(answers_from(&[0, 0, 0, 1, 0, 0, 0, 9, b'x']).is_err());
    }
}
