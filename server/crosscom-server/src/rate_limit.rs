//! Limits failed sign-ins, per address and per account name, so passwords can't be guessed
//! quickly. A success clears the name's count.

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
/// Entries kept before old ones are dropped, so the tables can't grow without bound.
const MAX_ENTRIES: usize = 50_000;

#[derive(Default)]
pub struct FailedSignIns {
    tables: Mutex<Tables>,
}

#[derive(Default)]
struct Tables {
    by_address: HashMap<IpAddr, Count>,
    by_name: HashMap<String, Count>,
}

#[derive(Clone, Copy)]
struct Count {
    since: Instant,
    failures: u32,
}

fn current(count: Option<&Count>, now: Instant) -> u32 {
    count.filter(|c| now.duration_since(c.since) < WINDOW).map_or(0, |c| c.failures)
}

fn bump<K: std::hash::Hash + Eq>(table: &mut HashMap<K, Count>, key: K, now: Instant) {
    if table.len() >= MAX_ENTRIES {
        table.retain(|_, c| now.duration_since(c.since) < WINDOW);
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
        current(tables.by_address.get(&address), now) < MAX_PER_ADDRESS && current(tables.by_name.get(&name.to_lowercase()), now) < MAX_PER_NAME
    }

    pub fn failed(&self, address: IpAddr, name: &str, now: Instant) {
        let mut tables = self.tables.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        bump(&mut tables.by_address, address, now);
        bump(&mut tables.by_name, name.to_lowercase(), now);
    }

    pub fn succeeded(&self, name: &str) {
        let mut tables = self.tables.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        tables.by_name.remove(&name.to_lowercase());
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
        limits.succeeded("KOZAK");
        assert!(limits.allowed(ip(200), "Kozak", now));
    }
}
