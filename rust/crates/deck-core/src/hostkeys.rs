//! Host key checking. Keys PuTTY already trusts are honoured from its own
//! store, which is only read. Keys accepted here go in PuTTYDeck's own store.

use crate::config::escape_key;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::{Algorithm, HashAlg, PublicKey};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Trusted,
    Unknown,
    /// A different key is on record for this host: possibly an attack.
    Changed,
}

pub trait HostKeyStore: Send + Sync {
    /// The value PuTTY stored under this name, in PuTTY's format.
    fn putty(&self, name: &str) -> Option<String>;
    /// The value PuTTYDeck stored under this name, in OpenSSH format.
    fn deck(&self, name: &str) -> Option<String>;
    fn remember(&self, name: &str, value: &str) -> anyhow::Result<()>;
}

/// PuTTY's name for a key type, from `cache_id` in its key algorithm tables.
fn cache_id(key: &PublicKey) -> Option<String> {
    match key.key_data() {
        KeyData::Rsa(_) => Some("rsa2".into()),
        KeyData::Ed25519(_) => Some("ssh-ed25519".into()),
        KeyData::Ecdsa(_) => Some(key.algorithm().to_string()),
        _ => None,
    }
}

pub fn store_name(host: &str, port: u16, key: &PublicKey) -> Option<String> {
    Some(format!("{}@{port}:{}", cache_id(key)?, escape_key(host)))
}

/// Whether a key of this algorithm is on record for a host, in either store.
pub fn known_any(store: &dyn HostKeyStore, host: &str, port: u16) -> bool {
    ["rsa2", "ssh-ed25519", "ssh-ed448", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521"].iter().any(|id| {
        let name = format!("{id}@{port}:{}", escape_key(host));
        store.deck(&name).is_some() || store.putty(&name).is_some()
    })
}

