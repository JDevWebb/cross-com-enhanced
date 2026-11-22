//! The server's address echo: where the server sees a UDP socket of this PC.
//!
//! The game asks it, on the server's sign-in ports, to learn the public address and port its
//! peer-to-peer socket has: a PRUDP user packet from virtual port 1 to virtual port 1. The server
//! answers with the packet's payload followed by `udp:/address=IP;port=PORT` and a NUL, sent
//! back raw. Asked from one socket at two of the server's ports, it shows whether the router
//! keeps one public port for the socket (other players can punch through to it) or gives each
//! destination its own (a symmetric NAT, which they mostly can't).

use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::UdpSocket;
use std::time::Duration;
use std::time::Instant;

/// The virtual port byte: port 1, stream type 7 (NAT echo).
const VPORT: u8 = 0x71;
/// PRUDP's user packet, with no flags.
const USER: u8 = 5;

/// The echo's packet, carrying `nonce` so its answer is known from anything else that arrives.
#[must_use]
pub fn packet(nonce: [u8; 8]) -> Vec<u8> {
    let mut data = vec![VPORT, VPORT, USER, 0];
    data.extend_from_slice(&0u32.to_le_bytes()); // signature
    data.extend_from_slice(&0u16.to_le_bytes()); // sequence
    data.extend_from_slice(&nonce);
    data.push(checksum(&data));
    data
}

/// PRUDP v0's checksum, with the key of a stream that isn't secure (0): the bytes of the sum of
/// the data's little-endian u32s, then the bytes left over.
fn checksum(data: &[u8]) -> u8 {
    let whole = data.len() - data.len() % 4;
    let sum = data[..whole]
        .chunks_exact(4)
        .fold(0u32, |acc, c| acc.wrapping_add(u32::from_le_bytes([c[0], c[1], c[2], c[3]])));
    let bytes = sum.to_le_bytes().iter().fold(0u8, |acc, b| acc.wrapping_add(*b));
    data[whole..].iter().fold(bytes, |acc, b| acc.wrapping_add(*b))
}

/// The address in an answer to the echo with `nonce`.
#[must_use]
pub fn answer(reply: &[u8], nonce: &[u8; 8]) -> Option<SocketAddrV4> {
    let text = reply.strip_prefix(nonce.as_slice())?;
    let text = std::str::from_utf8(text).ok()?.trim_end_matches('\0');
    let rest = text.strip_prefix("udp:/")?;
    let mut ip = None;
    let mut port = None;
    for field in rest.split(';') {
        match field.split_once('=') {
            Some(("address", v)) => ip = v.parse::<Ipv4Addr>().ok(),
            Some(("port", v)) => port = v.parse::<u16>().ok(),
            _ => {}
        }
    }
    Some(SocketAddrV4::new(ip?, port?))
}

/// A nonce for one question. Not secret: it only tells this answer from others.
#[must_use]
pub fn nonce() -> [u8; 8] {
    use std::hash::BuildHasher;
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.finish().to_le_bytes()
}

/// Where the server at `to` sees `socket`: asked up to three times within `wait`.
pub fn ask(socket: &UdpSocket, to: SocketAddr, wait: Duration) -> Result<SocketAddrV4, String> {
    let nonce = nonce();
    let request = packet(nonce);
    let started = Instant::now();
    let mut buf = [0u8; 256];
    for _ in 0..3 {
        socket.send_to(&request, to).map_err(|e| format!("couldn't send to {to}: {e}"))?;
        let round = Instant::now();
        while round.elapsed() < wait / 3 {
            let left = (wait / 3).saturating_sub(round.elapsed()).max(Duration::from_millis(10));
            socket.set_read_timeout(Some(left)).map_err(|e| e.to_string())?;
            match socket.recv_from(&mut buf) {
                Ok((n, from)) if from == to => {
                    if let Some(seen) = answer(&buf[..n], &nonce) {
                        return Ok(seen);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
    Err(format!("no answer from {to} in {:.0} s", started.elapsed().as_secs_f32()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_answer_for_its_own_nonce() {
        let nonce = [9u8; 8];
        let mut reply = nonce.to_vec();
        reply.extend_from_slice(b"udp:/address=203.0.113.7;port=56877\0");
        assert_eq!(answer(&reply, &nonce), Some("203.0.113.7:56877".parse().unwrap()));
        assert_eq!(answer(&reply, &[1u8; 8]), None, "another question's");
        assert_eq!(answer(b"udp:/address=1.2.3.4;port=1", &nonce), None);
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(nonce(), nonce());
    }
}
