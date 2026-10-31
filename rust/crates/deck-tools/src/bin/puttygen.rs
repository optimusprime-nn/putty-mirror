//! PuTTYgen: makes and converts keys, on the command line like PuTTY's
//! `cmdgen`.
//!
//!   puttygen -t ed25519|rsa|ecdsa [-b bits] [-C comment] -o key.ppk
//!   puttygen key [-P] -o new.ppk          convert, or change the passphrase
//!
//! A new key asks for a passphrase, and a converted key keeps the one it has.
//! `--no-passphrase` is how to say on purpose that the output has none.
//!   puttygen key -L                       print the public key
//!   puttygen key -l                       print the fingerprint
//!   puttygen key -O private-openssh -o f  export to OpenSSH

use anyhow::{anyhow, bail, Context};
use deck_core::ppk::{self, Argon, Ppk};
use deck_tools::prompt;
use russh::keys::ssh_key::{Algorithm, EcdsaCurve, HashAlg, LineEnding};
use russh::keys::{load_secret_key, PrivateKey};
use std::io::Write;

fn new_passphrase() -> anyhow::Result<String> {
    let first = prompt("Enter passphrase to save key: ")?;
    anyhow::ensure!(first == prompt("Re-enter passphrase to verify: ")?, "the passphrases don't match");
    Ok(first)
}

/// The key, with the passphrase it was stored under.
fn load(path: &str) -> anyhow::Result<(PrivateKey, String)> {
    let text = std::fs::read(path).with_context(|| format!("reading {path}"))?;
    if text.starts_with(b"PuTTY-User-Key-File") {
        let file = Ppk::parse(&String::from_utf8_lossy(&text))?;
        let pass = if file.encrypted { prompt("Enter passphrase to load key: ")? } else { String::new() };
        return Ok((file.to_private_key(&pass)?, pass));
    }
    match load_secret_key(path, None) {
        Ok(k) => Ok((k, String::new())),
        Err(russh::keys::Error::KeyIsEncrypted) => {
            let pass = prompt("Enter passphrase to load key: ")?;
            Ok((load_secret_key(path, Some(&pass)).map_err(|e| anyhow!("{e}"))?, pass))
        }
        Err(e) => bail!("{path}: {e}"),
    }
}

fn generate(kind: &str, bits: usize) -> anyhow::Result<PrivateKey> {
    let algorithm = match (kind, bits) {
        ("ed25519", _) => Algorithm::Ed25519,
        ("rsa", _) => Algorithm::Rsa { hash: None },
        ("ecdsa", 256 | 0) => Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 },
        ("ecdsa", 384) => Algorithm::Ecdsa { curve: EcdsaCurve::NistP384 },
        ("ecdsa", 521) => Algorithm::Ecdsa { curve: EcdsaCurve::NistP521 },
        ("ecdsa", n) => bail!("ecdsa keys are 256, 384 or 521 bits, not {n}"),
        (k, _) => bail!("unknown key type {k}: use ed25519, rsa or ecdsa"),
    };
    if kind == "rsa" {
        let bits = if bits == 0 { 2048 } else { bits };
        anyhow::ensure!(bits >= 2048, "RSA keys under 2048 bits aren't safe");
        let pair = russh::keys::ssh_key::private::RsaKeypair::random(&mut rand::rng(), bits).map_err(|e| anyhow!("{e}"))?;
        return PrivateKey::new(pair.into(), "").map_err(|e| anyhow!("{e}"));
    }
    PrivateKey::random(&mut rand::rng(), algorithm).map_err(|e| anyhow!("{e}"))
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut kind, mut bits, mut comment, mut out, mut input) = (None, 0, None, None, None);
    let (mut passphrase, mut public, mut fingerprint, mut format) = (false, false, false, "private".to_string());
    let mut no_passphrase = false;
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow!("{a} needs a value"));
        match a.as_str() {
            "-t" => kind = Some(value()?),
            "-b" => bits = value()?.parse().context("-b needs a number")?,
            "-C" => comment = Some(value()?),
            "-o" => out = Some(value()?),
            "-O" => format = value()?,
            "-P" => passphrase = true,
            "--no-passphrase" => no_passphrase = true,
            "-L" => public = true,
            "-l" => fingerprint = true,
            s if s.starts_with('-') => bail!("unknown option {s}"),
            _ => input = Some(a),
        }
    }
    anyhow::ensure!(!(passphrase && no_passphrase), "-P and --no-passphrase can't go together");
    let (key, old_pass) = match (&kind, &input) {
        (Some(k), None) => (generate(k, bits)?, None),
        (None, Some(path)) => {
            let (key, pass) = load(path)?;
            (key, Some(pass))
        }
        _ => bail!("give either -t to make a key, or a key file to read"),
    };
    let comment = comment.unwrap_or_else(|| key.comment().to_string());
    if public {
        println!("{}", key.public_key().to_openssh().map_err(|e| anyhow!("{e}"))?);
        return Ok(());
    }
    if fingerprint {
        println!("{} {comment}", key.fingerprint(HashAlg::Sha256));
        return Ok(());
    }
    let out = out.ok_or_else(|| anyhow!("give -o for the output file"))?;
    let pass = match old_pass {
        _ if no_passphrase => String::new(),
        Some(_) if passphrase => new_passphrase()?,
        Some(old) => old,
        None => new_passphrase()?,
    };
    if pass.is_empty() {
        eprintln!("Warning: {out} will not be protected by a passphrase.");
    }
    let text = match format.as_str() {
        "private" => {
            let (algorithm, public, private) = ppk::blobs_of(&key)?;
            let argon = (!pass.is_empty()).then(|| new_argon()).transpose()?;
            Ppk::write(&algorithm, &comment, &public, &private, &pass, argon)?
        }
        "private-openssh" => {
            let protected = if pass.is_empty() { key.clone() } else { key.encrypt(&mut rand::rng(), &pass).map_err(|e| anyhow!("{e}"))? };
            protected.to_openssh(LineEnding::LF).map_err(|e| anyhow!("{e}"))?.to_string()
        }
        f => bail!("unknown output format {f}: use private or private-openssh"),
    };
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&out).with_context(|| format!("creating {out}, which must not exist already"))?;
    file.write_all(text.as_bytes())?;
    eprintln!("Wrote {out}");
    Ok(())
}

/// PuTTY's defaults for a new key file.
fn new_argon() -> anyhow::Result<Argon> {
    let mut salt = vec![0u8; 16];
    ppk::random(&mut salt)?;
    Ok(Argon { flavour: "Argon2id".into(), memory: 8192, passes: 21, parallelism: 1, salt })
}
