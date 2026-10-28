//! Limits failed sign-ins, per address and per account name, so passwords can't be guessed
//! quickly.
//!
//! The name limit stops guessing one account from many addresses, but it mustn't let a stranger
//! lock the owner out: the last few addresses an account signed in from stay allowed while its
//! name is over the limit. A success clears the name's count.

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
/// Entries kept in each table; past it the oldest is dropped, so memory stays bounded.
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
}

#[derive(Clone, Copy)]
struct Count {
    since: Instant,
    failures: u32,
}

fn current(count: Option<&Count>, now: Instant) -> u32 {
    count.filter(|c| now.duration_since(c.since) < WINDOW).map_or(0, |c| c.failures)
}

fn bump<K: std::hash::Hash + Eq + Clone>(table: &mut HashMap<K, Count>, key: K, now: Instant) {
    if table.len() >= MAX_ENTRIES && !table.contains_key(&key) {
        table.retain(|_, c| now.duration_since(c.since) < WINDOW);
        if table.len() >= MAX_ENTRIES {
            if let Some(oldest) = table.iter().min_by_key(|(_, c)| c.since).map(|(k, _)| k.clone()) {
                table.remove(&oldest);
            }
        }
    }
    let count = table.entry(key).or_insert(Count { since: now, failures: 0 });
    if now.duration_since(count.since) >= WINDOW {
        *count = Count { since: now, failures: 0 };
    }
    count.failures += 1;
}

impl FailedSignIns {
    /// Whether a sign-in from `address` as `name` may be tried now.
    pub fn allowed(&self, address: IpAddr, name: &str, now: Instant) -> bool {
        let tables = self.tables.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let name = name.to_lowercase();
        let known = tables.known.get(&name).is_some_and(|a| a.contains(&address));
        current(tables.by_address.get(&address), now) < MAX_PER_ADDRESS && (known || current(tables.by_name.get(&name), now) < MAX_PER_NAME)
    }

    pub fn failed(&self, address: IpAddr, name: &str, now: Instant) {
        let mut tables = self.tables.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        bump(&mut tables.by_address, address, now);
        bump(&mut tables.by_name, name.to_lowercase(), now);
    }

    pub fn succeeded(&self, address: IpAddr, name: &str) {
        let mut tables = self.tables.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let name = name.to_lowercase();
        tables.by_name.remove(&name);
        if tables.known.len() >= MAX_ENTRIES && !tables.known.contains_key(&name) {
            // Only real accounts get here, so this is rare; any entry will do.
            if let Some(any) = tables.known.keys().next().cloned() {
                tables.known.remove(&any);
            }
        }
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
        limits.succeeded(ip(201), "KOZAK");
        assert!(limits.allowed(ip(200), "Kozak", now));
    }

    #[test]
    fn a_stranger_cant_lock_the_owner_out() {
        let limits = FailedSignIns::default();
        let now = Instant::now();
        let home = ip(10);
        limits.succeeded(home, "Kozak");
        for i in 0..MAX_PER_NAME {
            limits.failed(ip(100 + i as u8), "Kozak", now);
        }
        assert!(!limits.allowed(ip(99), "Kozak", now), "new addresses are stopped");
        assert!(limits.allowed(home, "Kozak", now), "the owner's usual address isn't");
    }

    #[test]
    fn tables_stay_bounded() {
        let mut table = HashMap::new();
        let start = Instant::now();
        for i in 0..MAX_ENTRIES + 10 {
            bump(&mut table, i, start + Duration::from_millis(i as u64));
        }
        assert_eq!(table.len(), MAX_ENTRIES);
        assert!(!table.contains_key(&0), "the oldest went first");
    }
}
