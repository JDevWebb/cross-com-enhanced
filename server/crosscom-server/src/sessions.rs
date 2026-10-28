//! Game sessions, kept in memory: who hosts each, its properties (mode, size, privacy...), who's
//! in it, and the station URLs each player registered, which others need to reach them.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Mutex;

use quazal::rmc::types::StationURL;

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
    next_id: u32,
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

    /// Where `pid` can be reached, as it registered.
    pub fn set_urls(&self, pid: u32, urls: Vec<StationURL>) {
        self.lock().urls.insert(pid, urls.into_iter().take(MAX_URLS).collect());
    }

    pub fn urls(&self, pid: u32) -> Vec<StationURL> {
        self.lock().urls.get(&pid).cloned().unwrap_or_default()
    }

    /// A new session hosted by `host`, who is its first participant. A host's oldest sessions
    /// go when they have too many (the game makes a new one each time).
    pub fn create(&self, host: u32, type_id: u32, attributes: &[(u32, u32)]) -> Result<Key, Error> {
        let mut inner = self.lock();
        let hosted: Vec<Key> = inner.sessions.iter().filter(|(_, s)| s.host == host).map(|(k, _)| *k).collect();
        for key in hosted.iter().take((hosted.len() + 1).saturating_sub(MAX_PER_HOST)) {
            inner.sessions.remove(key);
        }
        if inner.sessions.len() >= MAX_SESSIONS {
            return Err(Error::Full);
        }
        inner.next_id = inner.next_id.wrapping_add(1).max(1);
        let key = (type_id, inner.next_id);
        let mut attrs = Vec::new();
        set_attributes(&mut attrs, attributes);
        inner.sessions.insert(
            key,
            Session {
                host,
                attributes: attrs,
                participants: vec![host],
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
        self.add(key, &[pid])
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
        match session.participants.first() {
            None => {
                inner.sessions.remove(&key);
            }
            Some(&next) if pids.contains(&session.host) => session.host = next,
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
    pub fn search(&self, type_id: u32, limit: usize) -> Vec<(Key, Session)> {
        self.lock()
            .sessions
            .iter()
            .filter(|(k, _)| k.0 == type_id)
            .take(limit)
            .map(|(k, s)| (*k, s.clone()))
            .collect()
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

    #[test]
    fn searches_by_type_and_by_player() {
        let sessions = Sessions::default();
        let a = sessions.create(10, 1, &[]).unwrap();
        sessions.create(20, 2, &[]).unwrap();
        assert_eq!(sessions.search(1, 10).iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![a]);
        assert_eq!(sessions.with_participants(1, &[10]).len(), 1);
        assert!(sessions.with_participants(1, &[20]).is_empty());
    }
}
