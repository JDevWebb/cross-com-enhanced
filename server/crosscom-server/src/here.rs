//! Where the server is, as each client reaches it, and where its players are, as everyone else
//! does: for a server at home, on the same network as some of its players.
//!
//! A server on the internet is set up with its public address (`public_address`), and players
//! elsewhere reach it there. Its host's own game, and players on the host's network, reach it on
//! this machine or at its address on that network instead: sent to the public address, they'd
//! go out through the router and back in, which many routers don't do, and the server would see
//! them come from the router, not from where their tickets were issued (each ticket holds the
//! address it was issued to). So each client is given the server's address as it reaches it
//! ([`server_for`]): the online config's sign-in address and the secure server's.
//!
//! Players elsewhere see a player on the server's network at the network's public address, the
//! server's own (behind the same router), not at the private address the server sees it at
//! ([`seen`]): their games are given that, with the player's port, and players on the network
//! get its addresses there first, as players behind one router do (`Sessions::urls_seen_from`).
//!
//! Only for a server whose public address is a public one: a server set up by its address on a
//! LAN or VPN is reached there by all, as before. A player on a VPN reaches the server at its
//! address there too. For everyone else, a player on a VPN whose addresses are private (a
//! ZeroTier network's, say) counts as on the server's network; one on Tailscale's
//! (100.64.0.0/10) doesn't, and is seen where it is.

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::sync::OnceLock;

/// The server's public address, once it's serving with one.
static PUBLIC: OnceLock<IpAddr> = OnceLock::new();
/// Whether every client on this machine, a LAN or a VPN gets the server's address there, even for
/// a server whose public address isn't a public one (`answer_as_reached`).
static AS_REACHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Has every client on this machine, a LAN or a VPN given the server's address there
/// (`answer_as_reached`: a server hosted from the launcher for its network and VPN alone).
pub fn set_as_reached(on: bool) {
    AS_REACHED.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Notes the server's public address, as it starts serving.
pub fn set_public(public: IpAddr) {
    let _ = PUBLIC.set(public.to_canonical());
}

/// The server's public address, when it's serving with one that's public.
#[must_use]
pub fn public() -> Option<IpAddr> {
    PUBLIC.get().copied().filter(|ip| is_public(*ip))
}

/// Whether `ip` could be reached from anywhere on the internet: not this machine, nor a private,
/// shared (carrier-grade NAT, Tailscale) or link-local address.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            let shared = a == 100 && (64..128).contains(&b);
            !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || shared || a == 0 || a >= 224)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            !(v6.is_loopback() || v6.is_unspecified() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80)
        }
    }
}

