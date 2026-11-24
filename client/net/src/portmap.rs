//! Asks the router to forward a UDP port to this PC: UPnP first, then PCP (RFC 6887, which some
//! newer routers offer instead), then NAT-PMP. The client keeps the game's peer-to-peer port
//! forwarded while the game runs (`keep`), so other players reach it directly even when their
//! router and this one can't punch through to each other; the connection test (`check`) forwards
//! a port briefly to see whether the router allows it.
//!
//! Only the router this PC routes through is asked, and an answer from any other host is ignored,
//! so another machine on the network can't pose as the router. UPnP's search is done here, not
//! by `igd_next`, so that its answer's description address is only fetched when the answer came
//! from the router and the address is on the router: nothing on the network can point this PC
//! at another host. A mapping whose "external" address
//! isn't a public one (a router behind another NAT) is refused: it would be no use to players
//! outside.
//!
//! Adapted from 5th Echelon Enhanced's `portmap` (MIT, by the same author).

use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::UdpSocket;
use std::time::Duration;

use igd_next::PortMappingProtocol;

const PMP_PORT: u16 = 5351;

/// A port the router forwards to this PC. Dropping it leaves the mapping in
/// place until its lease ends; [`Mapping::remove`] takes it away now.
pub struct Mapping {
    /// The address and port players outside reach this PC on.
    pub public: SocketAddrV4,
    /// "UPnP", "PCP" or "NAT-PMP".
    pub how: &'static str,
    handle: Handle,
}

enum Handle {
    Upnp { gateway: igd_next::Gateway, port: u16 },
    Pmp { gateway: Ipv4Addr, local: Ipv4Addr, port: u16 },
    Pcp { gateway: Ipv4Addr, local: Ipv4Addr, port: u16, nonce: [u8; 12] },
}

impl Mapping {
    /// Asks the router to stop forwarding the port.
    pub fn remove(self) {
        match self.handle {
            Handle::Upnp { gateway, port } => {
                let _ = gateway.remove_port(PortMappingProtocol::UDP, port);
            }
            Handle::Pmp { gateway, local, port } => {
                let _ = pmp_map(gateway, local, port, 0, 0);
            }
            Handle::Pcp { gateway, local, port, nonce } => {
                let _ = pcp_request(gateway, local, &pcp_map_request(local, nonce, port, 0, 0));
            }
        }
    }
}

/// Asks the router to forward UDP `port` to this PC (at `pinned`, or the
/// address this PC reaches the router from) for `lease` seconds. On failure,
/// says why for each way tried.
pub fn map(port: u16, pinned: Option<Ipv4Addr>, lease: u32, description: &str) -> Result<Mapping, String> {
    map_as(port, port, pinned, lease, description)
}

/// As [`map`], asking for `external` on the router's side (the port it already gives this PC's
/// `port`; see [`port_to_ask`]).
pub fn map_as(port: u16, external: u16, pinned: Option<Ipv4Addr>, lease: u32, description: &str) -> Result<Mapping, String> {
    let upnp_err = match upnp(port, external, pinned, lease, description) {
        Ok(m) => return Ok(m),
        Err(e) => e,
    };
    let pcp_err = match pcp(port, external, pinned, lease) {
        Ok(m) => return Ok(m),
        Err(e) => e,
    };
    pmp(port, external, pinned, lease).map_err(|pmp_err| format!("UPnP: {upnp_err}; PCP: {pcp_err}; NAT-PMP: {pmp_err}"))
}

/// The router-side port to ask for, for this PC's `local` port: the one the server saw this PC's
/// packets come from, when it saw them (`observed`). Most routers keep the port, so it's
/// `local`; one that gave another port may have done so because another device on the network
/// holds `local` there (two PCs playing behind one router). Asking for `local` then makes a
/// forward that collides with that device's traffic, and its packets go to the wrong PC; asking
/// for the port the router already gave this PC doesn't.
pub fn port_to_ask(local: u16, observed: Option<u16>) -> u16 {
    observed.filter(|p| *p != 0).unwrap_or(local)
}

/// This PC's address on the network towards `to` (no packet is sent).
pub fn local_ip_towards(to: Ipv4Addr) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect((to, 9)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(a) => Some(*a.ip()),
        SocketAddr::V6(_) => None,
    }
}

