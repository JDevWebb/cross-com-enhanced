//! Asks the router to forward a UDP port to this PC: UPnP first, then PCP (RFC 6887, which some
//! newer routers offer instead), then NAT-PMP. The client keeps the game's peer-to-peer port
//! forwarded while the game runs (`keep`), so other players reach it directly even when their
//! router and this one can't punch through to each other; the connection test (`check`) forwards
//! a port briefly to see whether the router allows it.
//!
//! Only the router this PC routes through is asked, and an answer from any other host is ignored,
//! so another machine on the network can't pose as the router. UPnP is done here, search and
//! actions both: its answer's description address is only fetched when the answer came from the
//! router and the address is on the router, so nothing on the network can point this PC at
//! another host; and each action names the service the router offers (`WANIPConnection:2`, or
//! `WANPPPConnection:1` on a router that dials its internet itself), as stricter routers require.
//! A mapping whose "external" address isn't a public one (a router behind another NAT) is
//! refused: it would be no use to players outside.
//!
//! Adapted from 5th Echelon Enhanced's `portmap` (MIT, by the same author).

use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::UdpSocket;
use std::time::Duration;

const PMP_PORT: u16 = 5351;

/// What a forward carries: the game's UDP, or a hosted server's web side (TCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl Protocol {
    /// As UPnP names it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Udp => "UDP",
            Protocol::Tcp => "TCP",
        }
    }

    /// Its IP protocol number (PCP's).
    fn number(self) -> u8 {
        match self {
            Protocol::Udp => 17,
            Protocol::Tcp => 6,
        }
    }

    /// NAT-PMP's mapping request for it.
    fn pmp_opcode(self) -> u8 {
        match self {
            Protocol::Udp => 1,
            Protocol::Tcp => 2,
        }
    }
}

/// A port the router forwards to this PC. Dropping it leaves the mapping in
/// place until its lease ends; [`Mapping::remove`] takes it away now.
pub struct Mapping {
    /// The address and port players outside reach this PC on.
    pub public: SocketAddrV4,
    /// "UPnP", "PCP" or "NAT-PMP".
    pub how: &'static str,
    /// The port on this PC it goes to.
    local_port: u16,
    handle: Handle,
}

enum Handle {
    Upnp {
        service: Service,
        protocol: Protocol,
        port: u16,
    },
    Pmp {
        gateway: Ipv4Addr,
        local: Ipv4Addr,
        protocol: Protocol,
        port: u16,
    },
    Pcp {
        gateway: Ipv4Addr,
        local: Ipv4Addr,
        protocol: Protocol,
        port: u16,
        nonce: [u8; 12],
    },
}

impl Mapping {
    /// What it forwards.
    #[must_use]
    pub fn protocol(&self) -> Protocol {
        match &self.handle {
            Handle::Upnp { protocol, .. } | Handle::Pmp { protocol, .. } | Handle::Pcp { protocol, .. } => *protocol,
        }
    }

