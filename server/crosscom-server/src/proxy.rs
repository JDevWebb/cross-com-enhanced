//! Requests that come through a reverse proxy, such as Caddy giving the server HTTPS: who the
//! client is and whether it used HTTPS, from the headers the proxy adds.
//!
//! Only a proxy named in `trusted_proxies` is believed. From any other address the headers are
//! ignored, so a client can't claim to be someone else and get around the sign-in limits.
//!
//! - `X-Forwarded-For`: the client's address is its rightmost entry, the one the proxy itself
//!   added (entries left of it came from the client and could say anything).
//! - `X-Forwarded-Proto`: `https` when the client used HTTPS. Passwords are only taken through
//!   the proxy when it did.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

/// An address, or a network of them (`172.31.80.0/24`, `fd00::/8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    address: IpAddr,
    prefix: u8,
}

impl FromStr for Network {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let bad = || format!("{s:?} isn't an IP address or network (such as 172.31.80.10 or 172.31.80.0/24)");
        let (address, prefix) = match s.split_once('/') {
            Some((a, p)) => (a.parse::<IpAddr>().map_err(|_| bad())?, Some(p.parse::<u8>().map_err(|_| bad())?)),
            None => (s.parse::<IpAddr>().map_err(|_| bad())?, None),
        };
        let address = address.to_canonical();
        let bits = if address.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(bits);
        if prefix > bits {
            return Err(bad());
        }
        Ok(Self { address, prefix })
    }
}

impl Network {
    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        let bits = |a: IpAddr| match a {
            IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
            IpAddr::V6(v6) => (u128::from(v6), 128),
        };
        let (mine, width) = bits(self.address);
        let (theirs, their_width) = bits(address.to_canonical());
        if width != their_width {
            return false;
        }
        let shift = width - u32::from(self.prefix);
        shift >= width || (mine >> shift) == (theirs >> shift)
    }
}

/// The proxies whose headers are believed; none by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trusted(Vec<Network>);

impl Trusted {
    /// Reads `trusted_proxies`: addresses and networks.
    pub fn parse<S: AsRef<str>>(list: &[S]) -> Result<Self, String> {
        list.iter()
            .map(|s| s.as_ref().parse().map_err(|e| format!("trusted_proxies: {e}")))
            .collect::<Result<_, _>>()
            .map(Self)
    }

    #[must_use]
    pub fn trusts(&self, address: IpAddr) -> bool {
        self.0.iter().any(|n| n.contains(address))
    }
}

/// Who sent a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Client {
    /// The client's address: the connection's own, or the one a trusted proxy gives.
    pub address: IpAddr,
    /// Whether it came through a trusted proxy.
    pub proxied: bool,
    /// Whether the trusted proxy says the client used HTTPS (false for a straight connection).
    pub https: bool,
}

impl Client {
    /// Whether a password may be taken from it: always straight to the server (a LAN server
    /// without a proxy), through the proxy only over HTTPS.
    #[must_use]
    pub fn may_send_passwords(&self) -> bool {
        !self.proxied || self.https
    }
}

/// Who sent a request that came from `peer` with the headers in `head` (the request line and
/// headers, as received).
#[must_use]
pub fn client(peer: IpAddr, head: &str, trusted: &Trusted) -> Client {
    if !trusted.trusts(peer) {
        return Client {
            address: peer,
            proxied: false,
            https: false,
        };
    }
    let values = |name: &str| {
        head.lines()
            .skip(1)
            .filter_map(|l| l.split_once(':'))
            .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
            .flat_map(|(_, v)| v.split(','))
            .map(str::trim)
            .collect::<Vec<_>>()
    };
    let address = values("x-forwarded-for").last().and_then(|a| parse_address(a)).unwrap_or(peer);
    let https = values("x-forwarded-proto").last().is_some_and(|p| p.eq_ignore_ascii_case("https"));
    Client {
        address: address.to_canonical(),
        proxied: true,
        https,
    }
}