/// The UPnP search (SSDP), for an Internet Gateway Device.
const SSDP_SEARCH: &str = "M-SEARCH * HTTP/1.1\r\nHost: 239.255.255.250:1900\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nMan: \"ssdp:discover\"\r\nMX: 3\r\n\r\n";
/// How long each search waits for the router's answer.
const SSDP_WAIT: Duration = Duration::from_secs(3);
/// How long each of the router's descriptions may take to fetch, and the most read of one.
const DESCRIPTION_WAIT: Duration = Duration::from_secs(5);
const MAX_DESCRIPTION: u64 = 256 * 1024;
/// The UPnP services that forward ports.
const WAN_SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
];

/// Finds the router's UPnP service: the standard search to the whole network (some routers
/// answer nothing else), then one sent to the router alone. Only an answer that came from the
/// router, pointing at a description on the router, is followed, and only to the router: another
/// device on the network can neither pose as it nor have this PC fetch from anywhere else.
fn find_router(router: Ipv4Addr, local: Ipv4Addr) -> Result<igd_next::Gateway, String> {
    let multicast = ssdp_search(local, SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900), router);
    let (port, path) = match multicast {
        Ok(found) => found,
        Err(first) => ssdp_search(local, SocketAddrV4::new(router, 1900), router).map_err(|direct| format!("{first}; asked directly: {direct}"))?,
    };
    gateway_at(router, port, &path)
}

/// Sends the UPnP search to `to` from `local` and waits for the router's answer: the port and path
/// of its description. Answers from any other address, or pointing anywhere but the router, are
/// ignored.
fn ssdp_search(local: Ipv4Addr, to: SocketAddrV4, router: Ipv4Addr) -> Result<(u16, String), String> {
    let socket = UdpSocket::bind((local, 0)).map_err(|e| e.to_string())?;
    socket.send_to(SSDP_SEARCH.as_bytes(), to).map_err(|e| e.to_string())?;
    let started = std::time::Instant::now();
    let mut ignored = None;
    let mut buf = [0u8; 2048];
    while let Some(left) = SSDP_WAIT.checked_sub(started.elapsed()).filter(|l| !l.is_zero()) {
        socket.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
        let Ok((n, from)) = socket.recv_from(&mut buf) else { break };
        if from.ip() != std::net::IpAddr::V4(router) {
            ignored = Some(format!("{} answered, but the router is {router}; ignored", from.ip()));
            continue;
        }
        let answer = String::from_utf8_lossy(&buf[..n]);
        match location(&answer).map(|url| on_router(url, router, None)) {
            Some(Ok(found)) => return Ok(found),
            Some(Err(e)) => ignored = Some(e),
            None => {}
        }
    }
    Err(ignored.unwrap_or_else(|| "no answer from the router".into()))
}

/// The `LOCATION` an SSDP answer gives.
fn location(answer: &str) -> Option<&str> {
    answer.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("location").then(|| value.trim())
    })
}

/// The port and path of `url` when it's plain HTTP to `router` itself (and, given `port`, on
/// that port): `http://<router's address>[:port]/path`, nothing else in front. A path on its own
/// (`/ctl`, or `ctl`) is on the router at `port`.
fn on_router(url: &str, router: Ipv4Addr, port: Option<u16>) -> Result<(u16, String), String> {
    let refused = || format!("the router's answer points at {url:?}, not at the router; ignored");
    let http = url.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://"));
    let (url_port, path) = match port {
        _ if http => {
            let rest = &url[7..];
            let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
            let (host, url_port) = match authority.split_once(':') {
                Some((host, p)) => (host, p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(refused)?),
                None => (authority, 80),
            };
            if host.parse::<Ipv4Addr>().ok() != Some(router) {
                return Err(refused());
            }
            (url_port, path.to_owned())
        }
        Some(port) if !url.contains("://") && !url.starts_with("//") => (port, if url.starts_with('/') { url.to_owned() } else { format!("/{url}") }),
        _ => return Err(refused()),
    };
    if port.is_some_and(|p| p != url_port) || path.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '\\' | '#')) {
        return Err(refused());
    }
    Ok((url_port, path))
}