    /// The port on this PC it goes to.
    #[must_use]
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Asks the router to stop forwarding the port.
    pub fn remove(self) {
        match self.handle {
            Handle::Upnp { service, protocol, port } => {
                let _ = service.remove(protocol, port);
            }
            Handle::Pmp { gateway, local, protocol, port } => {
                let _ = pmp_map(gateway, local, protocol, port, 0, 0);
            }
            Handle::Pcp {
                gateway,
                local,
                protocol,
                port,
                nonce,
            } => {
                let _ = pcp_request(gateway, local, &pcp_map_request(local, nonce, protocol, port, 0, 0));
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
    map_with(Protocol::Udp, port, external, pinned, lease, description)
}

/// As [`map_as`], for `protocol`: a hosted server forwards TCP (its web side) as well as UDP.
pub fn map_with(protocol: Protocol, port: u16, external: u16, pinned: Option<Ipv4Addr>, lease: u32, description: &str) -> Result<Mapping, String> {
    let upnp_err = match upnp(protocol, port, external, pinned, lease, description) {
        Ok(m) => return Ok(m),
        Err(e) => e,
    };
    let pcp_err = match pcp(protocol, port, external, pinned, lease) {
        Ok(m) => return Ok(m),
        Err(e) => e,
    };
    pmp(protocol, port, external, pinned, lease).map_err(|pmp_err| format!("UPnP: {upnp_err}; PCP: {pcp_err}; NAT-PMP: {pmp_err}"))
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
/// How long each UPnP action may take, and the most read of its answer.
const ACTION_WAIT: Duration = Duration::from_secs(5);
const MAX_ANSWER: u64 = 64 * 1024;
/// The UPnP services that forward ports.
const WAN_SERVICES: [&str; 3] = [
    "urn:schemas-upnp-org:service:WANPPPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:1",
    "urn:schemas-upnp-org:service:WANIPConnection:2",
];

/// A router's port-forwarding service.
struct Service {
    /// The router, at the port its UPnP descriptions and actions are on.
    at: SocketAddrV4,
    /// Its type, which each action names: one of [`WAN_SERVICES`].
    kind: String,
    /// The path its actions are posted to.
    control: String,
    /// The actions it has.
    actions: Vec<String>,
}

/// Why a UPnP action didn't work.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// The router's own reason: its UPnP error code and description.
    Upnp(u16, String),
    /// No answer, or one that made no sense.
    Other(String),
}

impl Refused {
    fn code(&self) -> Option<u16> {
        match self {
            Refused::Upnp(code, _) => Some(*code),
            Refused::Other(_) => None,
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Upnp(code, what) if what.is_empty() => write!(f, "the router refused (UPnP error {code})"),
            Refused::Upnp(code, what) => write!(f, "the router refused (UPnP error {code}, {what})"),
            Refused::Other(why) => f.write_str(why),
        }
    }
}

/// UPnP's errors this code acts on (UPnP IGD's WANIPConnection specification).
const CONFLICT_IN_MAPPING_ENTRY: u16 = 718;
const ONLY_PERMANENT_LEASES_SUPPORTED: u16 = 725;

impl Service {
    /// The service's name without its namespace (`WANIPConnection:2`).
    fn name(&self) -> &str {
        self.kind.strip_prefix("urn:schemas-upnp-org:service:").unwrap_or(&self.kind)
    }

    fn has(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }

    /// Posts `action` with `args` (in the specification's order) to the service: its answer,
    /// or the router's reason for refusing.
    fn call(&self, action: &str, args: &[(&str, String)]) -> Result<xmltree::Element, Refused> {
        use std::io::Read as _;
        let mut body = format!(
            "<?xml version=\"1.0\"?>\r\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{}\">",
            self.kind
        );
        for (name, value) in args {
            body.push_str(&format!("<{name}>{}</{name}>", escape(value)));
        }
        body.push_str(&format!("</u:{action}></s:Body></s:Envelope>\r\n"));
        let url = format!("http://{}{}", self.at, self.control);
        let soap_action = format!("\"{}#{action}\"", self.kind);
        let failed = |e: &dyn std::fmt::Display| Refused::Other(format!("{action}: {e}"));
        let response = attohttpc::post(&url)
            .follow_redirects(false)
            .proxy_settings(attohttpc::ProxySettings::builder().build())
            .timeout(ACTION_WAIT)
            .header("Content-Type", "text/xml; charset=\"utf-8\"")
            .header("SOAPAction", soap_action.as_str())
            .text(body)
            .send()
            .map_err(|e| failed(&e))?;
        let (status, _, reader) = response.split();
        let mut text = Vec::new();
        reader.take(MAX_ANSWER).read_to_end(&mut text).map_err(|e| failed(&e))?;
        let root = xmltree::Element::parse(text.as_slice()).ok();
        if let Some(fault) = root.as_ref().and_then(|r| find(r, "UPnPError")) {
            let code = text_of(fault, "errorCode").and_then(|c| c.parse().ok()).unwrap_or(0);
            return Err(Refused::Upnp(code, text_of(fault, "errorDescription").unwrap_or_default()));
        }
        if !status.is_success() {
            return Err(failed(&format!("the router answered {status}")));
        }
        root.as_ref()
            .and_then(|r| find(r, &format!("{action}Response")))
            .cloned()
            .ok_or_else(|| failed(&"the router's answer made no sense"))
    }

    fn external_ip(&self) -> Result<Ipv4Addr, Refused> {
        let answer = self.call("GetExternalIPAddress", &[])?;
        let ip = text_of(&answer, "NewExternalIPAddress").unwrap_or_default();
        ip.parse()
            .map_err(|_| Refused::Other(format!("the router gave no internet address ({ip:?}): this connection of its may not be the one in use")))
    }

    /// Forwards `external` to `internal` for `lease` seconds (0: until taken away).
    fn add(&self, protocol: Protocol, external: u16, internal: SocketAddrV4, lease: u32, description: &str) -> Result<(), Refused> {
        self.call("AddPortMapping", &mapping_args(protocol, external, internal, lease, description)).map(|_| ())
    }

    /// Forwards a port the router picks (`external` if it can) to `internal`: the port.
    fn add_any(&self, protocol: Protocol, external: u16, internal: SocketAddrV4, lease: u32, description: &str) -> Result<u16, Refused> {
        let answer = self.call("AddAnyPortMapping", &mapping_args(protocol, external, internal, lease, description))?;
        text_of(&answer, "NewReservedPort")
            .and_then(|p| p.parse().ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| Refused::Other("AddAnyPortMapping: the router's answer had no port".into()))
    }

    fn remove(&self, protocol: Protocol, external: u16) -> Result<(), Refused> {
        let args = [
            ("NewRemoteHost", String::new()),
            ("NewExternalPort", external.to_string()),
            ("NewProtocol", protocol.name().into()),
        ];
        self.call("DeletePortMapping", &args).map(|_| ())
    }

    /// The router's mapping number `index`: (protocol, external port, internal client, internal
    /// port).
    fn entry(&self, index: u32) -> Result<(String, u16, String, u16), Refused> {
        let answer = self.call("GetGenericPortMappingEntry", &[("NewPortMappingIndex", index.to_string())])?;
        let text = |name: &str| text_of(&answer, name).unwrap_or_default();
        let port = |name: &str| text(name).parse().unwrap_or(0);
        Ok((text("NewProtocol"), port("NewExternalPort"), text("NewInternalClient"), port("NewInternalPort")))
    }
}

/// AddPortMapping's and AddAnyPortMapping's arguments, in the specification's order.
fn mapping_args(protocol: Protocol, external: u16, internal: SocketAddrV4, lease: u32, description: &str) -> Vec<(&'static str, String)> {
    vec![
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", external.to_string()),
        ("NewProtocol", protocol.name().into()),
        ("NewInternalPort", internal.port().to_string()),
        ("NewInternalClient", internal.ip().to_string()),
        ("NewEnabled", "1".into()),
        ("NewPortMappingDescription", description.into()),
        ("NewLeaseDuration", lease.to_string()),
    ]
}

/// `text` for an XML element's content.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The first element named `name` in `element` (itself included), whatever its namespace.
fn find<'a>(element: &'a xmltree::Element, name: &str) -> Option<&'a xmltree::Element> {
    if element.name == name {
        return Some(element);
    }
    element.children.iter().filter_map(|c| c.as_element()).find_map(|c| find(c, name))
}

/// The text of the first element named `name` in `element`.
fn text_of(element: &xmltree::Element, name: &str) -> Option<String> {
    find(element, name).map(|e| e.get_text().map(|t| t.trim().to_owned()).unwrap_or_default())
}

/// Finds the router's UPnP services: the standard search to the whole network (some routers
/// answer nothing else), then one sent to the router alone. Only an answer that came from the
/// router, pointing at a description on the router, is followed, and only to the router: another
/// device on the network can neither pose as it nor have this PC fetch from anywhere else.
fn find_router(router: Ipv4Addr, local: Ipv4Addr) -> Result<Vec<Service>, String> {
    let multicast = ssdp_search(local, SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900), router);
    let (port, path) = match multicast {
        Ok(found) => found,
        Err(first) => ssdp_search(local, SocketAddrV4::new(router, 1900), router).map_err(|direct| format!("{first}; asked directly: {direct}"))?,
    };
    services_at(router, port, &path)
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

/// The router's port-forwarding services, from its description at `path` on `port`: each one it
/// lists, in its order (a router may list a connection it doesn't use before the one it does).
/// Its `URLBase`, when it has one on the router, is where the services' own paths are.
fn services_at(router: Ipv4Addr, port: u16, path: &str) -> Result<Vec<Service>, String> {
    let (base, found) = wan_services(&fetch(router, port, path)?).ok_or("the router's description made no sense")?;
    let port = match base {
        Some(base) => on_router(&base, router, None)?.0,
        None => port,
    };
    let mut services = Vec::new();
    let mut why = "the router's description has no port-forwarding service".to_owned();
    for Listed { kind, scpd, control } in found {
        let service = (|| {
            let (_, scpd) = on_router(&scpd, router, Some(port))?;
            let (_, control) = on_router(&control, router, Some(port))?;
            let actions = actions(&fetch(router, port, &scpd)?).ok_or("the router's service description made no sense")?;
            Ok::<_, String>(Service {
                at: SocketAddrV4::new(router, port),
                kind,
                control,
                actions,
            })
        })();
        match service {
            Ok(s) => services.push(s),
            Err(e) => why = e,
        }
    }
    if services.is_empty() {
        return Err(why);
    }
    Ok(services)
}

/// A port-forwarding service as a device description lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    /// Its type (one of [`WAN_SERVICES`]), its description's URL and its control URL.
    kind: String,
    scpd: String,
    control: String,
}

