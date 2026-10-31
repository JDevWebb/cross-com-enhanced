//! Game sessions, kept in memory: who hosts each, its properties (mode, size, privacy...), who's
//! in it, and the station URLs each player registered, which others need to reach them.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

use quazal::rmc::types::StationURL;
use rand::Rng;

/// Sessions kept at most, and per host. The game makes one as a player opens Multiplayer.
const MAX_SESSIONS: usize = 10_000;
const MAX_PER_HOST: usize = 4;
/// Players in one session, and properties on one.
const MAX_PARTICIPANTS: usize = 64;
const MAX_PROPERTIES: usize = 64;
const MAX_URLS: usize = 8;

/// A session's key: its type and its ID.
pub type Key = (u32, u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub host: u32,
    /// (id, value), in the order first set.
    pub attributes: Vec<(u32, u32)>,
    pub participants: Vec<u32>,
    /// Participants who joined themselves (or host). Only theirs are shown with their addresses:
    /// a host adding a PID mustn't be a way to learn that player's address.
    pub joined: Vec<u32>,
    /// When it was made, in order: IDs are random, so they don't tell.
    created: u64,
}

/// Session properties the game sets: public slots (4 is private, invite-only, slots), and whether
/// the session wasn't made by matchmaking (a party, or a custom match).
pub const PUBLIC_SLOTS: u32 = 3;
pub const NOT_MATCHMADE: u32 = 7;

impl Session {
    pub fn attribute(&self, id: u32) -> Option<u32> {
        self.attributes.iter().find(|(i, _)| *i == id).map(|&(_, v)| v)
    }

    /// Whether a search with `parameters` may offer this session. A quick match searches with
    /// the public slots it needs (property 3) and the match settings it wants (mode, map pack,
    /// size...), which a session must have the same. Parties, custom matches and sessions with
    /// no public slots are never offered: they're joined by invitation.
    pub fn matches(&self, parameters: &[(u32, u32)]) -> bool {
        let public = self.attribute(PUBLIC_SLOTS).unwrap_or(0);
        if self.attribute(NOT_MATCHMADE).unwrap_or(0) != 0 || public == 0 {
            return false;
        }
        let free = public.saturating_sub(u32::try_from(self.participants.len()).unwrap_or(u32::MAX));
        parameters.iter().all(|&(id, value)| match id {
            PUBLIC_SLOTS => free >= value.max(1),
            _ => self.attribute(id) == Some(value),
        })
    }

    pub fn has(&self, pid: u32) -> bool {
        self.participants.contains(&pid)
    }

    pub fn joined(&self, pid: u32) -> bool {
        self.joined.contains(&pid)
    }
}

/// Whether a station URL a player claims for itself may be passed on: an address on a private
/// network (a LAN, or a VPN such as Tailscale's 100.64/10), or the one the server sees it at.
/// Anything else could point other players' games at someone else.
pub fn claimable(url: &StationURL, observed: IpAddr) -> bool {
    match url.address.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b)) || IpAddr::V4(ip) == observed
        }
        Ok(ip) => ip == observed,
        Err(_) => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    NotHost,
    Full,
}

#[derive(Default)]
pub struct Sessions {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    created: u64,
    sessions: BTreeMap<Key, Session>,
    urls: HashMap<u32, Vec<StationURL>>,
}