/// GETs `path` from the router at `port`: no proxy, no redirects, at most [`MAX_DESCRIPTION`].
fn fetch(router: Ipv4Addr, port: u16, path: &str) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let url = format!("http://{router}:{port}{path}");
    let response = attohttpc::get(&url)
        .follow_redirects(false)
        .proxy_settings(attohttpc::ProxySettings::builder().build())
        .timeout(DESCRIPTION_WAIT)
        .send()
        .map_err(|e| format!("couldn't fetch {url}: {e}"))?;
    let (status, _, reader) = response.split();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    let mut body = Vec::new();
    reader.take(MAX_DESCRIPTION).read_to_end(&mut body).map_err(|e| format!("couldn't fetch {url}: {e}"))?;
    Ok(body)
}

/// The router's port-forwarding service, from its description at `path` on `port`.
fn gateway_at(router: Ipv4Addr, port: u16, path: &str) -> Result<igd_next::Gateway, String> {
    let (scpd, control) = wan_service(&fetch(router, port, path)?).ok_or("the router's description has no port-forwarding service")?;
    let (_, scpd) = on_router(&scpd, router, Some(port))?;
    let (_, control) = on_router(&control, router, Some(port))?;
    let control_schema = actions(&fetch(router, port, &scpd)?).ok_or("the router's service description made no sense")?;
    Ok(igd_next::Gateway {
        addr: SocketAddr::V4(SocketAddrV4::new(router, port)),
        root_url: path.to_owned(),
        control_url: control,
        control_schema_url: scpd,
        control_schema,
    })
}

/// The first port-forwarding service in a UPnP device description: its description's URL and its
/// control URL.
fn wan_service(description: &[u8]) -> Option<(String, String)> {
    fn in_device(device: &xmltree::Element) -> Option<(String, String)> {
        let services = device.get_child("serviceList").into_iter().flat_map(|l| l.children.iter().filter_map(|c| c.as_element()));
        let found = services.filter(|s| s.name == "service").find_map(|s| {
            let text = |name: &str| s.get_child(name).and_then(|e| e.get_text()).map(|t| t.trim().to_owned());
            if !WAN_SERVICES.contains(&text("serviceType")?.as_str()) {
                return None;
            }
            Some((text("SCPDURL")?, text("controlURL")?))
        });
        found.or_else(|| {
            let devices = device.get_child("deviceList")?.children.iter().filter_map(|c| c.as_element());
            devices.filter(|d| d.name == "device").find_map(in_device)
        })
    }
    let root = xmltree::Element::parse(description).ok()?;
    root.children.iter().filter_map(|c| c.as_element()).filter(|d| d.name == "device").find_map(in_device)
}

/// The actions in a UPnP service description, each with the names of its arguments in.
fn actions(scpd: &[u8]) -> Option<std::collections::HashMap<String, Vec<String>>> {
    let root = xmltree::Element::parse(scpd).ok()?;
    let list = root.get_child("actionList")?;
    let text = |e: &xmltree::Element, name: &str| e.get_child(name).and_then(|c| c.get_text()).map(|t| t.trim().to_owned());
    Some(
        list.children
            .iter()
            .filter_map(|c| c.as_element())
            .filter(|a| a.name == "action")
            .filter_map(|action| {
                let args = action.get_child("argumentList")?.children.iter().filter_map(|c| c.as_element());
                let ins = args
                    .filter(|a| a.name == "argument" && text(a, "direction").as_deref() == Some("in"))
                    .filter_map(|a| text(a, "name"));
                Some((text(action, "name")?, ins.collect()))
            })
            .collect(),
    )
}

