//! Limits failed sign-ins, per address and per account name, so passwords can't be guessed
//! quickly.
//!
//! The name limit stops guessing one account from many addresses, but it mustn't let a stranger
//! lock the owner out: the last few addresses an account signed in from stay allowed while its
//! name is over the limit. A success clears the name's count.
//!
//! A sign-in counts as failed from the moment it's let through until it matches, so many tried at
//! once can't all pass the check while the password hashes are compared.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

/// How long failures count for.
const WINDOW: Duration = Duration::from_secs(15 * 60);
/// Failures allowed in a window from one address, and for one account name.
const MAX_PER_ADDRESS: u32 = 10;
const MAX_PER_NAME: u32 = 10;
/// Entries kept in each table, so memory stays bounded. When a table is full of live counts, the
/// one with the fewest failures goes (the oldest of those), never the ones near their limit: an
/// attacker can't flush their own count by filling the table.
const MAX_ENTRIES: usize = 50_000;
/// Addresses remembered per account.
const KNOWN_ADDRESSES: usize = 4;

#[derive(Default)]
pub struct FailedSignIns {
    tables: Mutex<Tables>,
}

#[derive(Default)]
struct Tables {
    by_address: HashMap<IpAddr, Count>,
    by_name: HashMap<String, Count>,
    /// Where each account last signed in from, newest last.
    known: HashMap<String, Vec<IpAddr>>,
    /// The next count's number.
    next: u64,
}

#[derive(Clone, Copy)]
struct Count {
    since: Instant,
    failures: u32,
    /// Which count this is: each new window (or entry) gets its own.
    number: u64,
}

/// The key an address is counted under. An IPv4 address seen on an IPv6 socket
/// (`::ffff:a.b.c.d`) counts as itself; real IPv6 addresses count by their /64, which one
/// connection usually has all of.
pub(crate) fn address_key(address: IpAddr) -> IpAddr {
    match address.to_canonical() {
        address @ IpAddr::V4(_) => address,
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[4..].fill(0);
            IpAddr::from(segments)
        }
    }
}

fn current(count: Option<&Count>, now: Instant) -> u32 {
    count.filter(|c| now.duration_since(c.since) < WINDOW).map_or(0, |c| c.failures)
}

/// Counts a failure under `key`; the count's number.
fn bump<K: std::hash::Hash + Eq + Clone>(table: &mut HashMap<K, Count>, key: K, now: Instant, next: &mut u64) -> u64 {
    if table.len() >= MAX_ENTRIES && !table.contains_key(&key) {
        table.retain(|_, c| now.duration_since(c.since) < WINDOW);
        if table.len() >= MAX_ENTRIES {
            if let Some(least) = table.iter().min_by_key(|(_, c)| (c.failures, c.since)).map(|(k, _)| k.clone()) {
                table.remove(&least);
            }
        }
    }
    let mut fresh = || {
        *next += 1;
        Count {
            since: now,
            failures: 0,
            number: *next,
        }
    };
    let count = table.entry(key).or_insert_with(&mut fresh);
    if now.duration_since(count.since) >= WINDOW {
        *count = fresh();
    }
    count.failures += 1;
    count.number
}

fn allows(tables: &Tables, address: IpAddr, name: &str, now: Instant) -> bool {
    let known = tables.known.get(name).is_some_and(|a| a.contains(&address.to_canonical()));
    current(tables.by_address.get(&address_key(address)), now) < MAX_PER_ADDRESS && (known || current(tables.by_name.get(name), now) < MAX_PER_NAME)
}

/// A sign-in let through by [`FailedSignIns::try_begin`], counted as failed until it's given to
/// [`FailedSignIns::succeeded`].
#[must_use]
pub struct Attempt {
    address: IpAddr,
    name: String,
    /// The address count it was charged to.
    count: u64,
}