/// A UPnP device description's `URLBase`, if any, and each port-forwarding service in it.
fn wan_services(description: &[u8]) -> Option<(Option<String>, Vec<Listed>)> {
    fn in_device(device: &xmltree::Element, found: &mut Vec<Listed>) {
        let services = device.get_child("serviceList").into_iter().flat_map(|l| l.children.iter().filter_map(|c| c.as_element()));
        for s in services.filter(|s| s.name == "service") {
            let text = |name: &str| s.get_child(name).and_then(|e| e.get_text()).map(|t| t.trim().to_owned());
            if let (Some(kind), Some(scpd), Some(control)) = (text("serviceType"), text("SCPDURL"), text("controlURL")) {
                if WAN_SERVICES.contains(&kind.as_str()) {
                    found.push(Listed { kind, scpd, control });
                }
            }
        }
        let devices = device.get_child("deviceList").into_iter().flat_map(|l| l.children.iter().filter_map(|c| c.as_element()));
        for d in devices.filter(|d| d.name == "device") {
            in_device(d, found);
        }
    }
    let root = xmltree::Element::parse(description).ok()?;
    let base = root.get_child("URLBase").and_then(|b| b.get_text()).map(|t| t.trim().to_owned()).filter(|b| !b.is_empty());
    let mut found = Vec::new();
    for device in root.children.iter().filter_map(|c| c.as_element()).filter(|d| d.name == "device") {
        in_device(device, &mut found);
    }
    Some((base, found))
}

