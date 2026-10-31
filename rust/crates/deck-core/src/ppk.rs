//! PuTTY private key files (`.ppk`), versions 1 to 3, after `sshpubk.c`.

use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{anyhow, bail, ensure, Context};
use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac};
use russh::keys::{decode_secret_key, PrivateKey};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

type Decrypt = cbc::Decryptor<aes::Aes256>;
type Encrypt = cbc::Encryptor<aes::Aes256>;

const AES_KEY: usize = 32;
/// The most a key file can ask of its key derivation: 1 GiB, far past any
/// real file. PuTTY's own defaults are 8 MiB and a few dozen passes.
const MAX_ARGON_MEMORY: u32 = 1 << 20;
const MAX_ARGON_PASSES: u32 = 100_000;
const MAX_ARGON_LANES: u32 = 64;
const AES_IV: usize = 16;
const MAC_KEY: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Argon {
    pub flavour: String,
    pub memory: u32,
    pub passes: u32,
    pub parallelism: u32,
    pub salt: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ppk {
    pub version: u32,
    pub algorithm: String,
    pub encrypted: bool,
    pub comment: String,
    pub public: Vec<u8>,
    private: Vec<u8>,
    argon: Option<Argon>,
    mac: Vec<u8>,
}

fn unbase64(lines: &str) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    let (mut n, mut bits) = (0u32, 0);
    for c in lines.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => bail!("the key file has bad base64 in it"),
        };
        n = n << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((n >> bits) as u8);
        }
    }
    Ok(out)
}

fn unhex(s: &str) -> anyhow::Result<Vec<u8>> {
    let s = s.trim();
    ensure!(s.is_ascii() && s.len() % 2 == 0, "the key file has bad hex in it");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| anyhow!("the key file has bad hex in it"))).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn put_string(out: &mut Vec<u8>, data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(data);
}

/// Reads length-prefixed fields.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn string(&mut self) -> anyhow::Result<&'a [u8]> {
        let len = self.0.get(..4).ok_or_else(|| anyhow!("the key is truncated"))?;
        let len = u32::from_be_bytes(len.try_into()?) as usize;
        ensure!(self.0.len() >= 4 + len, "the key is truncated");
        let (field, rest) = self.0[4..].split_at(len);
        self.0 = rest;
        Ok(field)
    }
}

fn cipher_material(version: u32, argon: Option<&Argon>, pass: &str) -> anyhow::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    if version == 3 {
        let a = argon.ok_or_else(|| anyhow!("the key file has no key derivation settings"))?;
        let algorithm = match a.flavour.as_str() {
            "Argon2d" => Algorithm::Argon2d,
            "Argon2i" => Algorithm::Argon2i,
            "Argon2id" => Algorithm::Argon2id,
            f => bail!("unknown key derivation {f}"),
        };
        // a hostile file could otherwise ask for all the memory there is, or hours
        ensure!(a.memory <= MAX_ARGON_MEMORY && a.passes <= MAX_ARGON_PASSES && a.parallelism <= MAX_ARGON_LANES, "the key file asks for too much work to unlock");
        let params = Params::new(a.memory, a.passes, a.parallelism, Some(AES_KEY + AES_IV + MAC_KEY)).map_err(|e| anyhow!("bad key derivation settings: {e}"))?;
        let mut out = [0u8; AES_KEY + AES_IV + MAC_KEY];
        Argon2::new(algorithm, Version::V0x13, params).hash_password_into(pass.as_bytes(), &a.salt, &mut out).map_err(|e| anyhow!("key derivation failed: {e}"))?;
        let material = (out[..AES_KEY].to_vec(), out[AES_KEY..AES_KEY + AES_IV].to_vec(), out[AES_KEY + AES_IV..].to_vec());
        out.zeroize();
        return Ok(material);
    }
    let mut key = Vec::new();
    for ctr in 0u32..2 {
        let mut h = Sha1::new();
        h.update(ctr.to_be_bytes());
        h.update(pass);
        key.extend(h.finalize());
    }
    key.truncate(AES_KEY);
    Ok((key, vec![0; AES_IV], mac_key_v2(pass)))
}

fn mac_key_v2(pass: &str) -> Vec<u8> {
    let mut h = Sha1::new();
    h.update("putty-private-key-file-mac-key");
    h.update(pass);
    h.finalize().to_vec()
}