pub fn known(store: &dyn HostKeyStore, host: &str, port: u16, algorithm: &Algorithm) -> bool {
    let id = match algorithm {
        Algorithm::Rsa { .. } => "rsa2".to_string(),
        a => a.to_string(),
    };
    let name = format!("{id}@{port}:{}", escape_key(host));
    store.deck(&name).is_some() || store.putty(&name).is_some()
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

fn openssh_value(key: &PublicKey) -> Option<String> {
    let line = key.to_openssh().ok()?;
    line.split_whitespace().nth(1).map(str::to_string)
}

fn hex_of(bytes: &[u8]) -> String {
    let s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    s.trim_start_matches('0').to_string()
}

fn norm(s: &str) -> Option<String> {
    let h = s.trim().strip_prefix("0x")?.to_lowercase();
    h.chars().all(|c| c.is_ascii_hexdigit()).then(|| h.trim_start_matches('0').to_string())
}

/// The trailing hex fields of a PuTTY value, after any curve name.
fn fields(value: &str) -> Option<Vec<String>> {
    value.split(',').filter(|f| !f.starts_with("nistp") && !f.is_empty()).map(norm).collect()
}

/// Compares a key with PuTTY's stored form of it (`cache_str` in PuTTY).
pub fn matches_putty(value: &str, key: &PublicKey) -> bool {
    let Some(f) = fields(value) else { return false };
    match key.key_data() {
        KeyData::Rsa(r) => {
            f.len() == 2 && f[0] == hex_of(r.e().as_bytes()) && f[1] == hex_of(r.n().as_bytes())
        }
        KeyData::Ed25519(k) => {
            // PuTTY stores the point as x,y; the wire form is y, little-endian,
            // with the low bit of x in the top bit.
            if f.len() != 2 {
                return false;
            }
            let mut wire = k.0;
            let x_odd = wire[31] & 0x80 != 0;
            wire[31] &= 0x7f;
            wire.reverse();
            let stored_odd = f[0].chars().last().and_then(|c| c.to_digit(16)).is_some_and(|d| d & 1 == 1);
            stored_odd == x_odd && f[1] == hex_of(&wire)
        }
        KeyData::Ecdsa(k) => {
            let sec1 = k.as_sec1_bytes();
            if sec1.first() != Some(&4) || f.len() != 2 {
                return false;
            }
            let (x, y) = sec1[1..].split_at((sec1.len() - 1) / 2);
            f[0] == hex_of(x) && f[1] == hex_of(y)
        }
        _ => false,
    }
}

pub fn check(store: &dyn HostKeyStore, host: &str, port: u16, key: &PublicKey) -> Verdict {
    let Some(name) = store_name(host, port, key) else { return Verdict::Unknown };
    if let Some(v) = store.deck(&name) {
        return if Some(v) == openssh_value(key) { Verdict::Trusted } else { Verdict::Changed };
    }
    match store.putty(&name) {
        Some(v) if matches_putty(&v, key) => Verdict::Trusted,
        Some(_) => Verdict::Changed,
        None => Verdict::Unknown,
    }
}

pub fn remember(store: &dyn HostKeyStore, host: &str, port: u16, key: &PublicKey) -> anyhow::Result<()> {
    let name = store_name(host, port, key).ok_or_else(|| anyhow::anyhow!("unsupported key type"))?;
    let value = openssh_value(key).ok_or_else(|| anyhow::anyhow!("could not encode key"))?;
    store.remember(&name, &value)
}

/// A store with no persistence, for tests and for builds with no registry.
#[derive(Default)]
pub struct MemoryStore {
    pub putty: Mutex<HashMap<String, String>>,
    pub deck: Mutex<HashMap<String, String>>,
}

impl HostKeyStore for MemoryStore {
    fn putty(&self, name: &str) -> Option<String> {
        self.putty.lock().unwrap().get(name).cloned()
    }
    fn deck(&self, name: &str) -> Option<String> {
        self.deck.lock().unwrap().get(name).cloned()
    }
    fn remember(&self, name: &str, value: &str) -> anyhow::Result<()> {
        self.deck.lock().unwrap().insert(name.to_string(), value.to_string());
        Ok(())
    }
}

#[cfg(windows)]
pub struct RegistryStore;

#[cfg(windows)]
impl HostKeyStore for RegistryStore {
    fn putty(&self, name: &str) -> Option<String> {
        use winreg::{enums::*, RegKey};
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let k = hkcu.open_subkey_with_flags(format!(r"{}\SshHostKeys", crate::config::PUTTY_KEY), KEY_READ).ok()?;
        k.get_value(name).ok()
    }
    fn deck(&self, name: &str) -> Option<String> {
        use winreg::{enums::*, RegKey};
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let k = hkcu.open_subkey_with_flags(r"Software\PuTTYDeck\SshHostKeys", KEY_READ).ok()?;
        k.get_value(name).ok()
    }
    fn remember(&self, name: &str, value: &str) -> anyhow::Result<()> {
        use winreg::{enums::*, RegKey};
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (k, _) = hkcu.create_subkey_with_flags(r"Software\PuTTYDeck\SshHostKeys", KEY_SET_VALUE)?;
        k.set_value(name, &value.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::public::Ed25519PublicKey;

    // The Ed25519 base point, a value checkable against RFC 8032.
    const BX: &str = "0x216936d3cd6e53fec0a4e231fdd6dc5c692cc7609525a7b2c9562d608f25d51a";
    const BY: &str = "0x6666666666666666666666666666666666666666666666666666666666666658";

    fn base_point() -> PublicKey {
        let mut k = [0x66u8; 32];
        k[0] = 0x58;
        PublicKey::from(KeyData::Ed25519(Ed25519PublicKey(k)))
    }

    #[test]
    fn ed25519_matches_putty_form() {
        let key = base_point();
        assert!(matches_putty(&format!("{BX},{BY}"), &key));
        // x with the other parity is a different key
        let odd = format!("{}b,{BY}", &BX[..BX.len() - 1]);
        assert!(!matches_putty(&odd, &key));
        let other_y = format!("{BX},{}7", &BY[..BY.len() - 1]);
        assert!(!matches_putty(&other_y, &key));
        assert!(!matches_putty("nonsense", &key));
        assert!(!matches_putty("", &key));
    }

    #[test]
    fn verdicts() {
        let key = base_point();
        let store = MemoryStore::default();
        assert_eq!(store_name("Host.example", 22, &key).unwrap(), "ssh-ed25519@22:Host.example");
        assert_eq!(check(&store, "h", 22, &key), Verdict::Unknown);

        store.putty.lock().unwrap().insert("ssh-ed25519@22:h".into(), format!("{BX},{BY}"));
        assert_eq!(check(&store, "h", 22, &key), Verdict::Trusted);
        assert_eq!(check(&store, "h", 2222, &key), Verdict::Unknown, "port is part of the identity");

        store.putty.lock().unwrap().insert("ssh-ed25519@22:h".into(), format!("{BX},0x1"));
        assert_eq!(check(&store, "h", 22, &key), Verdict::Changed);

        remember(&store, "h", 22, &key).unwrap();
        assert_eq!(check(&store, "h", 22, &key), Verdict::Trusted, "accepted key overrides the old record");
        assert!(store.putty.lock().unwrap().get("ssh-ed25519@22:h").unwrap().ends_with("0x1"), "PuTTY's record is untouched");

        store.deck.lock().unwrap().insert("ssh-ed25519@22:h".into(), "AAAAother".into());
        assert_eq!(check(&store, "h", 22, &key), Verdict::Changed);
    }
}