/// The names of the actions in a UPnP service description.
fn actions(scpd: &[u8]) -> Option<Vec<String>> {
    let root = xmltree::Element::parse(scpd).ok()?;
    let list = root.get_child("actionList")?;
    Some(
        list.children
            .iter()
            .filter_map(|c| c.as_element())
            .filter(|a| a.name == "action")
            .filter_map(|a| a.get_child("name").and_then(|n| n.get_text()).map(|t| t.trim().to_owned()))
            .collect(),
    )
}

fn upnp(protocol: Protocol, port: u16, external_port: u16, pinned: Option<Ipv4Addr>, lease: u32, description: &str) -> Result<Mapping, String> {
    let router = default_gateway().ok_or("no default gateway")?;
    let local = pinned.or_else(|| local_ip_towards(router)).ok_or("no local address towards the router")?;
    forward_on(find_router(router, local)?, protocol, SocketAddrV4::new(local, port), external_port, lease, description)
}

/// Forwards `external_port` to `internal` on the first of `services` that has an internet
/// address and agrees: a router may list a connection it doesn't use before the one it does.
fn forward_on(services: Vec<Service>, protocol: Protocol, internal: SocketAddrV4, external_port: u16, lease: u32, description: &str) -> Result<Mapping, String> {
    let several = services.len() > 1;
    let mut why = Vec::new();
    for service in services {
        let tried = service.external_ip().map_err(|e| e.to_string()).and_then(|external| {
            usable_external(external)?;
            let mapped = map_on(&service, protocol, external_port, internal, lease, description)?;
            Ok(SocketAddrV4::new(external, mapped))
        });
        match tried {
            Ok(public) => {
                return Ok(Mapping {
                    public,
                    how: "UPnP",
                    local_port: internal.port(),
                    handle: Handle::Upnp {
                        service,
                        protocol,
                        port: public.port(),
                    },
                })
            }
            Err(e) if several => why.push(format!("{}: {e}", service.name())),
            Err(e) => why.push(e),
        }
    }
    Err(why.join("; "))
}