fn mac_of(version: u32, key: &[u8], fields: &[&[u8]]) -> anyhow::Result<Vec<u8>> {
    let mut data = Vec::new();
    for f in fields {
        put_string(&mut data, f);
    }
    Ok(if version == 3 {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key)?;
        m.update(&data);
        m.finalize().into_bytes().to_vec()
    } else {
        let mut m = <Hmac<Sha1> as Mac>::new_from_slice(key)?;
        m.update(&data);
        m.finalize().into_bytes().to_vec()
    })
}

impl Ppk {
    pub fn parse(text: &str) -> anyhow::Result<Ppk> {
        let mut lines = text.lines();
        let header = |lines: &mut std::str::Lines<'_>, name: &str| -> anyhow::Result<String> {
            let line = lines.next().ok_or_else(|| anyhow!("the key file is truncated"))?;
            let (k, v) = line.split_once(": ").ok_or_else(|| anyhow!("the key file is damaged"))?;
            ensure!(k == name, "expected {name} in the key file, found {k}");
            Ok(v.to_string())
        };
        let first = lines.next().ok_or_else(|| anyhow!("the key file is empty"))?;
        let (kind, algorithm) = first.split_once(": ").ok_or_else(|| anyhow!("not a PuTTY key file"))?;
        let version = match kind {
            "PuTTY-User-Key-File-3" => 3,
            "PuTTY-User-Key-File-2" => 2,
            "PuTTY-User-Key-File-1" => bail!("this is a version 1 key file, which has no integrity check; convert it with PuTTYgen"),
            _ => bail!("not a PuTTY key file"),
        };
        let encryption = header(&mut lines, "Encryption")?;
        let encrypted = match encryption.as_str() {
            "none" => false,
            "aes256-cbc" => true,
            e => bail!("unknown key file encryption {e}"),
        };
        let comment = header(&mut lines, "Comment")?;
        let block = |lines: &mut std::str::Lines<'_>, count: usize| -> anyhow::Result<Vec<u8>> {
            let mut b64 = String::new();
            for _ in 0..count {
                b64.push_str(lines.next().ok_or_else(|| anyhow!("the key file is truncated"))?);
            }
            unbase64(&b64)
        };
        let count: usize = header(&mut lines, "Public-Lines")?.parse().context("bad Public-Lines")?;
        let public = block(&mut lines, count)?;
        let mut argon = None;
        if version == 3 && encrypted {
            argon = Some(Argon {
                flavour: header(&mut lines, "Key-Derivation")?,
                memory: header(&mut lines, "Argon2-Memory")?.parse().context("bad Argon2-Memory")?,
                passes: header(&mut lines, "Argon2-Passes")?.parse().context("bad Argon2-Passes")?,
                parallelism: header(&mut lines, "Argon2-Parallelism")?.parse().context("bad Argon2-Parallelism")?,
                salt: unhex(&header(&mut lines, "Argon2-Salt")?)?,
            });
        }
        let count: usize = header(&mut lines, "Private-Lines")?.parse().context("bad Private-Lines")?;
        let private = block(&mut lines, count)?;
        let mac = unhex(&header(&mut lines, "Private-MAC")?)?;
        Ok(Ppk { version, algorithm: algorithm.to_string(), encrypted, comment, public, private, argon, mac })
    }

    /// Decrypts and checks the key. A wrong passphrase is an error.
    pub fn private_blob(&self, pass: &str) -> anyhow::Result<Vec<u8>> {
        let mut private = self.private.clone();
        let (mac_key, encryption) = if self.encrypted {
            let (mut key, iv, mac) = cipher_material(self.version, self.argon.as_ref(), pass)?;
            ensure!(private.len() % 16 == 0, "the key file is damaged");
            Decrypt::new_from_slices(&key, &iv)?.decrypt_padded_mut::<NoPadding>(&mut private).map_err(|_| anyhow!("the key file is damaged"))?;
            key.zeroize();
            (mac, "aes256-cbc")
        } else if self.version == 3 {
            (Vec::new(), "none")
        } else {
            (mac_key_v2(""), "none")
        };
        let fields: [&[u8]; 5] = [self.algorithm.as_bytes(), encryption.as_bytes(), self.comment.as_bytes(), &self.public, &private];
        let want = mac_of(self.version, &mac_key, &fields)?;
        // equality needn't be constant-time: nothing here is remote or retried unthrottled
        if want != self.mac {
            private.zeroize();
            bail!(if self.encrypted { "wrong passphrase" } else { "the key file has been altered or damaged" });
        }
        Ok(private)
    }

    pub fn to_private_key(&self, pass: &str) -> anyhow::Result<PrivateKey> {
        let blob = Zeroizing::new(self.private_blob(pass)?);
        let pem = Zeroizing::new(openssh_pem(&self.algorithm, &self.comment, &self.public, &blob)?);
        decode_secret_key(&pem, None).map_err(|e| anyhow!("could not use this key: {e}"))
    }

    /// Writes a version 3 file. An empty passphrase leaves it unencrypted.
    pub fn write(algorithm: &str, comment: &str, public: &[u8], private: &[u8], pass: &str, argon: Option<Argon>) -> anyhow::Result<String> {
        let mut private = private.to_vec();
        let encrypted = !pass.is_empty();
        let (encryption, mac_key) = if encrypted {
            let pad = 16 - private.len() % 16;
            private.resize(private.len() + pad % 16, 0);
            let a = argon.as_ref().ok_or_else(|| anyhow!("encryption needs key derivation settings"))?;
            let (key, iv, mac) = cipher_material(3, Some(a), pass)?;
            ("aes256-cbc", (mac, key, iv))
        } else {
            ("none", (Vec::new(), Vec::new(), Vec::new()))
        };
        let (mac_key, key, iv) = mac_key;
        let fields: [&[u8]; 5] = [algorithm.as_bytes(), encryption.as_bytes(), comment.as_bytes(), public, &private];
        let mac = mac_of(3, &mac_key, &fields)?;
        if encrypted {
            let len = private.len();
            Encrypt::new_from_slices(&key, &iv)?.encrypt_padded_mut::<NoPadding>(&mut private, len).map_err(|_| anyhow!("encryption failed"))?;
        }
        let wrap = |data: &[u8]| -> (usize, String) {
            let b64 = crate::proxy::base64(data);
            let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
            (lines.len(), lines.join("\n") + "\n")
        };
        let mut out = format!("PuTTY-User-Key-File-3: {algorithm}\nEncryption: {encryption}\nComment: {comment}\n");
        let (n, body) = wrap(public);
        out += &format!("Public-Lines: {n}\n{body}");
        if let (true, Some(a)) = (encrypted, &argon) {
            out += &format!(
                "Key-Derivation: {}\nArgon2-Memory: {}\nArgon2-Passes: {}\nArgon2-Parallelism: {}\nArgon2-Salt: {}\n",
                a.flavour, a.memory, a.passes, a.parallelism, hex(&a.salt)
            );
        }
        let (n, body) = wrap(&private);
        out += &format!("Private-Lines: {n}\n{body}Private-MAC: {}\n", hex(&mac));
        Ok(out)
    }
}

