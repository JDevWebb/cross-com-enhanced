/// Kerberos-related functionality for authentication.
use std::io::Cursor;

use hmac::digest::Update;
use hmac::Hmac;
use hmac::Mac;
use md5::Digest;
use md5::Md5;

use crate::prudp::packet::Rc4;
use crate::rmc::basic::FromStream;
use crate::rmc::basic::FromStreamError;
use crate::rmc::basic::ReadStream;
use crate::rmc::basic::ToStream;
use crate::secretbox;

/// The size of the session key in bytes.
pub const SESSION_KEY_SIZE: usize = 16;

/// The internal representation of a Kerberos ticket.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, ToStream, FromStream)]
pub struct KerberosTicketInternal {
    /// The principal ID.
    pub principle_id: u32,
    /// The expiration time of the ticket.
    pub valid_until: u64,
    /// The session key.
    pub session_key: [u8; SESSION_KEY_SIZE],
    /// The address that asked for the ticket (IPv4 as IPv4-mapped IPv6): only a connection
    /// from there may use it.
    pub issued_to: [u8; 16],
}

impl KerberosTicketInternal {
    /// Records `ip` as the address the ticket is for.
    #[must_use]
    pub fn for_address(mut self, ip: std::net::IpAddr) -> Self {
        self.issued_to = crate::prudp::address_bytes(ip);
        self
    }

    /// Whether a connection from `ip` may use the ticket.
    #[must_use]
    pub fn is_for(&self, ip: std::net::IpAddr) -> bool {
        self.issued_to == crate::prudp::address_bytes(ip)
    }

    /// Whether `ip` is next to the address the ticket was issued to: the same /24 (IPv4) or
    /// /64 (IPv6). A VPN or a carrier's NAT sends a PC's connections out of neighbouring
    /// addresses (Cloudflare WARP: Mailz on eu1, 2026-10-05, asked for the ticket from
    /// 104.28.198.246 and used it from .247), so their game couldn't sign in. Someone replaying
    /// a ticket they saw would have to send from inside the player's own range too.
    #[must_use]
    pub fn is_near(&self, ip: std::net::IpAddr) -> bool {
        let other = crate::prudp::address_bytes(ip);
        let v4_mapped = |a: &[u8; 16]| a[..10] == [0; 10] && a[10..12] == [0xff, 0xff];
        match (v4_mapped(&self.issued_to), v4_mapped(&other)) {
            (true, true) => self.issued_to[..15] == other[..15],
            (false, false) => self.issued_to[..8] == other[..8],
            _ => false,
        }
    }

    /// Seals the ticket using a secret key.
    fn seal(&self, key: &secretbox::Key) -> Vec<u8> {
        secretbox::seal(&self.to_bytes(), key)
    }

    /// Opens a sealed ticket using a secret key.
    pub fn open(data: &[u8], key: &secretbox::Key) -> Result<Self, FromStreamError> {
        if data.len() < secretbox::NONCEBYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("got {} bytes, required at least {}", data.len(), secretbox::NONCEBYTES),
            )
            .into());
        }
        let data = secretbox::open(data, key).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "open failed"))?;
        let mut stream = ReadStream::from_bytes(data);
        stream.read()
    }
}

/// A Kerberos ticket.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug)]
pub struct KerberosTicket {
    /// The session key.
    pub session_key: [u8; SESSION_KEY_SIZE],
    /// The principal ID.
    pub pid: u32,
    /// The internal ticket.
    pub internal: KerberosTicketInternal,
}

/// Derived keys kept (see [`KerberosTicket::derive_key`]): every key the server makes is from one
/// of two passwords (players' tickets use the game's default, the telemetry account's its own),
/// and a key depends on the PID only by `pid % 1024`, so 2048 are all of them.
const DERIVED_KEYS_KEPT: usize = 2048;

/// Derived keys by password and round count.
type DerivedKeys = std::collections::HashMap<(String, u32), Vec<u8>>;