/// Whether `ip` is this machine, or on a network of its own (a private or link-local address):
/// a client there reaches the server without going through the router.
fn on_own_network(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// This machine's address on the network towards `to` (no packet is sent).
fn local_ip_towards(to: IpAddr) -> Option<IpAddr> {
    let bind: SocketAddr = match to {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect((to, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip()).filter(|ip| !ip.is_unspecified())
}

/// The server's address as `client` reaches it, for a server whose public address is `public`:
/// from this machine, a network of its own or a VPN (any address that isn't a public one), its
/// address there; from the internet, the public one.
/// With `as_reached`, so for a server whose public address isn't a public one too.
#[must_use]
pub fn server_for(public: IpAddr, client: IpAddr, as_reached: bool) -> IpAddr {
    let client = client.to_canonical();
    if !(is_public(public) || as_reached) || is_public(client) {
        return public;
    }
    if client.is_loopback() {
        return client;
    }
    local_ip_towards(client).unwrap_or(public)
}

/// [`server_for`] with the server's public address, once it's serving (`fallback` before then,
/// and in tests).
#[must_use]
pub fn server_as_reached(client: IpAddr, fallback: IpAddr) -> IpAddr {
    server_for(PUBLIC.get().copied().unwrap_or(fallback), client, AS_REACHED.load(std::sync::atomic::Ordering::Relaxed))
}

/// Where players elsewhere see a client the server sees at `addr`, for a server whose public
/// address is `public`: the public address, with its port, when the client is on the server's
/// network; else where it's seen.
#[must_use]
pub fn seen_with(public: Option<IpAddr>, addr: SocketAddr) -> SocketAddr {
    match public {
        Some(public) if is_public(public) && on_own_network(addr.ip()) => SocketAddr::new(public, addr.port()),
        _ => addr,
    }
}

/// [`seen_with`] the server's public address, once it's serving.
#[must_use]
pub fn seen(addr: SocketAddr) -> SocketAddr {
    seen_with(PUBLIC.get().copied(), addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn public_addresses() {
        for public in ["203.0.114.7", "144.202.110.74", "2001:4860::1"] {
            assert!(is_public(ip(public)), "{public}");
        }
        for private in [
            "192.168.1.7",
            "10.0.0.1",
            "172.20.0.1",
            "100.92.170.101",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
            "0.0.0.0",
        ] {
            assert!(!is_public(ip(private)), "{private}");
        }
        assert!(!is_public(ip("::ffff:192.168.1.7")), "an IPv4 address written as IPv6");
    }

    #[test]
    fn each_client_reaches_the_server_its_own_way() {
        let public = ip("203.0.114.7");
        assert_eq!(server_for(public, ip("198.51.100.20"), false), public, "from the internet");
        // From Tailscale: its address there (whatever it is here), not out through the internet.
        let vpn = server_for(public, ip("100.92.170.101"), false);
        assert!(vpn == public || !is_public(vpn), "{vpn}");
        assert_eq!(server_for(public, ip("127.0.0.1"), false), ip("127.0.0.1"), "from this machine");
        assert_eq!(server_for(public, ip("::ffff:127.0.0.1"), false), ip("127.0.0.1"));
        // From the network: this machine's address towards it (whatever it is here).
        let lan = server_for(public, ip("192.168.77.20"), false);
        assert!(lan == public || !is_public(lan), "{lan}");
        // As reached, a server set up on its LAN: its address on each network (a VPN's too).
        assert_eq!(server_for(ip("192.168.1.7"), ip("127.0.0.1"), true), ip("127.0.0.1"));
        let vpn = server_for(ip("192.168.1.7"), ip("100.92.170.101"), true);
        assert!(!is_public(vpn) && vpn != ip("100.92.170.101"), "{vpn}");
        assert_eq!(server_for(ip("192.168.1.7"), ip("198.51.100.20"), true), ip("192.168.1.7"), "from the internet: as set up");
        // A server set up on a LAN or VPN: there for everyone, as before.
        for configured in ["192.168.1.7", "100.92.170.101"] {
            assert_eq!(server_for(ip(configured), ip("127.0.0.1"), false), ip(configured));
            assert_eq!(server_for(ip(configured), ip("192.168.1.20"), false), ip(configured));
        }
    }

    #[test]
    fn players_on_its_network_are_seen_at_its_public_address() {
        let public = Some(ip("203.0.114.7"));
        let at = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert_eq!(seen_with(public, at("192.168.1.20:2346")), at("203.0.114.7:2346"));
        assert_eq!(seen_with(public, at("127.0.0.1:3074")), at("203.0.114.7:3074"), "the host's own game");
        assert_eq!(seen_with(public, at("198.51.100.20:41000")), at("198.51.100.20:41000"), "elsewhere: as seen");
        assert_eq!(seen_with(public, at("100.92.170.101:2346")), at("100.92.170.101:2346"), "on Tailscale");
        assert_eq!(seen_with(Some(ip("192.168.1.7")), at("192.168.1.20:2346")), at("192.168.1.20:2346"), "a LAN server");
        assert_eq!(seen_with(None, at("192.168.1.20:2346")), at("192.168.1.20:2346"));
    }
}
