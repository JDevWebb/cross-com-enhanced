//! Who's online: the players signed in to the secure server, for the launcher's count
//! (`GET /api/info`), and where each has played from lately, for the logs (`logs.rs`).
//!
//! The secure server keeps its connections on its own thread; it calls in here as a player signs
//! in and as their last connection closes or times out (a game that stops without a goodbye
//! counts for up to a minute more). A player is one pid however many connections their game
//! holds. The game's shared telemetry account isn't a player and isn't counted.

use std::collections::HashMap;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use crate::rate_limit::address_key;
use crate::ticket::TRACKING_PID;

/// How long a sign-in counts as recent.
pub const RECENT: Duration = Duration::from_secs(24 * 60 * 60);
/// Addresses remembered per player, the newest.
const ADDRESSES_PER_PLAYER: usize = 4;
/// Players remembered; when full, the one whose last sign-in is oldest goes.
const MAX_PLAYERS: usize = 50_000;

#[derive(Default)]
pub struct Online {
    pids: Mutex<HashSet<u32>>,
    /// Where each player signed in from lately (an address as `address_key` counts it), and when;
    /// newest last.
    recent: Mutex<HashMap<u32, Vec<(IpAddr, Instant)>>>,
}

impl Online {
    pub fn signed_in(&self, pid: u32, address: IpAddr) {
        self.signed_in_at(pid, address, Instant::now());
    }

    fn signed_in_at(&self, pid: u32, address: IpAddr, now: Instant) {
        if pid == TRACKING_PID {
            return;
        }
        self.pids().insert(pid);
        let mut recent = self.recent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if recent.len() >= MAX_PLAYERS && !recent.contains_key(&pid) {
            recent.retain(|_, seen| seen.last().is_some_and(|(_, at)| now.duration_since(*at) < RECENT));
            if recent.len() >= MAX_PLAYERS {
                if let Some(oldest) = recent.iter().min_by_key(|(_, seen)| seen.last().map(|(_, at)| *at)).map(|(pid, _)| *pid) {
                    recent.remove(&oldest);
                }
            }
        }
        let key = address_key(address);
        let seen = recent.entry(pid).or_default();
        seen.retain(|(a, at)| *a != key && now.duration_since(*at) < RECENT);
        seen.push((key, now));
        if seen.len() > ADDRESSES_PER_PLAYER {
            seen.remove(0);
        }
    }

    /// Whether the player signed in to the game from `address` (IPv4 exactly, IPv6 by its /64)
    /// in the last 24 hours.
    pub fn played_from(&self, pid: u32, address: IpAddr) -> bool {
        self.played_from_at(pid, address, Instant::now())
    }

    fn played_from_at(&self, pid: u32, address: IpAddr, now: Instant) -> bool {
        if pid == TRACKING_PID {
            return false;
        }
        let key = address_key(address);
        let recent = self.recent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        recent
            .get(&pid)
            .is_some_and(|seen| seen.iter().any(|(a, at)| *a == key && now.duration_since(*at) < RECENT))
    }

    /// The player's last connection is gone.
    pub fn gone(&self, pid: u32) {
        self.pids().remove(&pid);
    }

    pub fn count(&self) -> usize {
        self.pids().len()
    }

    fn pids(&self) -> std::sync::MutexGuard<'_, HashSet<u32>> {
        self.pids.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records a sign-in `ago` before now (tests elsewhere).
    #[cfg(test)]
    pub fn signed_in_ago(&self, pid: u32, address: IpAddr, ago: Duration) {
        let now = Instant::now();
        self.signed_in_at(pid, address, now.checked_sub(ago).unwrap_or(now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn counts_players_once_and_not_the_telemetry_account() {
        let online = Online::default();
        online.signed_in(0x10_0000, ip("192.0.2.1"));
        online.signed_in(0x10_0000, ip("192.0.2.1"));
        online.signed_in(0x10_0001, ip("192.0.2.2"));
        online.signed_in(TRACKING_PID, ip("192.0.2.3"));
        assert_eq!(online.count(), 2);
        online.gone(0x10_0000);
        online.gone(0x10_0000);
        assert_eq!(online.count(), 1);
    }

    #[test]
    fn remembers_where_players_played_from_for_a_day() {
        let online = Online::default();
        let start = Instant::now();
        online.signed_in_at(0x10_0000, ip("192.0.2.1"), start);
        assert!(online.played_from_at(0x10_0000, ip("192.0.2.1"), start + Duration::from_secs(3600)));
        assert!(online.played_from_at(0x10_0000, ip("::ffff:192.0.2.1"), start), "IPv4 either way");
        assert!(!online.played_from_at(0x10_0000, ip("192.0.2.2"), start), "another address");
        assert!(!online.played_from_at(0x10_0001, ip("192.0.2.1"), start), "another player");
        assert!(!online.played_from_at(0x10_0000, ip("192.0.2.1"), start + RECENT), "a day later");
        online.signed_in_at(TRACKING_PID, ip("192.0.2.9"), start);
        assert!(!online.played_from_at(TRACKING_PID, ip("192.0.2.9"), start), "never the telemetry account");

        online.signed_in_at(0x10_0002, ip("2001:db8:1:2::5"), start);
        assert!(online.played_from_at(0x10_0002, ip("2001:db8:1:2:ffff::1"), start), "the same /64");
        assert!(!online.played_from_at(0x10_0002, ip("2001:db8:1:3::5"), start));
    }

    #[test]
    fn keeps_a_few_addresses_per_player() {
        let online = Online::default();
        let now = Instant::now();
        for i in 1..=ADDRESSES_PER_PLAYER + 1 {
            online.signed_in_at(0x10_0000, ip(&format!("192.0.2.{i}")), now);
        }
        assert!(!online.played_from_at(0x10_0000, ip("192.0.2.1"), now), "the oldest went");
        assert!(online.played_from_at(0x10_0000, ip("192.0.2.2"), now));
    }
}