fn upnp(port: u16, external_port: u16, pinned: Option<Ipv4Addr>, lease: u32, description: &str) -> Result<Mapping, String> {
    let router = default_gateway().ok_or("no default gateway")?;
    let local = pinned.or_else(|| local_ip_towards(router)).ok_or("no local address towards the router")?;
    let gateway = find_router(router, local)?;
    let external = match gateway.get_external_ip().map_err(|e| e.to_string())? {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => return Err("IPv6 external address".into()),
    };
    usable_external(external)?;
    let internal = SocketAddr::V4(SocketAddrV4::new(local, port));
    let mapped = match gateway.add_port(PortMappingProtocol::UDP, external_port, internal, lease, description) {
        Ok(()) => external_port,
        Err(_) => {
            // This PC's own mapping from an earlier game (one that didn't get to remove it)
            // holds the port for up to its lease; many routers then refuse any other mapping
            // to the same address and port ("no free ports"). Only mappings to this PC's
            // address and port are taken away.
            let entries = (0..MAX_ENTRIES).map_while(|i| gateway.get_generic_port_mapping_entry(i).ok());
            let stale = stale_mappings(entries.map(|e| (e.protocol, e.external_port, e.internal_client, e.internal_port)), local, port);
            for external in &stale {
                let _ = gateway.remove_port(PortMappingProtocol::UDP, *external);
            }
            match gateway.add_port(PortMappingProtocol::UDP, external_port, internal, lease, description) {
                Ok(()) => external_port,
                // Taken, e.g. by another PC on this network playing too.
                Err(_) => gateway.add_any_port(PortMappingProtocol::UDP, internal, lease, description).map_err(|e| e.to_string())?,
            }
        }
    };
    Ok(Mapping {
        public: SocketAddrV4::new(external, mapped),
        how: "UPnP",
        handle: Handle::Upnp { gateway, port: mapped },
    })
}

/// The most mapping entries read from a router's table.
const MAX_ENTRIES: u32 = 128;

/// The external ports of the router's UDP mappings to `local`:`port`, from its table's
/// (protocol, external port, internal client, internal port) entries.
fn stale_mappings(entries: impl Iterator<Item = (PortMappingProtocol, u16, String, u16)>, local: Ipv4Addr, port: u16) -> Vec<u16> {
    entries
        .filter(|(protocol, _, client, internal)| *protocol == PortMappingProtocol::UDP && *internal == port && client.trim() == local.to_string())
        .map(|(_, external, _, _)| external)
        .collect()
}

/// Whether the router's "external" address could be one the internet reaches.
/// A private or shared (carrier-grade NAT) one means the mapping is no use,
/// and a made-up one would send other players somewhere else.
pub fn usable_external(ip: Ipv4Addr) -> Result<(), String> {
    let [a, b, ..] = ip.octets();
    let shared = a == 100 && (64..128).contains(&b);
    if ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || shared || a == 0 || a >= 240 {
        return Err(format!(
            "the router's external address {ip} isn't a public one (another router or the ISP's NAT is in front)"
        ));
    }
    Ok(())
}

/// The default gateway: the next hop towards the internet.
#[cfg(target_os = "windows")]
pub fn default_gateway() -> Option<Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::GetBestRoute;
    use windows::Win32::NetworkManagement::IpHelper::MIB_IPFORWARDROW;
    let mut row = MIB_IPFORWARDROW::default();
    let dest = u32::from_ne_bytes([8, 8, 8, 8]);
    if unsafe { GetBestRoute(dest, 0, &mut row) } != 0 {
        return None;
    }
    let hop = Ipv4Addr::from(row.dwForwardNextHop.to_ne_bytes());
    (!hop.is_unspecified()).then_some(hop)
}

/// The default gateway: the next hop towards the internet.
#[cfg(target_os = "linux")]
pub fn default_gateway() -> Option<Ipv4Addr> {
    gateway_from_route_table(&std::fs::read_to_string("/proc/net/route").ok()?)
}

/// The default gateway.
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn default_gateway() -> Option<Ipv4Addr> {
    None
}

/// The default route's gateway in `/proc/net/route` (hex, in host byte order).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gateway_from_route_table(table: &str) -> Option<Ipv4Addr> {
    const RTF_GATEWAY: u32 = 0x2;
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (dest, gateway, flags) = (f.get(1)?, f.get(2)?, u32::from_str_radix(f.get(3)?, 16).ok()?);
        if *dest != "00000000" || flags & RTF_GATEWAY == 0 {
            return None;
        }
        let hop = Ipv4Addr::from(u32::from_str_radix(gateway, 16).ok()?.to_le_bytes());
        (!hop.is_unspecified()).then_some(hop)
    })
}

