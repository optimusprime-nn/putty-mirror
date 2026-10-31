//! X11 login cookies, MIT and XDM-AUTHORIZATION-1, after PuTTY's `x11fwd.c`.

use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};

type Decrypt = cbc::Decryptor<des::Des>;
type Encrypt = cbc::Encryptor<des::Des>;

pub const MIT: &[u8] = b"MIT-MAGIC-COOKIE-1";
pub const XDM: &[u8] = b"XDM-AUTHORIZATION-1";
const MAX_SKEW: i64 = 20 * 60;

/// XDM keeps its DES key in 7 bytes. DES wants 8, each with the low bit unused.
fn expand_key(k: &[u8]) -> [u8; 8] {
    let (mut bits, mut nbits, mut j) = (0u32, 0, 0);
    let mut key = [0u8; 8];
    for slot in &mut key {
        if nbits < 7 {
            bits = bits << 8 | k[j] as u32;
            nbits += 8;
            j += 1;
        }
        *slot = ((bits >> (nbits - 7)) << 1) as u8;
        bits &= !(0x7f << (nbits - 7));
        nbits -= 7;
    }
    key
}

pub fn encrypt(key: &[u8], block: &mut [u8]) {
    let len = block.len();
    if let Ok(c) = Encrypt::new_from_slices(&expand_key(key), &[0; 8]) {
        let _ = c.encrypt_padded_mut::<NoPadding>(block, len);
    }
}

pub fn decrypt(key: &[u8], block: &mut [u8]) {
    if let Ok(c) = Decrypt::new_from_slices(&expand_key(key), &[0; 8]) {
        let _ = c.decrypt_padded_mut::<NoPadding>(block);
    }
}

/// A made-up XDM cookie from 15 random bytes: byte 8 has to be zero.
pub fn make_cookie(random: [u8; 15]) -> [u8; 16] {
    let mut c = [0u8; 16];
    c[..15].copy_from_slice(&random);
    c[15] = c[8];
    c[8] = 0;
    c
}

/// Replays of an XDM login, which has to be refused.
#[derive(Default)]
pub struct Seen(Vec<(u32, [u8; 6])>);

/// Checks a client's XDM login against the made-up cookie.
pub fn check(cookie: &[u8; 16], data: &[u8], peer: Option<([u8; 4], u16)>, now: i64, seen: &mut Seen) -> bool {
    let (Ok(data), Some((ip, port))) = (<[u8; 24]>::try_from(data), peer) else { return false };
    let mut d = data;
    decrypt(&cookie[9..], &mut d);
    let port = port.to_be_bytes();
    if d[..8] != cookie[..8] || d[8..12] != ip || d[12..14] != port || d[18..] != [0; 6] {
        return false;
    }
    let t = u32::from_be_bytes([d[14], d[15], d[16], d[17]]);
    if (t as i64 - now).abs() > MAX_SKEW {
        return false;
    }
    let mut client = [0u8; 6];
    client.copy_from_slice(&d[8..14]);
    if seen.0.contains(&(t, client)) {
        return false;
    }
    seen.0.retain(|(old, _)| (*old as i64 - now).abs() <= MAX_SKEW);
    seen.0.push((t, client));
    true
}

/// The login data to give a real display that uses XDM.
pub fn login_data(cookie: &[u8], peer: ([u8; 4], u16), now: i64) -> Vec<u8> {
    let mut d = [0u8; 24];
    if cookie.len() != 16 {
        return Vec::new();
    }
    d[..8].copy_from_slice(&cookie[..8]);
    d[8..12].copy_from_slice(&peer.0);
    d[12..14].copy_from_slice(&peer.1.to_be_bytes());
    d[14..18].copy_from_slice(&(now as u32).to_be_bytes());
    encrypt(&cookie[9..], &mut d);
    d.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn key_expansion_is_seven_bits_at_a_time() {
        assert_eq!(expand_key(&[0xff; 7]), [0xfe; 8]);
        assert_eq!(expand_key(&[0x80, 0, 0, 0, 0, 0, 0]), [0x80, 0, 0, 0, 0, 0, 0, 0]);
    }

    // From PuTTY's test/cryptsuite.py.
    #[test]
    fn matches_putty() {
        let plain = b"three AES blocks, or six DES, of arbitrary input";
        let cipher = unhex("5cac9999cffc980a1d1184d84b71c8cb313d12a1d25a7831179aeb11edaca5ad9482b224105a61c27137587620edcba8");
        let mut b = *plain;
        encrypt(b"7 bytes", &mut b);
        assert_eq!(&b[..], &cipher[..]);
        decrypt(b"7 bytes", &mut b);
        assert_eq!(&b, plain);
    }

    #[test]
    fn encryption_round_trips() {
        let key = unhex("0123456789abcd");
        let mut b = *b"12345678abcdefgh";
        encrypt(&key, &mut b);
        assert_ne!(&b, b"12345678abcdefgh");
        decrypt(&key, &mut b);
        assert_eq!(&b, b"12345678abcdefgh");
    }

    fn cookie() -> [u8; 16] {
        make_cookie(*b"abcdefgh1234567")
    }

    #[test]
    fn a_good_login_passes_once() {
        let c = cookie();
        assert_eq!(c[8], 0);
        let peer = ([127, 0, 0, 1], 4000);
        let data = login_data(&c, peer, 1000);
        let mut seen = Seen::default();
        assert!(check(&c, &data, Some(peer), 1000, &mut seen));
        assert!(!check(&c, &data, Some(peer), 1000, &mut seen), "a replay is refused");
    }

    #[test]
    fn bad_logins_fail() {
        let c = cookie();
        let peer = ([127, 0, 0, 1], 4000);
        let data = login_data(&c, peer, 1000);
        let mut seen = Seen::default();
        assert!(!check(&c, &data, Some(([10, 0, 0, 1], 4000)), 1000, &mut seen), "wrong address");
        assert!(!check(&c, &data, Some(peer), 1000 + MAX_SKEW + 1, &mut seen), "too old");
        assert!(!check(&c, &data[..23], Some(peer), 1000, &mut seen), "too short");
        assert!(!check(&c, &data, None, 1000, &mut seen), "no peer address");
    }
}