/// Keys made by [`KerberosTicket::derive_key`].
fn derived_keys() -> std::sync::MutexGuard<'static, DerivedKeys> {
    static KEYS: std::sync::LazyLock<std::sync::Mutex<DerivedKeys>> = std::sync::LazyLock::new(Default::default);
    KEYS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl KerberosTicket {
    /// Derives a key from a peer PID and a password: some 65,000 rounds of MD5, so each ticket
    /// cost the server that much, and signing in as the telemetry account (whose password is
    /// public) over and over could keep it busy. It depends only on the password and
    /// `pid % 1024`, so keys once made are kept (at most [`DERIVED_KEYS_KEPT`]).
    fn derive_key(peer_pid: u32, password: Option<&str>) -> Vec<u8> {
        let count = 65000 + (peer_pid % 1024);
        let password = password.unwrap_or("UbiDummyPwd");
        if let Some(key) = derived_keys().get(&(password.to_owned(), count)) {
            return key.clone();
        }
        let key = Self::derive_key_uncached(count, password);
        let mut keys = derived_keys();
        if keys.len() >= DERIVED_KEYS_KEPT {
            keys.clear();
        }
        keys.insert((password.to_owned(), count), key.clone());
        key
    }

    fn derive_key_uncached(count: u32, password: &str) -> Vec<u8> {
        let mut key = password.as_bytes().to_vec();
        for _ in 0..count {
            let h = Md5::new().chain(key).finalize();
            key = h.to_vec();
        }
        key
    }

    /// Reads a ticket made by [`Self::as_bytes`]: an error, never a panic, for one that's too
    /// short or whose MAC doesn't match.
    pub fn from_bytes(buf: &[u8], peer_pid: u32, password: Option<&str>, key: &secretbox::Key) -> Result<Self, FromStreamError> {
        let Some(off) = buf.len().checked_sub(Md5::output_size()) else {
            return Err(FromStreamError::MissingBytes(Md5::output_size() - buf.len()));
        };
        let (buf, mac) = buf.split_at(off);

        let obf_key = Self::derive_key(peer_pid, password);
        let mut check: Hmac<Md5> = Hmac::new_from_slice(&obf_key).expect("any key length");
        Mac::update(&mut check, buf);
        check
            .verify_slice(mac)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "ticket MAC mismatch"))?;
        let buf: Vec<u8> = Rc4::new(&obf_key).zip(buf).map(|(a, b)| a ^ b).collect();
        let mut rdr = ReadStream::from_reader(Cursor::new(buf));

        let session_key = FromStream::from_stream(&mut rdr)?;
        let pid = FromStream::from_stream(&mut rdr)?;
        // Written with its length in front (`Vec<u8>`'s `to_bytes`), as `as_bytes` does.
        let internal: Vec<u8> = rdr.read()?;
        let internal = KerberosTicketInternal::open(&internal, key)?;

        Ok(Self { session_key, pid, internal })
    }

    /// Converts the ticket to a byte vector.
    #[must_use]
    pub fn as_bytes(&self, peer_pid: u32, password: Option<&str>, key: &secretbox::Key) -> Vec<u8> {
        let mut buf = self.session_key.to_vec();
        buf.append(&mut self.pid.to_bytes());
        buf.append(&mut self.internal.seal(key).to_bytes());

        let key = Self::derive_key(peer_pid, password);

        let mut buf: Vec<u8> = Rc4::new(&key).zip(&buf).map(|(a, b)| a ^ b).collect();

        let mut mac: Hmac<Md5> = Hmac::new_from_slice(&key).unwrap();
        Mac::update(&mut mac, &buf);
        buf.extend(mac.finalize().into_bytes());

        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_for_the_address_that_asked() {
        let key = secretbox::gen_key();
        let ticket = KerberosTicketInternal {
            principle_id: 1234,
            valid_until: 1,
            session_key: [7; SESSION_KEY_SIZE],
            issued_to: [0; 16],
        }
        .for_address("203.0.113.5".parse().unwrap());
        let opened = KerberosTicketInternal::open(&ticket.seal(&key), &key).unwrap();
        assert!(opened.is_for("203.0.113.5".parse().unwrap()));
        assert!(opened.is_for("::ffff:203.0.113.5".parse().unwrap()), "either way of writing it");
        assert!(!opened.is_for("203.0.113.6".parse().unwrap()));
    }

    /// A ticket reads back as written; a short one or one with a wrong MAC is an error.
    #[test]
    fn a_ticket_reads_back_and_a_bad_one_is_an_error() {
        let key = secretbox::gen_key();
        let ticket = KerberosTicket {
            session_key: [3; SESSION_KEY_SIZE],
            pid: 0x1000,
            internal: KerberosTicketInternal {
                principle_id: 0x1234,
                valid_until: 99,
                session_key: [3; SESSION_KEY_SIZE],
                issued_to: [0; 16],
            },
        };
        let bytes = ticket.as_bytes(0x1234, None, &key);
        let back = KerberosTicket::from_bytes(&bytes, 0x1234, None, &key).unwrap();
        assert_eq!((back.session_key, back.pid, back.internal.principle_id), ([3; SESSION_KEY_SIZE], 0x1000, 0x1234));
        assert!(KerberosTicket::from_bytes(&[1, 2, 3], 0x1234, None, &key).is_err(), "shorter than its MAC");
        assert!(KerberosTicket::from_bytes(&[], 0x1234, None, &key).is_err());
        let mut tampered = bytes.clone();
        tampered[0] ^= 1;
        assert!(KerberosTicket::from_bytes(&tampered, 0x1234, None, &key).is_err(), "MAC mismatch");
        assert!(KerberosTicket::from_bytes(&bytes, 0x1234, Some("JaDe!"), &key).is_err(), "another password's key");
    }

    /// A key comes out the same from the cache as made afresh, and differs by password and by
    /// `pid % 1024` only.
    #[test]
    fn derived_keys_are_kept_and_the_same() {
        let made = KerberosTicket::derive_key_uncached(65000 + 7, "UbiDummyPwd");
        assert_eq!(KerberosTicket::derive_key(7, None), made);
        assert_eq!(KerberosTicket::derive_key(7, None), made, "from the cache");
        assert_eq!(KerberosTicket::derive_key(7 + 1024, None), made, "the same pid % 1024");
        assert_ne!(KerberosTicket::derive_key(7, Some("JaDe!")), made);
        assert_ne!(KerberosTicket::derive_key(8, None), made);
    }

    #[test]
    fn a_neighbouring_address_is_near() {
        let t = |ip: &str| {
            KerberosTicketInternal {
                principle_id: 1,
                valid_until: 1,
                session_key: [0; SESSION_KEY_SIZE],
                issued_to: [0; 16],
            }
            .for_address(ip.parse().unwrap())
        };
        // Mailz behind Cloudflare WARP.
        let warp = t("104.28.198.246");
        assert!(warp.is_near("104.28.198.247".parse().unwrap()));
        assert!(warp.is_near("::ffff:104.28.198.1".parse().unwrap()));
        assert!(!warp.is_near("104.28.199.246".parse().unwrap()), "another /24");
        assert!(!warp.is_near("2001:db8::1".parse().unwrap()), "v4 and v6 aren't neighbours");
        let v6 = t("2001:db8:1:2::10");
        assert!(v6.is_near("2001:db8:1:2:ffff::1".parse().unwrap()));
        assert!(!v6.is_near("2001:db8:1:3::10".parse().unwrap()), "another /64");
    }
}