/// Forwards `external` (or, when the router won't, another port) to `internal` on `service`:
/// the port forwarded, or the router's reason for refusing the one asked for.
fn map_on(service: &Service, protocol: Protocol, external: u16, internal: SocketAddrV4, lease: u32, description: &str) -> Result<u16, String> {
    let first = match add_leased(service, protocol, external, internal, lease, description) {
        Ok(()) => return Ok(external),
        Err(e) => e,
    };
    // This PC's own mapping from an earlier game (one that didn't get to remove it) holds the
    // port for up to its lease; many routers then refuse any other mapping to the same address
    // and port. Only mappings to this PC's address and port are taken away.
    let entries = (0..MAX_ENTRIES).map_while(|i| service.entry(i).ok());
    let stale = stale_mappings(entries, protocol, *internal.ip(), internal.port());
    for old in &stale {
        let _ = service.remove(protocol, *old);
    }
    if !stale.is_empty() && add_leased(service, protocol, external, internal, lease, description).is_ok() {
        return Ok(external);
    }
    // Taken, e.g. by another PC on this network playing too: another port. Any other refusal
    // (not allowed for this PC, say) would refuse that as much, and its reason is the one given.
    if stale.is_empty() && first.code() != Some(CONFLICT_IN_MAPPING_ENTRY) {
        return Err(first.to_string());
    }
    if service.has("AddAnyPortMapping") {
        let any = match service.add_any(protocol, external, internal, lease, description) {
            Err(e) if e.code() == Some(ONLY_PERMANENT_LEASES_SUPPORTED) && lease != 0 => service.add_any(protocol, external, internal, 0, description),
            other => other,
        };
        if let Ok(port) = any {
            return Ok(port);
        }
    } else {
        for _ in 0..OTHER_PORTS {
            let other = random_port();
            if add_leased(service, protocol, other, internal, lease, description).is_ok() {
                return Ok(other);
            }
        }
    }
    Err(first.to_string())
}

/// Ports tried when the router can't pick one itself (it has no AddAnyPortMapping).
const OTHER_PORTS: usize = 4;

/// AddPortMapping for `lease` seconds; with none (until taken away) on a router that keeps only
/// those, as older ones do. Such a forward outlives the game: the next game's takes it away
/// (see [`stale_mappings`]).
fn add_leased(service: &Service, protocol: Protocol, external: u16, internal: SocketAddrV4, lease: u32, description: &str) -> Result<(), Refused> {
    match service.add(protocol, external, internal, lease, description) {
        Err(e) if e.code() == Some(ONLY_PERMANENT_LEASES_SUPPORTED) && lease != 0 => service.add(protocol, external, internal, 0, description),
        other => other,
    }
}

/// A port from the dynamic range (49152 to 65535).
fn random_port() -> u16 {
    let mut bytes = [0u8; 2];
    getrandom::fill(&mut bytes).expect("the system's random source");
    49_152 + u16::from_le_bytes(bytes) % 16_384
}

/// The most mapping entries read from a router's table.
const MAX_ENTRIES: u32 = 128;

