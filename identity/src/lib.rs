//! A player's identity: how players have accounts without passwords.
//!
//! The launcher makes an Ed25519 key once and keeps it (encrypted for the Windows user). Its
//! public half, in base32, is the player's global id. Signing proves the player holds it:
//!
//! * making an account on a server ([`signup_message`]): the account belongs to the identity;
//! * signing in to it ([`login_message`]): the server answers a game token, which the game signs
//!   in with (see the server's `accounts`).
//!
//! Each message names the server by the host the player reached it as (as they set it up, never
//! what a server says about itself) and a time, so a signature is good for one server, once, for
//! a few minutes: a server pretending to be another can't use what players sign for it anywhere
//! else. Ported from 5th Echelon Enhanced's identity, with messages of Cross-Com's own, so a
//! signature for one project's server is never taken by the other's.

use ring::signature::Ed25519KeyPair;
use ring::signature::KeyPair as _;

/// How far a signed time may be from the server's clock, in seconds.
pub const MAX_CLOCK_SKEW: i64 = 5 * 60;

/// The player's key.
pub struct Identity {
    seed: [u8; 32],
    pair: Ed25519KeyPair,
}

impl Identity {
    /// A new random identity.
    ///
    /// # Panics
    /// When the system has no random source.
    #[must_use]
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("the system's random source");
        Self::from_secret(&seed)
    }

    /// The identity with this secret (from [`Identity::secret`]).
    ///
    /// # Panics
    /// Never: any 32 bytes are an Ed25519 seed.
    #[must_use]
    pub fn from_secret(secret: &[u8; 32]) -> Self {
        let pair = Ed25519KeyPair::from_seed_unchecked(secret).expect("any 32 bytes are a seed");
        Self { seed: *secret, pair }
    }

    /// The secret, for saving. Whoever has it *is* this player.
    #[must_use]
    pub fn secret(&self) -> [u8; 32] {
        self.seed
    }

    /// The public key in base32: the player's global id.
    #[must_use]
    pub fn global_id(&self) -> String {
        base32_encode(self.pair.public_key().as_ref())
    }

    /// Signs `message`; the signature in base32.
    #[must_use]
    pub fn sign(&self, message: &str) -> String {
        base32_encode(self.pair.sign(message.as_bytes()).as_ref())
    }

    /// What a server needs to make the account `username` for this identity.
    #[must_use]
    pub fn sign_signup(&self, host: &str, username: &str, time: i64) -> String {
        self.sign(&signup_message(host, username, time))
    }

    /// What a server needs to sign this identity in to its account.
    #[must_use]
    pub fn sign_login(&self, host: &str, time: i64) -> String {
        self.sign(&login_message(host, time))
    }
}

/// The form of a server's host name that is signed: trimmed, lower-cased, without a port.
#[must_use]
pub fn host_key(host: &str) -> String {
    let host = host.trim().to_lowercase();
    // "name:port" (not an IPv6 address, which has more than one colon).
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
        _ => host,
    }
}

/// Whether a field may go into a signed message: printable ASCII, no separators, so no two
/// messages read the same.
#[must_use]
pub fn valid_field(s: &str) -> bool {
    !s.is_empty() && s.len() <= 253 && s.bytes().all(|b| b.is_ascii_graphic())
}

/// What a player signs to make the account `username`, owned by their identity, on the server
/// they reached as `host`.
#[must_use]
pub fn signup_message(host: &str, username: &str, time: i64) -> String {
    format!("cross-com/signup/v1\n{}\n{username}\n{time}", host_key(host))
}

/// What a player signs to sign in to their account on `host`.
#[must_use]
pub fn login_message(host: &str, time: i64) -> String {
    format!("cross-com/login/v1\n{}\n{time}", host_key(host))
}

/// Whether `signature` (base32) is the identity `global_id`'s over `message`.
#[must_use]
pub fn verify(global_id: &str, message: &str, signature: &str) -> bool {
    let (Some(public), Some(signature)) = (base32_decode(global_id), base32_decode(signature)) else {
        return false;
    };
    public.len() == 32
        && signature.len() == 64
        && ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public)
            .verify(message.as_bytes(), &signature)
            .is_ok()
}

/// Whether a signed `time` is close enough to `now` (see [`MAX_CLOCK_SKEW`]).
#[must_use]
pub fn fresh(time: i64, now: i64) -> bool {
    (time - now).abs() <= MAX_CLOCK_SKEW
}

/// Whether `s` has a global id's form (a 32-byte key in base32).
#[must_use]
pub fn is_global_id(s: &str) -> bool {
    s.len() == 52 && base32_decode(s).is_some_and(|k| k.len() == 32)
}

/// A global id's first characters, to show (`ABCD-EFGH`).
#[must_use]
pub fn short(global_id: &str) -> String {
    let head: String = global_id.chars().take(8).collect();
    match head.len() {
        8 => format!("{}-{}", &head[..4], &head[4..]),
        _ => head,
    }
}

/// The time now, as signed: seconds since 1970.
#[must_use]
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// RFC 4648 base32 without padding.
#[must_use]
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    out
}

/// [`base32_encode`]'s text back to bytes (upper case only); none if it isn't.
#[must_use]
pub fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let value = u32::try_from(ALPHABET.iter().position(|&a| a == c)?).ok()?;
        buffer = (buffer << 5) | value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    // Leftover bits must be zero padding, so each text decodes from one value only.
    (bits < 5 && buffer & ((1 << bits) - 1) == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trips() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0xff; 32]] {
            assert_eq!(base32_decode(&base32_encode(data)).as_deref(), Some(data));
        }
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("mzxw6ytboi"), None, "upper case only");
        assert_eq!(base32_decode("MZXW6YTBOJ"), None, "leftover bits must be zero");
    }

    #[test]
    fn signatures_bind_server_name_and_time() {
        let id = Identity::generate();
        let gid = id.global_id();
        assert!(is_global_id(&gid));
        let sig = id.sign_login("Play.Example.org:80", 1_000);
        assert!(verify(&gid, &login_message("play.example.org", 1_000), &sig), "host as signed");
        assert!(!verify(&gid, &login_message("other.example.org", 1_000), &sig), "another server");
        assert!(!verify(&gid, &login_message("play.example.org", 1_001), &sig), "another time");
        assert!(!verify(&Identity::generate().global_id(), &login_message("play.example.org", 1_000), &sig), "another key");
        let signup = id.sign_signup("play.example.org", "Kozak", 1_000);
        assert!(verify(&gid, &signup_message("play.example.org", "Kozak", 1_000), &signup));
        assert!(!verify(&gid, &signup_message("play.example.org", "Pepper", 1_000), &signup), "another name");
        assert!(!verify(&gid, &login_message("play.example.org", 1_000), &signup), "a sign-up isn't a sign-in");
    }

    #[test]
    fn secret_restores_the_same_identity_and_times_are_checked() {
        let id = Identity::generate();
        assert_eq!(Identity::from_secret(&id.secret()).global_id(), id.global_id());
        assert_ne!(Identity::generate().global_id(), id.global_id());
        assert!(fresh(1_000, 1_000 + MAX_CLOCK_SKEW) && !fresh(1_000, 1_001 + MAX_CLOCK_SKEW));
        assert_eq!(host_key(" Play.Example.ORG:8080 "), "play.example.org");
        assert_eq!(host_key("[::1]"), "[::1]");
        assert!(valid_field("Kozak") && !valid_field("Ko zak") && !valid_field(""));
        assert_eq!(short(&id.global_id()).len(), 9);
    }
}
