//! Sealing tickets: XSalsa20-Poly1305 with a random nonce (`NaCl`'s `secretbox`), in pure Rust,
//! so the server builds for any target (libsodium's build can't cross-compile for Windows).
//!
//! A sealed box is the nonce followed by the ciphertext and its tag. Only the server that sealed
//! it opens it: the auth service seals the tickets it issues, the secure service opens them with
//! the same key, made afresh at each start.

use crypto_secretbox::aead::Aead;
use crypto_secretbox::KeyInit;
use crypto_secretbox::XSalsa20Poly1305;

/// The nonce's length, at the start of each sealed box.
pub const NONCEBYTES: usize = 24;

/// A key that seals and opens boxes (in settings, its 32 bytes).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Key([u8; 32]);

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(..)")
    }
}

/// A fresh random key.
#[must_use]
pub fn gen_key() -> Key {
    Key(rand::random())
}

/// `plain` sealed with `key`: the nonce, then the ciphertext and its tag.
#[must_use]
pub fn seal(plain: &[u8], key: &Key) -> Vec<u8> {
    let nonce: [u8; NONCEBYTES] = rand::random();
    let cipher = XSalsa20Poly1305::new(&key.0.into());
    let sealed = cipher.encrypt(&nonce.into(), plain).expect("sealing in memory doesn't fail");
    let mut out = Vec::with_capacity(NONCEBYTES + sealed.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    out
}

/// What `data` (as [`seal`] writes it) holds, or `None` if it's too short or wasn't sealed with
/// `key`.
#[must_use]
pub fn open(data: &[u8], key: &Key) -> Option<Vec<u8>> {
    if data.len() < NONCEBYTES {
        return None;
    }
    let (nonce, sealed) = data.split_at(NONCEBYTES);
    let nonce: [u8; NONCEBYTES] = nonce.try_into().ok()?;
    XSalsa20Poly1305::new(&key.0.into()).decrypt(&nonce.into(), sealed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_what_it_sealed_and_nothing_else() {
        let key = gen_key();
        let sealed = seal(b"a ticket", &key);
        assert_eq!(sealed.len(), NONCEBYTES + 8 + 16, "nonce, text, tag");
        assert_eq!(open(&sealed, &key).as_deref(), Some(&b"a ticket"[..]));
        assert_eq!(open(&sealed, &gen_key()), None, "another key");
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(open(&tampered, &key), None, "changed");
        assert_eq!(open(&sealed[..10], &key), None, "short");
        assert_ne!(seal(b"a ticket", &key), sealed, "a fresh nonce each time");
    }

    /// The same cipher as libsodium's `crypto_secretbox`: `NaCl`'s own test vector (its key and
    /// nonce, and the first 16 bytes of its message), whose ciphertext starts `8e 99 3b 9f`.
    #[test]
    fn is_nacls_secretbox() {
        let key = Key([
            0x1b, 0x27, 0x55, 0x64, 0x73, 0xe9, 0x85, 0xd4, 0x62, 0xcd, 0x51, 0x19, 0x7a, 0x9a, 0x46, 0xc7, 0x60, 0x09, 0x54, 0x9e, 0xac, 0x64, 0x74, 0xf2, 0x06, 0xc4, 0xee, 0x08,
            0x44, 0xf6, 0x83, 0x89,
        ]);
        let nonce = [
            0x69, 0x69, 0x6e, 0xe9, 0x55, 0xb6, 0x2b, 0x73, 0xcd, 0x62, 0xbd, 0xa8, 0x75, 0xfc, 0x73, 0xd6, 0x82, 0x19, 0xe0, 0x03, 0x6b, 0x7a, 0x0b, 0x37,
        ];
        let message = [0xbe, 0x07, 0x5f, 0xc5, 0x3c, 0x81, 0xf2, 0xd5, 0xcf, 0x14, 0x13, 0x16, 0xeb, 0xeb, 0x0c, 0x7b];
        let text = [0x8e, 0x99, 0x3b, 0x9f, 0x48, 0x68, 0x12, 0x73, 0xc2, 0x96, 0x50, 0xba, 0x32, 0xfc, 0x76, 0xce];
        let sealed = XSalsa20Poly1305::new(&key.0.into()).encrypt(&nonce.into(), &message[..]).unwrap();
        assert_eq!(sealed.len(), 32);
        assert!(sealed[16..] == text || sealed[..16] == text, "{sealed:02x?}");
        let mut boxed = nonce.to_vec();
        boxed.extend_from_slice(&sealed);
        assert_eq!(open(&boxed, &key).as_deref(), Some(&message[..]));
    }
}