impl FailedSignIns {
    fn tables(&self) -> std::sync::MutexGuard<'_, Tables> {
        self.tables.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts a sign-in from `address` as `name`, if one may be tried now. It counts as failed
    /// until [`Self::succeeded`] says otherwise, decided under one lock: sign-ins checked at once
    /// (the launcher's each have a thread) can't all pass before any of them has failed.
    pub fn try_begin(&self, address: IpAddr, name: &str, now: Instant) -> Option<Attempt> {
        let mut guard = self.tables();
        let tables = &mut *guard;
        let name = name.to_lowercase();
        if !allows(tables, address, &name, now) {
            return None;
        }
        let count = bump(&mut tables.by_address, address_key(address), now, &mut tables.next);
        bump(&mut tables.by_name, name.clone(), now, &mut tables.next);
        Some(Attempt { address, name, count })
    }

    /// Whether a sign-in from `address` as `name` may be tried now.
    #[cfg(test)]
    fn allowed(&self, address: IpAddr, name: &str, now: Instant) -> bool {
        allows(&self.tables(), address, &name.to_lowercase(), now)
    }

    #[cfg(test)]
    fn failed(&self, address: IpAddr, name: &str, now: Instant) {
        let mut guard = self.tables();
        let tables = &mut *guard;
        bump(&mut tables.by_address, address_key(address), now, &mut tables.next);
        bump(&mut tables.by_name, name.to_lowercase(), now, &mut tables.next);
    }

    /// A sign-in begun with [`Self::try_begin`] matched: it doesn't count as failed, and the
    /// name's failures are forgotten. Only the count it was charged to is given it back: if the
    /// window has moved on since (the check is slow), the new window's failures stay.
    pub fn succeeded(&self, attempt: Attempt) {
        self.signed_in(attempt.address, &attempt.name, Some(attempt.count));
    }

    fn signed_in(&self, address: IpAddr, name: &str, charged: Option<u64>) {
        let mut tables = self.tables();
        let name = name.to_lowercase();
        tables.by_name.remove(&name);
        if let Some(count) = tables.by_address.get_mut(&address_key(address)).filter(|c| Some(c.number) == charged) {
            count.failures = count.failures.saturating_sub(1);
        }
        if tables.known.len() >= MAX_ENTRIES && !tables.known.contains_key(&name) {
            // Only real accounts get here, so this is rare; any entry will do.
            if let Some(any) = tables.known.keys().next().cloned() {
                tables.known.remove(&any);
            }
        }
        let address = address.to_canonical();
        let known = tables.known.entry(name).or_default();
        known.retain(|a| *a != address);
        known.push(address);
        if known.len() > KNOWN_ADDRESSES {
            known.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    #[test]
    fn sign_ins_at_once_cant_pass_the_limit_together() {
        let limits = std::sync::Arc::new(FailedSignIns::default());
        let start = Instant::now();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(40));
        let threads: Vec<_> = (0..40)
            .map(|_| {
                let (limits, barrier) = (std::sync::Arc::clone(&limits), std::sync::Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    limits.try_begin(ip(1), "Kozak", start).is_some()
                })
            })
            .collect();
        let begun = threads.into_iter().filter_map(|t| t.join().ok()).filter(|begun| *begun).count();
        assert_eq!(begun, MAX_PER_ADDRESS as usize, "none of them had failed yet, but each counted");
    }

    #[test]
    fn a_sign_in_that_matches_doesnt_count() {
        let limits = FailedSignIns::default();
        let now = Instant::now();
        for _ in 0..MAX_PER_ADDRESS * 2 {
            limits.succeeded(limits.try_begin(ip(1), "Kozak", now).unwrap());
        }
        for _ in 0..MAX_PER_ADDRESS {
            assert!(limits.try_begin(ip(1), "Kozak", now).is_some());
        }
        assert!(limits.try_begin(ip(1), "Kozak", now).is_none());
    }

    #[test]
    fn a_match_after_the_window_moved_on_leaves_the_new_failures() {
        let limits = FailedSignIns::default();
        let start = Instant::now();
        let slow = limits.try_begin(ip(1), "Kozak", start).unwrap();
        // While it's checked, the window ends and a new one fills with failures.
        let later = start + WINDOW;
        for i in 0..MAX_PER_ADDRESS {
            assert!(limits.try_begin(ip(1), &format!("name{i}"), later).is_some());
        }
        limits.succeeded(slow);
        assert!(limits.try_begin(ip(1), "other", later).is_none(), "still over the limit");
    }