fn set_attributes(into: &mut Vec<(u32, u32)>, from: &[(u32, u32)]) {
    for &(id, value) in from {
        match into.iter().position(|(i, _)| *i == id) {
            Some(at) => into[at].1 = value,
            None if into.len() < MAX_PROPERTIES => into.push((id, value)),
            None => {}
        }
    }
}

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Where `pid` can be reached: first the address the server sees it at, then those it claims
    /// that may be passed on (see [`claimable`]).
    pub fn set_urls(&self, pid: u32, observed: StationURL, claimed: Vec<StationURL>) {
        let seen: Option<IpAddr> = observed.address.parse().ok();
        let mut urls = vec![observed];
        urls.extend(claimed.into_iter().filter(|u| seen.is_some_and(|ip| claimable(u, ip))).take(MAX_URLS - 1));
        self.lock().urls.insert(pid, urls);
    }

    pub fn urls(&self, pid: u32) -> Vec<StationURL> {
        self.lock().urls.get(&pid).cloned().unwrap_or_default()
    }

    /// A new session hosted by `host`, who is its first participant. A host's oldest sessions
    /// go when they have too many (the game makes a new one each time).
    pub fn create(&self, host: u32, type_id: u32, attributes: &[(u32, u32)]) -> Result<Key, Error> {
        let mut inner = self.lock();
        let mut hosted: Vec<(u64, Key)> = inner.sessions.iter().filter(|(_, s)| s.host == host).map(|(k, s)| (s.created, *k)).collect();
        hosted.sort_unstable();
        let hosted: Vec<Key> = hosted.into_iter().map(|(_, k)| k).collect();
        for key in hosted.iter().take((hosted.len() + 1).saturating_sub(MAX_PER_HOST)) {
            inner.sessions.remove(key);
        }
        if inner.sessions.len() >= MAX_SESSIONS {
            return Err(Error::Full);
        }
        // Random, so one session's ID doesn't lead to the others.
        let key = loop {
            let id = rand::rng().random_range(1..=u32::MAX);
            if !inner.sessions.contains_key(&(type_id, id)) {
                break (type_id, id);
            }
        };
        inner.created += 1;
        let created = inner.created;
        let mut attrs = Vec::new();
        set_attributes(&mut attrs, attributes);
        inner.sessions.insert(
            key,
            Session {
                host,
                attributes: attrs,
                participants: vec![host],
                joined: vec![host],
                created,
            },
        );
        Ok(key)
    }

    pub fn update(&self, pid: u32, key: Key, attributes: &[(u32, u32)]) -> Result<(), Error> {
        let mut inner = self.lock();
        let session = inner.sessions.get_mut(&key).ok_or(Error::NotFound)?;
        if session.host != pid {
            return Err(Error::NotHost);
        }
        set_attributes(&mut session.attributes, attributes);
        Ok(())
    }

    pub fn delete(&self, pid: u32, key: Key) -> Result<(), Error> {
        let mut inner = self.lock();
        let session = inner.sessions.get(&key).ok_or(Error::NotFound)?;
        if session.host != pid {
            return Err(Error::NotHost);
        }
        inner.sessions.remove(&key);
        Ok(())
    }

    pub fn join(&self, pid: u32, key: Key) -> Result<(), Error> {
        self.add(key, &[pid])?;
        let mut inner = self.lock();
        if let Some(session) = inner.sessions.get_mut(&key) {
            if !session.joined.contains(&pid) {
                session.joined.push(pid);
            }
        }
        Ok(())
    }

    pub fn add(&self, key: Key, pids: &[u32]) -> Result<(), Error> {
        let mut inner = self.lock();
        let session = inner.sessions.get_mut(&key).ok_or(Error::NotFound)?;
        for &pid in pids {
            if !session.participants.contains(&pid) {
                if session.participants.len() >= MAX_PARTICIPANTS {
                    return Err(Error::Full);
                }
                session.participants.push(pid);
            }
        }
        Ok(())
    }

    /// Takes players out; the next one hosts if the host left, and the session ends when the
    /// last one leaves.
    pub fn remove(&self, key: Key, pids: &[u32]) -> Result<(), Error> {
        let mut inner = self.lock();
        let session = inner.sessions.get_mut(&key).ok_or(Error::NotFound)?;
        session.participants.retain(|p| !pids.contains(p));
        session.joined.retain(|p| !pids.contains(p));
        match session.participants.first() {
            None => {
                inner.sessions.remove(&key);
            }
            Some(&next) if pids.contains(&session.host) => session.host = session.joined.first().copied().unwrap_or(next),
            Some(_) => {}
        }
        Ok(())
    }

    /// Whether `pid` may change who is in the session: its host, or a player taking themself out.
    pub fn is_host(&self, pid: u32, key: Key) -> bool {
        self.lock().sessions.get(&key).is_some_and(|s| s.host == pid)
    }

    pub fn get(&self, key: Key) -> Option<Session> {
        self.lock().sessions.get(&key).cloned()
    }

    /// Sessions of a type, up to `limit`.
    #[cfg(test)]
    pub fn search(&self, type_id: u32, limit: usize) -> Vec<(Key, Session)> {
        self.lock()
            .sessions
            .iter()
            .filter(|(k, _)| k.0 == type_id)
            .take(limit)
            .map(|(k, s)| (*k, s.clone()))
            .collect()
    }

    /// Sessions of a type a search may offer: matchmade, with public slots, and matching the
    /// search's parameters (see [`Session::matches`]), up to `limit`, fullest first.
    pub fn find(&self, type_id: u32, parameters: &[(u32, u32)], limit: usize) -> Vec<(Key, Session)> {
        let mut found: Vec<(Key, Session)> = self
            .lock()
            .sessions
            .iter()
            .filter(|(k, s)| k.0 == type_id && s.matches(parameters))
            .map(|(k, s)| (*k, s.clone()))
            .collect();
        found.sort_by_key(|(_, s)| (std::cmp::Reverse(s.participants.len()), s.created));
        found.truncate(limit);
        found
    }

    /// Whether `a` and `b` are in a session together.
    pub fn share(&self, a: u32, b: u32) -> bool {
        self.lock().sessions.values().any(|s| s.has(a) && s.has(b))
    }

    /// Whether `pid` hosts a session a search may offer: anyone may be on their way into it.
    pub fn hosts_public(&self, pid: u32) -> bool {
        self.lock().sessions.values().any(|s| s.host == pid && s.matches(&[]))
    }

    /// Sessions of a type with any of `pids` in them.
    pub fn with_participants(&self, type_id: u32, pids: &[u32]) -> Vec<(Key, Session)> {
        self.lock()
            .sessions
            .iter()
            .filter(|(k, s)| k.0 == type_id && s.participants.iter().any(|p| pids.contains(p)))
            .map(|(k, s)| (*k, s.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_from_creation_to_its_end() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 6), (7, 1)]).unwrap();
        assert_eq!(sessions.get(key).unwrap().participants, vec![10]);
        sessions.update(10, key, &[(3, 4), (100, 1)]).unwrap();
        assert_eq!(sessions.get(key).unwrap().attributes, vec![(3, 4), (7, 1), (100, 1)]);
        assert_eq!(sessions.update(11, key, &[(3, 1)]), Err(Error::NotHost));
        sessions.join(11, key).unwrap();
        sessions.remove(key, &[10]).unwrap();
        assert_eq!(sessions.get(key).unwrap().host, 11, "the next player hosts");
        sessions.remove(key, &[11]).unwrap();
        assert!(sessions.get(key).is_none(), "the last one out ends it");
    }

    #[test]
    fn a_host_keeps_only_its_latest_sessions() {
        let sessions = Sessions::default();
        let keys: Vec<_> = (0..MAX_PER_HOST + 2).map(|_| sessions.create(10, 1, &[]).unwrap()).collect();
        assert_eq!(sessions.search(1, 100).len(), MAX_PER_HOST);
        assert!(sessions.get(keys[0]).is_none());
        assert!(sessions.get(*keys.last().unwrap()).is_some());
    }

    fn url(address: &str) -> StationURL {
        format!("prudp:/address={address};port=3074").parse().unwrap()
    }

    #[test]
    fn keeps_the_seen_address_first_and_only_claims_that_can_be_passed_on() {
        let sessions = Sessions::default();
        let claimed = vec![url("192.168.1.20"), url("100.111.224.124"), url("8.8.8.8"), url("203.0.113.7"), url("not-an-ip")];
        sessions.set_urls(10, url("203.0.113.7"), claimed);
        let kept: Vec<String> = sessions.urls(10).iter().map(|u| u.address.clone()).collect();
        assert_eq!(kept, vec!["203.0.113.7", "192.168.1.20", "100.111.224.124", "203.0.113.7"]);
    }

    #[test]
    fn only_players_who_joined_count_as_joined() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[]).unwrap();
        sessions.add(key, &[11]).unwrap();
        sessions.join(12, key).unwrap();
        let session = sessions.get(key).unwrap();
        assert!(session.has(11) && !session.joined(11), "added by the host only");
        assert!(session.joined(12) && session.joined(10));
    }

    #[test]
    fn searches_by_type_and_by_player() {
        let sessions = Sessions::default();
        let a = sessions.create(10, 1, &[]).unwrap();
        sessions.create(20, 2, &[]).unwrap();
        assert_eq!(sessions.search(1, 10).iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![a]);
        assert_eq!(sessions.with_participants(1, &[10]).len(), 1);
        assert!(sessions.with_participants(1, &[20]).is_empty());
    }

    /// What the game sends: a quick match's search, and the sessions it makes.
    const SEARCH: [(u32, u32); 8] = [(3, 1), (100, 1), (101, 2), (116, 2), (105, 16), (103, 0), (115, 0), (114, 12)];
    const QUICK_MATCH: [(u32, u32); 14] = [
        (7, 0),
        (3, 12),
        (4, 0),
        (100, 1),
        (101, 2),
        (116, 2),
        (105, 16),
        (115, 0),
        (103, 0),
        (113, 10),
        (114, 12),
        (112, 3),
        (117, 0),
        (102, 3),
    ];

    #[test]
    fn a_quick_match_search_finds_only_open_public_matches_like_it() {
        let sessions = Sessions::default();
        let quick = sessions.create(10, 1, &QUICK_MATCH).unwrap();
        sessions.create(11, 1, &[(7, 1), (3, 6), (4, 0), (102, 0), (100, 1)]).unwrap(); // a party
        let mut custom = QUICK_MATCH;
        custom[0] = (7, 1);
        custom[1] = (3, 0);
        custom[2] = (4, 12);
        sessions.create(12, 1, &custom).unwrap();
        let mut other_mode = QUICK_MATCH;
        other_mode[4] = (101, 3);
        sessions.create(13, 1, &other_mode).unwrap();
        let found: Vec<Key> = sessions.find(1, &SEARCH, 10).into_iter().map(|(k, _)| k).collect();
        assert_eq!(found, vec![quick]);

        // Full: no public slot left.
        let small = sessions.create(14, 1, &[(7, 0), (3, 2)]).unwrap();
        let offered = |s: &Sessions| s.find(1, &[(3, 1)], 10).into_iter().any(|(k, _)| k == small);
        assert!(offered(&sessions));
        sessions.join(15, small).unwrap();
        assert!(!offered(&sessions));
    }

    #[test]
    fn fuller_matches_come_first() {
        let sessions = Sessions::default();
        let a = sessions.create(10, 1, &QUICK_MATCH).unwrap();
        let b = sessions.create(20, 1, &QUICK_MATCH).unwrap();
        sessions.join(21, b).unwrap();
        let found: Vec<Key> = sessions.find(1, &SEARCH, 10).into_iter().map(|(k, _)| k).collect();
        assert_eq!(found, vec![b, a]);
        assert_eq!(sessions.find(1, &SEARCH, 1).len(), 1);
    }

    #[test]
    fn who_shares_a_session_and_who_hosts_a_public_one() {
        let sessions = Sessions::default();
        let party = sessions.create(10, 1, &[(7, 1), (3, 6)]).unwrap();
        sessions.join(11, party).unwrap();
        assert!(sessions.share(10, 11) && sessions.share(11, 10));
        assert!(!sessions.share(10, 12));
        assert!(!sessions.hosts_public(10), "a party isn't public");
        sessions.create(20, 1, &QUICK_MATCH).unwrap();
        assert!(sessions.hosts_public(20));
        assert!(!sessions.hosts_public(21));
    }
}