fn pmp_request(gateway: Ipv4Addr, local: Ipv4Addr, request: &[u8], reply_len: usize) -> Result<Vec<u8>, String> {
    let socket = UdpSocket::bind((local, 0)).map_err(|e| e.to_string())?;
    socket.connect((gateway, PMP_PORT)).map_err(|e| e.to_string())?;
    let mut wait = Duration::from_millis(250);
    let mut buf = [0u8; 16];
    // RFC 6886: 250 ms, doubling; stop at 2 s here.
    while wait <= Duration::from_secs(2) {
        socket.send(request).map_err(|e| e.to_string())?;
        socket.set_read_timeout(Some(wait)).map_err(|e| e.to_string())?;
        if let Ok(n) = socket.recv(&mut buf) {
            if n >= reply_len && buf[0] == 0 && buf[1] == request[1] + 128 {
                let result = u16::from_be_bytes([buf[2], buf[3]]);
                if result != 0 {
                    return Err(format!("the router refused (result {result})"));
                }
                return Ok(buf[..n].to_vec());
            }
        }
        wait *= 2;
    }
    Err("no answer".into())
}

fn pmp_map(gateway: Ipv4Addr, local: Ipv4Addr, port: u16, external: u16, lifetime: u32) -> Result<u16, String> {
    let mut req = vec![0u8, 1, 0, 0];
    req.extend_from_slice(&port.to_be_bytes());
    req.extend_from_slice(&external.to_be_bytes());
    req.extend_from_slice(&lifetime.to_be_bytes());
    let reply = pmp_request(gateway, local, &req, 16)?;
    Ok(u16::from_be_bytes([reply[10], reply[11]]))
}

fn pmp(port: u16, external_port: u16, pinned: Option<Ipv4Addr>, lease: u32) -> Result<Mapping, String> {
    let gateway = default_gateway().ok_or("no default gateway")?;
    let local = pinned.or_else(|| local_ip_towards(gateway)).ok_or("no local address towards the router")?;
    let reply = pmp_request(gateway, local, &[0, 0], 12)?;
    let external = Ipv4Addr::new(reply[8], reply[9], reply[10], reply[11]);
    usable_external(external)?;
    let mapped = pmp_map(gateway, local, port, external_port, lease)?;
    Ok(Mapping {
        public: SocketAddrV4::new(external, mapped),
        how: "NAT-PMP",
        handle: Handle::Pmp { gateway, local, port },
    })
}

/// A PCP MAP request (RFC 6887 §11.1) for UDP `port` on `local` to `external` (0: any) for
/// `lifetime` seconds (0: remove), with the mapping's `nonce`.
fn pcp_map_request(local: Ipv4Addr, nonce: [u8; 12], port: u16, external: u16, lifetime: u32) -> Vec<u8> {
    let mut req = vec![2u8, 1, 0, 0];
    req.extend_from_slice(&lifetime.to_be_bytes());
    req.extend_from_slice(&local.to_ipv6_mapped().octets());
    req.extend_from_slice(&nonce);
    req.extend_from_slice(&[17, 0, 0, 0]);
    req.extend_from_slice(&port.to_be_bytes());
    req.extend_from_slice(&external.to_be_bytes());
    req.extend_from_slice(&[0u8; 16]);
    req
}

/// A fresh PCP mapping nonce (RFC 6887 11.1: random, so another host can't take over or remove
/// the mapping), from the system's random source.
fn pcp_nonce() -> [u8; 12] {
    let mut out = [0u8; 12];
    getrandom::fill(&mut out).expect("the system's random source");
    out
}

/// A PCP MAP answer for `nonce`: the assigned external address and port.
fn pcp_map_answer(reply: &[u8], nonce: &[u8; 12]) -> Result<SocketAddrV4, String> {
    if reply.len() < 60 || reply[0] != 2 || reply[1] != 0x81 {
        return Err("not a PCP MAP answer".into());
    }
    if reply[3] != 0 {
        return Err(format!("the router refused (result {})", reply[3]));
    }
    if reply[24..36] != nonce[..] {
        return Err("an answer to another request".into());
    }
    let port = u16::from_be_bytes([reply[42], reply[43]]);
    let ip: [u8; 16] = reply[44..60].try_into().map_err(|_| "short".to_string())?;
    let ip = std::net::Ipv6Addr::from(ip).to_ipv4_mapped().ok_or("an IPv6 address")?;
    Ok(SocketAddrV4::new(ip, port))
}