/// The external ports of the router's `protocol` mappings to `local`:`port`, from its table's
/// (protocol, external port, internal client, internal port) entries.
fn stale_mappings(entries: impl Iterator<Item = (String, u16, String, u16)>, protocol: Protocol, local: Ipv4Addr, port: u16) -> Vec<u16> {
    entries
        .filter(|(kind, _, client, internal)| kind.eq_ignore_ascii_case(protocol.name()) && *internal == port && client.trim() == local.to_string())
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

fn pmp_map(gateway: Ipv4Addr, local: Ipv4Addr, protocol: Protocol, port: u16, external: u16, lifetime: u32) -> Result<u16, String> {
    let mut req = vec![0u8, protocol.pmp_opcode(), 0, 0];
    req.extend_from_slice(&port.to_be_bytes());
    req.extend_from_slice(&external.to_be_bytes());
    req.extend_from_slice(&lifetime.to_be_bytes());
    let reply = pmp_request(gateway, local, &req, 16)?;
    Ok(u16::from_be_bytes([reply[10], reply[11]]))
}

fn pmp(protocol: Protocol, port: u16, external_port: u16, pinned: Option<Ipv4Addr>, lease: u32) -> Result<Mapping, String> {
    let gateway = default_gateway().ok_or("no default gateway")?;
    let local = pinned.or_else(|| local_ip_towards(gateway)).ok_or("no local address towards the router")?;
    let reply = pmp_request(gateway, local, &[0, 0], 12)?;
    let external = Ipv4Addr::new(reply[8], reply[9], reply[10], reply[11]);
    usable_external(external)?;
    let mapped = pmp_map(gateway, local, protocol, port, external_port, lease)?;
    Ok(Mapping {
        public: SocketAddrV4::new(external, mapped),
        how: "NAT-PMP",
        local_port: port,
        handle: Handle::Pmp { gateway, local, protocol, port },
    })
}

/// A PCP MAP request (RFC 6887 §11.1) for UDP `port` on `local` to `external` (0: any) for
/// `lifetime` seconds (0: remove), with the mapping's `nonce`.
fn pcp_map_request(local: Ipv4Addr, nonce: [u8; 12], protocol: Protocol, port: u16, external: u16, lifetime: u32) -> Vec<u8> {
    let mut req = vec![2u8, 1, 0, 0];
    req.extend_from_slice(&lifetime.to_be_bytes());
    req.extend_from_slice(&local.to_ipv6_mapped().octets());
    req.extend_from_slice(&nonce);
    req.extend_from_slice(&[protocol.number(), 0, 0, 0]);
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

fn pcp(protocol: Protocol, port: u16, external_port: u16, pinned: Option<Ipv4Addr>, lease: u32) -> Result<Mapping, String> {
    let gateway = default_gateway().ok_or("no default gateway")?;
    let local = pinned.or_else(|| local_ip_towards(gateway)).ok_or("no local address towards the router")?;
    let nonce = pcp_nonce();
    let reply = pcp_request(gateway, local, &pcp_map_request(local, nonce, protocol, port, external_port, lease))?;
    let public = pcp_map_answer(&reply, &nonce)?;
    usable_external(*public.ip())?;
    Ok(Mapping {
        public,
        how: "PCP",
        local_port: port,
        handle: Handle::Pcp {
            gateway,
            local,
            protocol,
            port,
            nonce,
        },
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
        let req = super::pcp_map_request(local, nonce, Protocol::Udp, 2346, 2346, 3600);
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
        let entry = |protocol: &str, external, client: &str, internal| (protocol.to_string(), external, client.to_string(), internal);
        let table = vec![
            entry("UDP", 2346, "192.168.0.2", 2346),
            entry("udp", 41234, "192.168.0.2", 2346),
            entry("UDP", 13001, "192.168.0.7", 2346),
            entry("TCP", 2346, "192.168.0.2", 2346),
            entry("UDP", 3074, "192.168.0.2", 3074),
        ];
        assert_eq!(stale_mappings(table.clone().into_iter(), Protocol::Udp, local, 2346), [2346, 41234]);
        assert_eq!(stale_mappings(table.into_iter(), Protocol::Tcp, local, 2346), [2346], "TCP's own");
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

    /// A router that dials its internet itself listing that connection first, unused, and the
    /// one it uses (`WANIPConnection:2`) after.
    const DESCRIPTION: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:2</deviceType>
<serviceList><service><serviceType>urn:schemas-upnp-org:service:Layer3Forwarding:1</serviceType><SCPDURL>/L3F.xml</SCPDURL><controlURL>/ctl/L3F</controlURL></service></serviceList>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:2</deviceType><deviceList>
<device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType><SCPDURL>/WANPPPCn.xml</SCPDURL><controlURL>/ctl/PPPConn</controlURL></service></serviceList></device>
<device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:2</serviceType><SCPDURL>/WANIPCn.xml</SCPDURL><controlURL>/ctl/IPConn</controlURL></service></serviceList></device>
</deviceList></device></deviceList></device></root>"#;

    const SCPD: &str = r#"<?xml version="1.0"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0"><actionList>
<action><name>GetExternalIPAddress</name><argumentList><argument><name>NewExternalIPAddress</name><direction>out</direction></argument></argumentList></action>
<action><name>AddPortMapping</name></action>
<action><name>DeletePortMapping</name><argumentList><argument><name>NewRemoteHost</name><direction>in</direction></argument></argumentList></action>
</actionList></scpd>"#;

    #[test]
    fn reads_the_routers_descriptions() {
        let (base, found) = wan_services(DESCRIPTION.as_bytes()).unwrap();
        assert_eq!(base, None);
        let kinds: Vec<&str> = found.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(
            kinds,
            ["urn:schemas-upnp-org:service:WANPPPConnection:1", "urn:schemas-upnp-org:service:WANIPConnection:2"],
            "each, in order"
        );
        assert_eq!((found[1].scpd.as_str(), found[1].control.as_str()), ("/WANIPCn.xml", "/ctl/IPConn"));
        let with_base = DESCRIPTION.replace(
            "<device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:2",
            "<URLBase>http://192.168.1.1:5431/</URLBase><device><deviceType>x",
        );
        assert_eq!(wan_services(with_base.as_bytes()).unwrap().0.as_deref(), Some("http://192.168.1.1:5431/"));
        assert_eq!(wan_services(b"<root><device></device></root>"), Some((None, vec![])));
        assert_eq!(wan_services(b"not xml"), None);
        assert_eq!(actions(SCPD.as_bytes()).unwrap(), ["GetExternalIPAddress", "AddPortMapping", "DeletePortMapping"]);
    }

    /// One HTTP request off `stream`: its head and body (by its Content-Length).
    fn read_request(stream: &mut std::net::TcpStream) -> (String, String) {
        use std::io::Read as _;
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            data.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&data).into_owned();
            if let Some(end) = text.find("\r\n\r\n") {
                let length = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse().unwrap_or(0))
                    })
                    .unwrap_or(0usize);
                if n == 0 || data.len() >= end + 4 + length {
                    return (text[..end].to_owned(), text[end + 4..].to_owned());
                }
            } else if n == 0 {
                return (text, String::new());
            }
        }
    }

    /// The fake router's UPnP actions: the PPP connection isn't in use (no internet address),
    /// and the IP one takes only its own service type and only forwards with no lease (725).
    fn soap_answer(path: &str, head: &str, body: &str) -> (&'static str, String) {
        let kind = match path {
            "/ctl/PPPConn" => "urn:schemas-upnp-org:service:WANPPPConnection:1",
            "/ctl/IPConn" => "urn:schemas-upnp-org:service:WANIPConnection:2",
            _ => return ("404 Not Found", String::new()),
        };
        let fault = |code: u16, what: &str| {
            let xml = format!(
                "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>{what}</errorDescription></UPnPError></detail></s:Fault></s:Body></s:Envelope>"
            );
            ("500 Internal Server Error", xml)
        };
        let action = head
            .lines()
            .find_map(|l| {
                l.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("soapaction"))
                    .map(|(_, v)| v.trim().trim_matches('"').to_owned())
            })
            .unwrap_or_default();
        let Some(action) = action.strip_prefix(&format!("{kind}#")).map(str::to_owned) else {
            return fault(401, "Invalid Action");
        };
        if !body.contains(&format!("xmlns:u=\"{kind}\"")) {
            return fault(401, "Invalid Action");
        }
        let answer = |inner: &str| {
            let xml = format!("<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body><u:{action}Response xmlns:u=\"{kind}\">{inner}</u:{action}Response></s:Body></s:Envelope>");
            ("200 OK", xml)
        };
        match (path, action.as_str()) {
            ("/ctl/PPPConn", "GetExternalIPAddress") => answer("<NewExternalIPAddress></NewExternalIPAddress>"),
            ("/ctl/IPConn", "GetExternalIPAddress") => answer("<NewExternalIPAddress>203.0.114.7</NewExternalIPAddress>"),
            ("/ctl/IPConn", "AddPortMapping") if body.contains("<NewInternalClient>192.168.1.66</NewInternalClient>") => fault(606, "Action not authorized"),
            ("/ctl/IPConn", "AddPortMapping") if !body.contains("<NewLeaseDuration>0</NewLeaseDuration>") => fault(725, "OnlyPermanentLeasesSupported"),
            ("/ctl/IPConn", "AddPortMapping" | "DeletePortMapping") => answer(""),
            ("/ctl/IPConn", "GetGenericPortMappingEntry") => fault(713, "SpecifiedArrayIndexInvalid"),
            _ => fault(401, "Invalid Action"),
        }
    }

    /// A router on this PC: answers the search with `location` (from `from`), serves the
    /// descriptions over HTTP, and answers UPnP actions (`soap_answer`). Its search port and its
    /// HTTP port.
    fn fake_router(from: Ipv4Addr, location: impl Fn(u16) -> String + Send + 'static) -> (u16, u16) {
        use std::io::Write as _;
        let http = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let http_port = http.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in http.incoming().flatten() {
                let mut stream = stream;
                let (head, body) = read_request(&mut stream);
                let path = head.split(' ').nth(1).unwrap_or("").to_owned();
                let (status, body) = match path.as_str() {
                    "/rootDesc.xml" => ("200 OK", DESCRIPTION.to_owned()),
                    "/WANIPCn.xml" | "/WANPPPCn.xml" => ("200 OK", SCPD.to_owned()),
                    _ if head.starts_with("POST ") => soap_answer(&path, &head, &body),
                    _ => ("404 Not Found", String::new()),
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
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
        let services = services_at(router, port, &path).unwrap();
        assert_eq!(services.len(), 2);
        let ip = &services[1];
        assert_eq!(
            (ip.at, ip.control.as_str(), ip.name()),
            (SocketAddrV4::new(router, http), "/ctl/IPConn", "WANIPConnection:2")
        );
        assert!(ip.has("DeletePortMapping") && !ip.has("AddAnyPortMapping"));
    }

    /// Each action names the router's own service; the unused connection listed first is passed
    /// over; a router keeping only forwards with no lease gets one.
    #[test]
    fn forwards_on_the_connection_in_use_in_its_own_terms() {
        let router = Ipv4Addr::LOCALHOST;
        let (_, http) = fake_router(router, |_| String::new());
        let internal = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 2346);
        let mapping = forward_on(services_at(router, http, "/rootDesc.xml").unwrap(), Protocol::Udp, internal, 2346, 1800, "test").unwrap();
        assert_eq!((mapping.public, mapping.how), (SocketAddrV4::new(Ipv4Addr::new(203, 0, 114, 7), 2346), "UPnP"));
        let Handle::Upnp { service, port, .. } = &mapping.handle else {
            panic!("a UPnP forward")
        };
        assert_eq!((service.name(), *port), ("WANIPConnection:2", 2346));
        mapping.remove();
    }

    /// The router's reason comes back as it gave it, not as "no free ports".
    #[test]
    fn says_why_the_router_refused() {
        let router = Ipv4Addr::LOCALHOST;
        let (_, http) = fake_router(router, |_| String::new());
        let internal = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 66), 2346);
        let e = forward_on(services_at(router, http, "/rootDesc.xml").unwrap(), Protocol::Udp, internal, 2346, 1800, "test")
            .err()
            .unwrap();
        assert!(e.contains("WANIPConnection:2: the router refused (UPnP error 606, Action not authorized)"), "{e}");
        assert!(e.contains("WANPPPConnection:1: the router gave no internet address"), "{e}");
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

    /// Against this network's real router: forwards a spare UDP port (`CROSSCOM_LIVE_PORT`,
    /// default 45346) to this machine through each way in turn, says what each did, and takes
    /// every forward away again. `cargo test -p crosscom-client-net live_router -- --ignored
    /// --nocapture`, on the router's network (in Docker: `--network host`).
    #[test]
    #[ignore = "changes the real router's forwards for a moment"]
    fn live_router() {
        let port = std::env::var("CROSSCOM_LIVE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(45_346);
        let router = default_gateway().expect("a default gateway");
        let local = local_ip_towards(router).expect("a local address");
        println!("router {router}, this machine {local}, port {port}");
        match find_router(router, local) {
            Ok(services) => {
                for s in services {
                    println!("UPnP service: {} at {}{} ({} actions)", s.name(), s.at, s.control, s.actions.len());
                }
            }
            Err(e) => println!("UPnP search: {e}"),
        }
        type Way = fn(u16, u16, Option<Ipv4Addr>, u32) -> Result<Mapping, String>;
        let ways: [(&str, Way); 6] = [
            ("UPnP UDP", |p, e, l, t| upnp(Protocol::Udp, p, e, l, t, "Cross-Com live test")),
            ("PCP UDP", |p, e, l, t| pcp(Protocol::Udp, p, e, l, t)),
            ("NAT-PMP UDP", |p, e, l, t| pmp(Protocol::Udp, p, e, l, t)),
            ("UPnP TCP", |p, e, l, t| upnp(Protocol::Tcp, p, e, l, t, "Cross-Com live test")),
            ("PCP TCP", |p, e, l, t| pcp(Protocol::Tcp, p, e, l, t)),
            ("NAT-PMP TCP", |p, e, l, t| pmp(Protocol::Tcp, p, e, l, t)),
        ];
        for (name, way) in ways {
            match way(port, port, Some(local), 120) {
                Ok(m) => {
                    println!("{name}: forwards {} ({})", m.public, m.how);
                    m.remove();
                }
                Err(e) => println!("{name}: {e}"),
            }
        }
    }

    #[test]
    fn only_public_external_addresses() {
        assert!(usable_external(Ipv4Addr::new(203, 0, 114, 7)).is_ok());
        for ip in [[192, 168, 1, 2], [10, 0, 0, 1], [100, 64, 0, 1], [0, 0, 0, 0]] {
            assert!(usable_external(Ipv4Addr::from(ip)).is_err(), "{ip:?}");
        }
    }
}