/// An `X-Forwarded-For` entry: an address, perhaps with a port (`[2001:db8::1]:443`).
fn parse_address(entry: &str) -> Option<IpAddr> {
    entry
        .parse::<IpAddr>()
        .ok()
        .or_else(|| entry.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| entry.strip_prefix('[')?.strip_suffix(']')?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn reads_addresses_and_networks() {
        let net: Network = "172.31.80.0/24".parse().unwrap();
        assert!(net.contains(ip("172.31.80.10")));
        assert!(net.contains(ip("::ffff:172.31.80.200")), "an IPv4 address seen on an IPv6 socket");
        assert!(!net.contains(ip("172.31.81.10")));
        assert!(!net.contains(ip("fd00::1")));
        let one: Network = " 172.31.80.10 ".parse().unwrap();
        assert!(one.contains(ip("172.31.80.10")));
        assert!(!one.contains(ip("172.31.80.11")));
        let v6: Network = "fd00::/8".parse().unwrap();
        assert!(v6.contains(ip("fd12:3456::1")));
        assert!(!v6.contains(ip("2001:db8::1")));
        let all: Network = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains(ip("198.51.100.1")));
        assert!("172.31.80.0/33".parse::<Network>().is_err());
        assert!("caddy".parse::<Network>().is_err());
        assert!("10.0.0.0/x".parse::<Network>().is_err());
        assert!(Trusted::parse(&["172.31.80.10", "nope"]).is_err());
        assert_eq!(Trusted::parse::<&str>(&[]).unwrap(), Trusted::default());
    }

    const PROXY: &str = "172.31.80.10";

    fn trusted() -> Trusted {
        Trusted::parse(&[PROXY]).unwrap()
    }

    fn head(headers: &str) -> String {
        format!("POST /api/login HTTP/1.1\r\nHost: play.example.org\r\n{headers}")
    }

    #[test]
    fn believes_only_the_trusted_proxy() {
        let h = head("X-Forwarded-For: 198.51.100.7\r\nX-Forwarded-Proto: https");
        let via_proxy = client(ip(PROXY), &h, &trusted());
        assert_eq!(
            via_proxy,
            Client {
                address: ip("198.51.100.7"),
                proxied: true,
                https: true
            }
        );
        assert!(via_proxy.may_send_passwords());

        // The same headers from anyone else are the client's own word, and ignored.
        let straight = client(ip("203.0.113.9"), &h, &trusted());
        assert_eq!(
            straight,
            Client {
                address: ip("203.0.113.9"),
                proxied: false,
                https: false
            }
        );
        assert!(straight.may_send_passwords(), "straight to a server on the LAN, as before");
        assert!(!client(ip(PROXY), &h, &Trusted::default()).proxied, "no proxy trusted unless named");
    }

    #[test]
    fn takes_the_entry_the_proxy_added() {
        let trusted = trusted();
        let address = |headers: &str| client(ip(PROXY), &head(headers), &trusted).address;
        // A client that sends its own header: the proxy adds the real address on the right.
        assert_eq!(address("X-Forwarded-For: 10.9.9.9, 198.51.100.7"), ip("198.51.100.7"));
        assert_eq!(address("x-forwarded-for: 10.9.9.9\r\nX-Forwarded-For: 198.51.100.7"), ip("198.51.100.7"));
        assert_eq!(address("X-Forwarded-For: 2001:db8::7"), ip("2001:db8::7"));
        assert_eq!(address("X-Forwarded-For: [2001:db8::7]:5000"), ip("2001:db8::7"));
        assert_eq!(address("X-Forwarded-For: 198.51.100.7:5000"), ip("198.51.100.7"));
        assert_eq!(address("X-Forwarded-For: ::ffff:198.51.100.7"), ip("198.51.100.7"));
        // Without a usable header, the proxy's own address.
        assert_eq!(address(""), ip(PROXY));
        assert_eq!(address("X-Forwarded-For: 198.51.100.7, unknown"), ip(PROXY));
    }

    #[test]
    fn takes_passwords_through_the_proxy_over_https_only() {
        let trusted = trusted();
        let https = |headers: &str| client(ip(PROXY), &head(headers), &trusted).may_send_passwords();
        assert!(https("X-Forwarded-Proto: https"));
        assert!(https("X-Forwarded-Proto: HTTPS"));
        assert!(!https("X-Forwarded-Proto: http"));
        assert!(!https(""), "the proxy didn't say");
        assert!(!https("X-Forwarded-Proto: https, http"), "the proxy's own entry is the last");
    }
}