fn pcp_request(gateway: Ipv4Addr, local: Ipv4Addr, request: &[u8]) -> Result<Vec<u8>, String> {
    let socket = UdpSocket::bind((local, 0)).map_err(|e| e.to_string())?;
    socket.connect((gateway, PMP_PORT)).map_err(|e| e.to_string())?;
    let mut wait = Duration::from_millis(250);
    let mut buf = [0u8; 1100];
    while wait <= Duration::from_secs(2) {
        socket.send(request).map_err(|e| e.to_string())?;
        socket.set_read_timeout(Some(wait)).map_err(|e| e.to_string())?;
        if let Ok(n) = socket.recv(&mut buf) {
            return Ok(buf[..n].to_vec());
        }
        wait *= 2;
    }
    Err("no answer".into())
}

fn pcp(port: u16, external_port: u16, pinned: Option<Ipv4Addr>, lease: u32) -> Result<Mapping, String> {
    let gateway = default_gateway().ok_or("no default gateway")?;
    let local = pinned.or_else(|| local_ip_towards(gateway)).ok_or("no local address towards the router")?;
    let nonce = pcp_nonce();
    let reply = pcp_request(gateway, local, &pcp_map_request(local, nonce, port, external_port, lease))?;
    let public = pcp_map_answer(&reply, &nonce)?;
    usable_external(*public.ip())?;
    Ok(Mapping {
        public,
        how: "PCP",
        handle: Handle::Pcp { gateway, local, port, nonce },
    })
}

#[cfg(test)]
mod tests {