/// Re-frames PuTTY's public and private blobs as an unencrypted OpenSSH key,
/// the one private key format the SSH library takes.
pub(crate) fn openssh_pem(algorithm: &str, comment: &str, public: &[u8], private: &[u8]) -> anyhow::Result<String> {
    let mut pubr = Reader(public);
    let mut privr = Reader(private);
    pubr.string()?;
    let mut key = Vec::new();
    put_string(&mut key, algorithm.as_bytes());
    match algorithm {
        "ssh-rsa" => {
            let (e, n) = (pubr.string()?, pubr.string()?);
            let (d, p, q, iqmp) = (privr.string()?, privr.string()?, privr.string()?, privr.string()?);
            for f in [n, e, d, iqmp, p, q] {
                put_string(&mut key, f);
            }
        }
        "ssh-ed25519" => {
            let (a, k) = (pubr.string()?, privr.string()?);
            ensure!(a.len() == 32 && k.len() == 32, "the Ed25519 key has the wrong length");
            put_string(&mut key, a);
            put_string(&mut key, &[k, a].concat());
        }
        a if a.starts_with("ecdsa-sha2-nistp") => {
            let (curve, q) = (pubr.string()?, pubr.string()?);
            put_string(&mut key, curve);
            put_string(&mut key, q);
            put_string(&mut key, privr.string()?);
        }
        a => bail!("RustuTTY can't use {a} keys"),
    }
    put_string(&mut key, comment.as_bytes());

    // the check words match, as nothing is encrypted; padding fills the last block
    let mut section = vec![0x70, 0x75, 0x74, 0x74, 0x70, 0x75, 0x74, 0x74];
    section.extend(&key);
    let mut pad = 1u8;
    while section.len() % 8 != 0 {
        section.push(pad);
        pad += 1;
    }
    let mut file = b"openssh-key-v1\0".to_vec();
    put_string(&mut file, b"none");
    put_string(&mut file, b"none");
    put_string(&mut file, b"");
    file.extend(1u32.to_be_bytes());
    put_string(&mut file, public);
    put_string(&mut file, &section);
    let b64 = crate::proxy::base64(&file);
    let body: Vec<&str> = b64.as_bytes().chunks(70).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    Ok(format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n", body.join("\n")))
}

/// The algorithm name and PuTTY's public and private blobs for a key: the
/// reverse of `openssh_pem`.
pub fn blobs_of(key: &PrivateKey) -> anyhow::Result<(String, Vec<u8>, Vec<u8>)> {
    let pem = key.to_openssh(russh::keys::ssh_key::LineEnding::LF).map_err(|e| anyhow!("{e}"))?;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let file = unbase64(&b64)?;
    let mut r = Reader(file.strip_prefix(b"openssh-key-v1\0").ok_or_else(|| anyhow!("unexpected key format"))?);
    ensure!(r.string()? == b"none", "the key is encrypted");
    r.string()?;
    r.string()?;
    ensure!(r.0.get(..4) == Some(&1u32.to_be_bytes()[..]), "expected one key");
    r.0 = &r.0[4..];
    let public = r.string()?.to_vec();
    let section = r.string()?;
    let mut s = Reader(section.get(8..).ok_or_else(|| anyhow!("the key is truncated"))?);
    let algorithm = String::from_utf8_lossy(s.string()?).into_owned();
    let mut private = Vec::new();
    match algorithm.as_str() {
        "ssh-rsa" => {
            let (_n, _e, d, iqmp, p, q) = (s.string()?, s.string()?, s.string()?, s.string()?, s.string()?, s.string()?);
            for f in [d, p, q, iqmp] {
                put_string(&mut private, f);
            }
        }
        "ssh-ed25519" => {
            s.string()?;
            let both = s.string()?;
            ensure!(both.len() == 64, "the Ed25519 key has the wrong length");
            put_string(&mut private, &both[..32]);
        }
        a if a.starts_with("ecdsa-sha2-nistp") => {
            s.string()?;
            s.string()?;
            put_string(&mut private, s.string()?);
        }
        a => bail!("RustuTTY can't use {a} keys"),
    }
    Ok((algorithm, public, private))
}

pub fn random(buf: &mut [u8]) -> anyhow::Result<()> {
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), buf).map_err(|_| anyhow!("no random numbers available"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap() -> Argon {
        Argon { flavour: "Argon2id".into(), memory: 64, passes: 1, parallelism: 1, salt: vec![7; 16] }
    }

    fn ed25519_blobs() -> (Vec<u8>, Vec<u8>) {
        // RFC 8032 test 1
        let k = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60").unwrap();
        let a = unhex("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a").unwrap();
        let (mut public, mut private) = (Vec::new(), Vec::new());
        put_string(&mut public, b"ssh-ed25519");
        put_string(&mut public, &a);
        put_string(&mut private, &k);
        (public, private)
    }

    #[test]
    fn unencrypted_round_trips_to_a_usable_key() {
        let (public, private) = ed25519_blobs();
        let text = Ppk::write("ssh-ed25519", "c", &public, &private, "", None).unwrap();
        let key = Ppk::parse(&text).unwrap().to_private_key("").unwrap();
        assert_eq!(key.public_key().to_bytes().unwrap(), public);
    }

    #[test]
    fn encrypted_needs_the_right_passphrase() {
        let (public, private) = ed25519_blobs();
        let text = Ppk::write("ssh-ed25519", "c", &public, &private, "pw", Some(cheap())).unwrap();
        let ppk = Ppk::parse(&text).unwrap();
        assert!(ppk.encrypted);
        assert_eq!(ppk.to_private_key("no").unwrap_err().to_string(), "wrong passphrase");
        assert_eq!(ppk.to_private_key("pw").unwrap().public_key().to_bytes().unwrap(), public);
    }

    #[test]
    fn altered_public_part_is_caught() {
        let (mut public, private) = ed25519_blobs();
        let text = Ppk::write("ssh-ed25519", "c", &public, &private, "", None).unwrap();
        let mut ppk = Ppk::parse(&text).unwrap();
        *public.last_mut().unwrap() ^= 1;
        ppk.public = public;
        assert!(ppk.private_blob("").unwrap_err().to_string().contains("altered"));
    }

    // Files written by PuTTY itself, from its test/cryptsuite.py.
    const PUBLIC: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHJCszOHaI9X/yGLtjn22f0hO6VPMQDVtctkym6F\nJH1W\n";
    const PASS: &str = "test-passphrase";

    fn putty_file(version: u32, encryption: &str, extra: &str, private: &str, mac: &str) -> String {
        format!(
            "PuTTY-User-Key-File-{version}: ssh-ed25519\nEncryption: {encryption}\nComment: ed25519-key-20200105\n\
             Public-Lines: 2\n{PUBLIC}{extra}Private-Lines: 1\n{private}\nPrivate-MAC: {mac}\n"
        )
    }

    fn assert_same_key(text: &str, pass: &str) {
        let key = Ppk::parse(text).unwrap().to_private_key(pass).unwrap();
        assert_eq!(key.public_key().to_bytes().unwrap(), unbase64(PUBLIC).unwrap());
    }

    #[test]
    fn reads_files_putty_wrote() {
        const CLEAR: &str = "AAAAIGvvIpl8jyqn8Xufkw6v3FnEGtXF3KWw55AP3/AGEBpY";
        assert_same_key(&putty_file(3, "none", "", CLEAR, "816c84093fc4877e8411b8e5139c5ce35d8387a2630ff087214911d67417a54d"), "");
        assert_same_key(&putty_file(2, "none", "", CLEAR, "2a629acfcfbe28488a1ba9b6948c36406bc28422"), "");
        assert_same_key(
            &putty_file(2, "aes256-cbc", "", "4/jKlTgC652oa9HLVGrMjHZw7tj0sKRuZaJPOuLhGTvb25Jzpcqpbi+Uf+y+uo+Z", "5b1f6f4cc43eb0060d2c3e181bc0129343adba2b"),
            PASS,
        );
        let argon = "Key-Derivation: Argon2id\nArgon2-Memory: 8192\nArgon2-Passes: 13\nArgon2-Parallelism: 1\nArgon2-Salt: 37c3911bfefc8c1d11ec579627d2b3d9\n";
        let v3 = putty_file(3, "aes256-cbc", argon, "amviz4sVUBN64jLO3gt4HGXJosUArghc4Soi7aVVLb2Tir5Baj0OQClorycuaPRd", "6f5e588e475e55434106ec2c3569695b03f423228b44993a9e97d52ffe7be5a8");
        assert_same_key(&v3, PASS);
        assert_eq!(Ppk::parse(&v3).unwrap().to_private_key("wrong").unwrap_err().to_string(), "wrong passphrase");
    }

    #[test]
    fn every_key_type_survives_a_round_trip_through_ppk() {
        use russh::keys::ssh_key::{Algorithm as A, EcdsaCurve};
        let mut rng = rand::rng();
        let kinds = [
            A::Ed25519,
            A::Ecdsa { curve: EcdsaCurve::NistP256 },
            A::Ecdsa { curve: EcdsaCurve::NistP384 },
            A::Ecdsa { curve: EcdsaCurve::NistP521 },
            A::Rsa { hash: None },
        ];
        for kind in kinds {
            let key = PrivateKey::random(&mut rng, kind.clone()).unwrap();
            let (alg, public, private) = blobs_of(&key).unwrap();
            let text = Ppk::write(&alg, "c", &public, &private, "", None).unwrap();
            let back = Ppk::parse(&text).unwrap().to_private_key("").unwrap();
            assert_eq!(back.public_key().to_bytes().unwrap(), key.public_key().to_bytes().unwrap(), "{kind:?}");
            let (_, _, again) = blobs_of(&back).unwrap();
            assert_eq!(again, private, "{kind:?}");
        }
    }

    #[test]
    fn version_1_is_turned_away() {
        assert!(Ppk::parse("PuTTY-User-Key-File-1: ssh-rsa\n").unwrap_err().to_string().contains("version 1"));
    }
}

#[cfg(test)]
mod hostile_files {
    use super::*;

    #[test]
    fn non_ascii_hex_is_an_error_not_a_crash() {
        assert!(unhex("a\u{e9}b").is_err());
        assert_eq!(unhex("0aff").unwrap(), [10, 255]);
    }

    #[test]
    fn a_file_cannot_ask_for_unlimited_work() {
        let argon = |memory, passes, parallelism| Argon { flavour: "Argon2id".into(), memory, passes, parallelism, salt: vec![0; 16] };
        for a in [argon(u32::MAX, 1, 1), argon(8192, u32::MAX, 1), argon(8192, 1, 1 << 20)] {
            let e = cipher_material(3, Some(&a), "x").unwrap_err();
            assert!(e.to_string().contains("too much work"), "{e}");
        }
        assert!(cipher_material(3, Some(&argon(64, 1, 1)), "x").is_ok());
    }
}