    #[test]
    fn stops_an_address_after_too_many_failures_then_forgives() {
        let limits = FailedSignIns::default();
        let start = Instant::now();
        for i in 0..MAX_PER_ADDRESS {
            assert!(limits.allowed(ip(1), &format!("name{i}"), start));
            limits.failed(ip(1), &format!("name{i}"), start);
        }
        assert!(!limits.allowed(ip(1), "someone", start));
        assert!(limits.allowed(ip(2), "someone", start), "other addresses aren't affected");
        assert!(limits.allowed(ip(1), "someone", start + WINDOW));
    }

    #[test]
    fn stops_guessing_one_name_from_many_addresses() {
        let limits = FailedSignIns::default();
        let now = Instant::now();
        for i in 0..MAX_PER_NAME {
            limits.failed(ip(i as u8), "Kozak", now);
        }
        assert!(!limits.allowed(ip(200), "kozak", now), "names ignore case");
        limits.signed_in(ip(201), "KOZAK", None);
        assert!(limits.allowed(ip(200), "Kozak", now));
    }

    #[test]
    fn a_stranger_cant_lock_the_owner_out() {
        let limits = FailedSignIns::default();
        let now = Instant::now();
        let home = ip(10);
        limits.signed_in(home, "Kozak", None);
        for i in 0..MAX_PER_NAME {
            limits.failed(ip(100 + i as u8), "Kozak", now);
        }
        assert!(!limits.allowed(ip(99), "Kozak", now), "new addresses are stopped");
        assert!(limits.allowed(home, "Kozak", now), "the owner's usual address isn't");
    }

    #[test]
    fn tables_stay_bounded_without_flushing_counts_near_their_limit() {
        let mut table = HashMap::new();
        let start = Instant::now();
        // The attacker's own count, nearly at the limit, made first (so the oldest).
        for _ in 0..MAX_PER_NAME - 1 {
            bump(&mut table, usize::MAX, start, &mut 0);
        }
        // Then the table is flooded with single failures.
        for i in 0..MAX_ENTRIES + 10 {
            bump(&mut table, i, start + Duration::from_millis(i as u64), &mut 0);
        }
        assert_eq!(table.len(), MAX_ENTRIES);
        assert_eq!(table[&usize::MAX].failures, MAX_PER_NAME - 1, "the near-limit count survives");
        assert!(!table.contains_key(&0), "single failures went, oldest first");
    }

    #[test]
    fn ipv4_on_an_ipv6_socket_counts_as_itself() {
        let limits = FailedSignIns::default();
        let now = Instant::now();
        for i in 0..MAX_PER_ADDRESS {
            limits.failed("::ffff:192.0.2.1".parse().unwrap(), &format!("name{i}"), now);
        }
        assert!(!limits.allowed("192.0.2.1".parse().unwrap(), "other", now), "the same address either way");
        assert!(limits.allowed("::ffff:192.0.2.2".parse().unwrap(), "other", now), "other IPv4 players aren't affected");
        limits.signed_in("::ffff:192.0.2.9".parse().unwrap(), "Kozak", None);
        assert_eq!(address_key("::ffff:192.0.2.9".parse().unwrap()), "192.0.2.9".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn ipv6_counts_by_the_64() {
        let limits = FailedSignIns::default();
        let now = Instant::now();
        for i in 0..MAX_PER_ADDRESS {
            let address: IpAddr = format!("2001:db8:1:2::{:x}", i + 1).parse().unwrap();
            limits.failed(address, &format!("name{i}"), now);
        }
        assert!(!limits.allowed("2001:db8:1:2:ffff::1".parse().unwrap(), "other", now));
        assert!(limits.allowed("2001:db8:1:3::1".parse().unwrap(), "other", now));
    }
}