    #[test]
    fn pcp_nonces_are_random_and_differ() {
        let (a, b) = (super::pcp_nonce(), super::pcp_nonce());
        assert_ne!(a, b, "each mapping its own");
        assert!(a.iter().any(|x| *x != a[0]), "not one byte twelve times: {a:?}");
    }
    /// PCP (RFC 6887): a MAP request as the RFC lays it out, and its answer read back.
    #[test]
    fn pcp_map_requests_and_answers() {
        let local = Ipv4Addr::new(192, 168, 1, 20);
        let nonce = [9u8; 12];
        let req = super::pcp_map_request(local, nonce, 2346, 2346, 3600);
        assert_eq!(req.len(), 60);
        assert_eq!((req[0], req[1]), (2, 1), "version 2, MAP");
        assert_eq!(&req[4..8], &3600u32.to_be_bytes());
        assert_eq!(&req[8..24], &local.to_ipv6_mapped().octets());
        assert_eq!((req[36], u16::from_be_bytes([req[40], req[41]]), u16::from_be_bytes([req[42], req[43]])), (17, 2346, 2346));
        let mut answer = req.clone();
        answer[1] = 0x81;
        answer[3] = 0;
        answer[42..44].copy_from_slice(&41234u16.to_be_bytes());
        answer[44..60].copy_from_slice(&Ipv4Addr::new(203, 0, 113, 7).to_ipv6_mapped().octets());
        assert_eq!(super::pcp_map_answer(&answer, &nonce), Ok(SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 7), 41234)));
        assert!(super::pcp_map_answer(&answer, &[1; 12]).is_err(), "another nonce");
        answer[3] = 2;
        assert!(super::pcp_map_answer(&answer, &nonce).unwrap_err().contains("refused"));
        assert!(super::pcp_map_answer(&[0u8; 8], &nonce).is_err());
    }

    use super::*;

    #[test]
    fn asks_for_the_port_the_router_already_gives_this_pc() {
        assert_eq!(port_to_ask(2346, None), 2346, "not seen yet");
        assert_eq!(port_to_ask(2346, Some(2346)), 2346, "kept");
        assert_eq!(port_to_ask(2346, Some(41234)), 41234, "another PC holds 2346 at the router");
        assert_eq!(port_to_ask(2346, Some(0)), 2346);
    }

    #[test]
    fn reads_the_default_route() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                     eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert_eq!(gateway_from_route_table(table), Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(gateway_from_route_table("Iface\tDestination\n"), None);
    }

    #[test]
    fn only_this_pcs_own_mappings_are_stale() {
        let local = Ipv4Addr::new(192, 168, 0, 2);
        let entry = |protocol, external, client: &str, internal| (protocol, external, client.to_string(), internal);
        let table = vec![
            entry(PortMappingProtocol::UDP, 2346, "192.168.0.2", 2346),
            entry(PortMappingProtocol::UDP, 41234, "192.168.0.2", 2346),
            entry(PortMappingProtocol::UDP, 13001, "192.168.0.7", 2346),
            entry(PortMappingProtocol::TCP, 2346, "192.168.0.2", 2346),
            entry(PortMappingProtocol::UDP, 3074, "192.168.0.2", 3074),
        ];
        assert_eq!(stale_mappings(table.into_iter(), local, 2346), [2346, 41234]);
    }

    #[test]
    fn only_the_routers_own_description_is_fetched() {
        let router = Ipv4Addr::new(192, 168, 1, 1);
        let at = |url: &str| on_router(url, router, None);
        assert_eq!(at("http://192.168.1.1:5000/rootDesc.xml"), Ok((5000, "/rootDesc.xml".into())));
        assert_eq!(at("HTTP://192.168.1.1/desc"), Ok((80, "/desc".into())));
        assert_eq!(at("http://192.168.1.1:49152"), Ok((49152, "/".into())));
        for elsewhere in [
            "http://192.168.1.7:5000/rootDesc.xml",
            "http://192.168.1.1.evil.example/x",
            "http://192.168.1.1@evil.example/x",
            "http://evil.example/http://192.168.1.1/",
            "https://192.168.1.1/x",
            "http://192.168.1.1:0/x",
            "http://192.168.1.1:99999/x",
            "http://192.168.1.1/x@evil",
            "http://192.168.1.1/x y",
            "/rootDesc.xml",
            "file:///etc/passwd",
        ] {
            assert!(at(elsewhere).is_err(), "{elsewhere}");
        }
        // The description's own URLs: paths on the router, at the same port.
        let in_it = |url: &str| on_router(url, router, Some(5000));
        assert_eq!(in_it("/ctl/IPConn"), Ok((5000, "/ctl/IPConn".into())));
        assert_eq!(in_it("ctl/IPConn"), Ok((5000, "/ctl/IPConn".into())));
        assert_eq!(in_it("http://192.168.1.1:5000/ctl"), Ok((5000, "/ctl".into())));
        assert!(in_it("http://192.168.1.1:6000/ctl").is_err(), "another port");
        assert!(in_it("//evil.example/ctl").is_err());
        assert!(in_it("@evil.example/ctl").is_err());
    }

    #[test]
    fn reads_an_ssdp_answers_location() {
        let answer = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nlocation: http://192.168.1.1:5000/rootDesc.xml\r\nST: x\r\n\r\n";
        assert_eq!(location(answer), Some("http://192.168.1.1:5000/rootDesc.xml"));
        assert_eq!(location("HTTP/1.1 200 OK\r\n\r\n"), None);
    }

    const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
<serviceList><service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><SCPDURL>/L3F.xml</SCPDURL><controlURL>/ctl/L3F</controlURL></service></serviceList>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType><deviceList><device>
<serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><SCPDURL>/WANIPCn.xml</SCPDURL><controlURL>/ctl/IPConn</controlURL></service></serviceList>
</device></deviceList></device></deviceList></device></root>"#;

    const SCPD: &str = r#"<?xml version="1.0"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>
<action><name>GetExternalIPAddress</name><argumentList><argument><name>NewExternalIPAddress</name><direction>out</direction></argument></argumentList></action>
<action><name>DeletePortMapping</name><argumentList><argument><name>NewRemoteHost</name><direction>in</direction></argument><argument><name>NewExternalPort</name><direction>in</direction></argument><argument><name>NewProtocol</name><direction>in</direction></argument></argumentList></action>
</actionList></scpd>"#;

    #[test]
    fn reads_the_routers_descriptions() {
        assert_eq!(wan_service(DESCRIPTION.as_bytes()), Some(("/WANIPCn.xml".into(), "/ctl/IPConn".into())));
        assert_eq!(wan_service(b"<root><device></device></root>"), None);
        assert_eq!(wan_service(b"not xml"), None);
        let actions = actions(SCPD.as_bytes()).unwrap();
        assert_eq!(actions["DeletePortMapping"], ["NewRemoteHost", "NewExternalPort", "NewProtocol"]);
        assert!(actions["GetExternalIPAddress"].is_empty());
    }

    /// A router on this PC: answers the search with `location` (from `from`), and serves the
    /// descriptions over HTTP. Its search port and its HTTP port.
    fn fake_router(from: Ipv4Addr, location: impl Fn(u16) -> String + Send + 'static) -> (u16, u16) {
        use std::io::Read as _;
        use std::io::Write as _;
        let http = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let http_port = http.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in http.incoming().flatten() {
                let mut stream = stream;
                let mut request = [0u8; 2048];
                let n = stream.read(&mut request).unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let path = request.split(' ').nth(1).unwrap_or("");
                let body = match path {
                    "/rootDesc.xml" => DESCRIPTION,
                    "/WANIPCn.xml" => SCPD,
                    _ => "",
                };
                let status = if body.is_empty() { "404 Not Found" } else { "200 OK" };
                let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let ssdp = UdpSocket::bind((from, 0)).unwrap();
        let ssdp_port = ssdp.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok((_, asker)) = ssdp.recv_from(&mut buf) {
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nLOCATION: {}\r\n\r\n",
                    location(http_port)
                );
                let _ = ssdp.send_to(answer.as_bytes(), asker);
            }
        });
        (ssdp_port, http_port)
    }

    #[test]
    fn finds_the_router_answering_for_itself() {
        let router = Ipv4Addr::LOCALHOST;
        let (ssdp, http) = fake_router(router, |http| format!("http://127.0.0.1:{http}/rootDesc.xml"));
        let (port, path) = ssdp_search(router, SocketAddrV4::new(router, ssdp), router).unwrap();
        assert_eq!((port, path.as_str()), (http, "/rootDesc.xml"));
        let gateway = gateway_at(router, port, &path).unwrap();
        assert_eq!(gateway.addr, SocketAddr::V4(SocketAddrV4::new(router, http)));
        assert_eq!((gateway.control_url.as_str(), gateway.control_schema_url.as_str()), ("/ctl/IPConn", "/WANIPCn.xml"));
        assert!(gateway.control_schema.contains_key("DeletePortMapping"));
    }

    #[test]
    fn an_answer_pointing_elsewhere_is_ignored() {
        let router = Ipv4Addr::LOCALHOST;
        // The router's address, but its description somewhere else.
        let (ssdp, _) = fake_router(router, |_| "http://203.0.113.9/rootDesc.xml".into());
        let e = ssdp_search(router, SocketAddrV4::new(router, ssdp), router).unwrap_err();
        assert!(e.contains("not at the router"), "{e}");
    }

    #[test]
    fn an_answer_from_another_host_is_ignored() {
        // Another host on the network (here 127.0.0.2) answering, even with the router's address.
        let other = Ipv4Addr::new(127, 0, 0, 2);
        if UdpSocket::bind((other, 0)).is_err() {
            return; // No 127.0.0.2 here.
        }
        let (ssdp, _) = fake_router(other, |http| format!("http://127.0.0.1:{http}/rootDesc.xml"));
        let e = ssdp_search(Ipv4Addr::LOCALHOST, SocketAddrV4::new(other, ssdp), Ipv4Addr::LOCALHOST).unwrap_err();
        assert!(e.contains("127.0.0.2 answered, but the router is 127.0.0.1"), "{e}");
    }

    #[test]
    fn only_public_external_addresses() {
        assert!(usable_external(Ipv4Addr::new(203, 0, 114, 7)).is_ok());
        for ip in [[192, 168, 1, 2], [10, 0, 0, 1], [100, 64, 0, 1], [0, 0, 0, 0]] {
            assert!(usable_external(Ipv4Addr::from(ip)).is_err(), "{ip:?}");
        }
    }
}
