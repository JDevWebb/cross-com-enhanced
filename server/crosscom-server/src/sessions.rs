//! Game sessions, kept in memory: who hosts each, its properties (mode, size, privacy...), who's
//! in it, and the station URLs each player registered, which others need to reach them.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddrV4;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use quazal::rmc::types::StationURL;
use rand::Rng;

/// Sessions kept at most, and per host. The game makes one as a player opens Multiplayer. When
/// there are as many as can be kept, a new one takes the place of the oldest with nobody but its
/// host in it (or else the oldest): a few thousand accounts making sessions mustn't stop everyone
/// else making theirs.
const MAX_SESSIONS: usize = 10_000;
const MAX_PER_HOST: usize = 4;
/// Players in one session, and properties on one.
const MAX_PARTICIPANTS: usize = 64;
const MAX_PROPERTIES: usize = 64;
const MAX_URLS: usize = 8;

/// How long a player's report of its router (`/api/forward`) holds: the client renews it every
/// 10 minutes while the game runs.
const REACHABILITY_HOLDS: Duration = Duration::from_secs(35 * 60);

/// How long a player refused hosting a session waits before it may ask again.
const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// A session's key: its type and its ID.
pub type Key = (u32, u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub host: u32,
    /// (id, value), in the order first set.
    pub attributes: Vec<(u32, u32)>,
    pub participants: Vec<u32>,
    /// Participants in private (invite-only) slots; the rest are in public ones.
    pub private: Vec<u32>,
    /// Players a search offered this session to (the most recent [`MAX_OFFERED`]). Being offered
    /// gives nothing by itself: a search offers several sessions.
    offered: Vec<u32>,
    /// Players who then chose this session, by asking to reach its host (the game's next step
    /// after a search). They may reach its members, and the host adding one is that player
    /// joining.
    wanting: Vec<u32>,
    /// Players a member invited (friends only, see `game_session`), with who invited them and
    /// when, until they answer, are added or the invitation is old: they may reach the
    /// session's members (not the other way), and nothing more until they accept.
    invited: Vec<Invited>,
    /// Invited players who accepted: the host adding one of them is them joining.
    accepted: Vec<u32>,
    /// Participants who joined themselves (or host). Only theirs are shown with their addresses:
    /// a host adding a PID mustn't be a way to learn that player's address.
    pub joined: Vec<u32>,
    /// Players refused hosting it (see [`Sessions::take_over`]), and when.
    refused: Vec<(u32, std::time::Instant)>,
    /// Players its host took out (the most recent [`MAX_OFFERED`]): they may not join it again
    /// on their own, only if the host invites or adds them again.
    removed: Vec<u32>,
    /// When it was made, in order: IDs are random, so they don't tell.
    created: u64,
}

/// Session properties the game sets: public slots (4 is private, invite-only, slots), and whether
/// the session wasn't made by matchmaking (a party, or a custom match).
pub const PUBLIC_SLOTS: u32 = 3;
pub const NOT_MATCHMADE: u32 = 7;
/// Session properties the service keeps: how many public and private slots are taken. A game
/// joining a session reads both (with 3 and 4), and crashed on a search result without them.
/// One invitation into a session, as the session keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Invited {
    pid: u32,
    sender: u32,
    at: std::time::Instant,
}

/// As long as an invitation lasts (see `invitations`).
const INVITED_FOR: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How many players a session remembers having been offered to.
const MAX_OFFERED: usize = 64;
pub const PUBLIC_TAKEN: u32 = 5;
pub const PRIVATE_TAKEN: u32 = 6;
/// The DLC packs a Quick Match uses, as the game sets it from the packs it has (bit 0 Arctic
/// Strike, bit 1 Khyber Strike; Raven Strike has no adversarial content), and searches with its
/// own. A search may be offered a lobby whose packs it has: not only one with the same packs, or
/// players with different packs would never meet.
pub const PACKS: u32 = 115;
/// A Quick Match's size, as the game made it (12, or 8 in the smaller size), and as a search asks
/// for it. A host may make its lobbies smaller (the overlay's most players): such a lobby is offered
/// to searches for its mode's size too.
pub const SIZE: u32 = 114;

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
        let taken = self.participants.iter().filter(|p| !self.private.contains(p)).count();
        let free = public.saturating_sub(u32::try_from(taken).unwrap_or(u32::MAX));
        // Its size as the game made it: a squad member's lobby also has invite-only slots, which
        // its host and squadmates take, so free public slots alone could take it past its size.
        let full = self
            .attribute(SIZE)
            .is_some_and(|size| self.participants.len() >= usize::try_from(size).unwrap_or(usize::MAX));
        if full {
            return false;
        }
        parameters.iter().all(|&(id, value)| match id {
            PUBLIC_SLOTS => free >= value.max(1),
            PACKS => self.attribute(id).is_some_and(|lobby| lobby & !value == 0),
            SIZE => self.attribute(id).is_some_and(|lobby| lobby <= value),
            _ => self.attribute(id) == Some(value),
        })
    }

    pub fn has(&self, pid: u32) -> bool {
        self.participants.contains(&pid)
    }

    /// The properties as the game is given them: the host's, with the taken slots (5 and 6) as
    /// they are now.
    pub fn properties(&self) -> Vec<(u32, u32)> {
        let private = self.participants.iter().filter(|p| self.private.contains(p)).count();
        let public = self.participants.len() - private;
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let mut properties: Vec<(u32, u32)> = self.attributes.iter().copied().filter(|(id, _)| *id != PUBLIC_TAKEN && *id != PRIVATE_TAKEN).collect();
        properties.push((PUBLIC_TAKEN, count(public)));
        properties.push((PRIVATE_TAKEN, count(private)));
        properties
    }

    pub fn joined(&self, pid: u32) -> bool {
        self.joined.contains(&pid)
    }

    /// Whether the session has a public slot free (whatever else it is: a custom match or a party
    /// may have public slots too).
    fn public_slot_free(&self) -> bool {
        let public = self.attribute(PUBLIC_SLOTS).unwrap_or(0);
        let taken = self.participants.iter().filter(|p| !self.private.contains(p)).count();
        u32::try_from(taken).is_ok_and(|taken| taken < public)
    }

    /// Whether `pid` may join the session itself (`GameSession.JoinSession`): not if its host took
    /// it out; otherwise if a search offered it the session (or it chose it), it was invited, the
    /// host already added it, or the session has a public slot free. Anyone who knew a session's
    /// key used to be able to join it, and then see its members' addresses.
    fn may_join(&self, pid: u32) -> bool {
        if self.removed.contains(&pid) {
            return false;
        }
        self.participants.contains(&pid)
            || self.offered.contains(&pid)
            || self.wanting.contains(&pid)
            || self.accepted.contains(&pid)
            || self.invited.iter().any(|i| i.pid == pid && i.at.elapsed() < INVITED_FOR)
            || self.public_slot_free()
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
    /// Not a session the player may join (see [`Session::may_join`]).
    NotAllowed,
}

/// What a player's client said about its router (`/api/forward`, see `reach`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reachability {
    /// The public port its router forwards to the game's peer-to-peer socket, and how it was
    /// asked (`UPnP`, `PCP`, `NAT-PMP`).
    pub forward: Option<(u16, String)>,
    /// How its router maps ports: `consistent`, `symmetric` or `unknown`.
    pub nat: String,
    /// The address the report came from: a forward holds only there.
    pub seen: IpAddr,
    /// The game's socket on the player's network, as the client found it: the address of the
    /// adapter that reaches the server, which the game may not have announced (a VPN's or a
    /// virtual machine's adapter in its place).
    pub local: SocketAddrV4,
    pub at: Instant,
}

#[derive(Default)]
pub struct Sessions {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    created: u64,
    sessions: BTreeMap<Key, Session>,
    /// Per player: where the server sees it, and the URLs it registered that may be passed on.
    urls: HashMap<u32, (StationURL, Vec<StationURL>)>,
    /// Per player: its client's last report of its router.
    reachability: HashMap<u32, Reachability>,
}

/// The session to end to make room for a new one: the oldest with nobody but its host in it, or
/// else the oldest.
fn oldest_to_end(sessions: &BTreeMap<Key, Session>) -> Option<Key> {
    sessions.iter().min_by_key(|(_, s)| (s.participants.len() > 1, s.created)).map(|(k, _)| *k)
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

    /// Where `pid` can be reached: the address the server sees it at, and those it registered that
    /// may be passed on (see [`claimable`]).
    pub fn set_urls(&self, pid: u32, observed: StationURL, claimed: Vec<StationURL>) {
        let seen: Option<IpAddr> = observed.address.parse().ok();
        let first = claimed.first().cloned();
        let mut kept: Vec<StationURL> = claimed.into_iter().filter(|u| seen.is_some_and(|ip| claimable(u, ip))).take(MAX_URLS).collect();
        // None that may be passed on (a player behind a NAT whose public address the game
        // didn't register): its first URL at the address the server sees, so it keeps the
        // game's scheme, peer-to-peer port and parameters.
        if kept.is_empty() {
            if let Some(mut url) = first {
                url.address.clone_from(&observed.address);
                kept.push(url);
            }
        }
        self.lock().urls.insert(pid, (observed, kept));
    }

    /// The URLs other players are given for `pid`: those it registered, as the game wrote them.
    /// The game reads each one's parameters (`PID`, `RVCID`): a bare address there (the server's
    /// view of its connection, which used to come first) crashed a joining game. With nothing
    /// registered, the address the server sees.
    pub fn urls(&self, pid: u32) -> Vec<StationURL> {
        match self.lock().urls.get(&pid) {
            Some((_, kept)) if !kept.is_empty() => kept.clone(),
            Some((observed, _)) => vec![observed.clone()],
            None => Vec::new(),
        }
    }

    /// The URLs `pid` is given to a player the server sees at `viewer`: as [`Self::urls`], with
    /// its private addresses first when the two are behind one public address (players at one
    /// house), so their games reach each other across their network rather than through their
    /// router's public address, which many routers don't loop back inside.
    ///
    /// Elsewhere, when its router forwards a port to its game (see [`Self::forward_of`]) other
    /// than the one its public URL names, that URL at the forwarded port comes first: it reaches
    /// the game whatever the viewer's router does.
    pub fn urls_seen_from(&self, pid: u32, viewer: IpAddr) -> Vec<StationURL> {
        let mut urls = self.urls(pid);
        let seen = self.lock().urls.get(&pid).and_then(|(observed, _)| observed.address.parse::<IpAddr>().ok());
        if seen == Some(viewer) {
            let private = |u: &StationURL| u.address.parse::<IpAddr>().is_ok_and(|ip| ip != viewer && claimable(u, ip));
            urls.sort_by_key(|u| !private(u));
            // The address on its network the client found, when the game announced another
            // adapter's: first, as the one housemates reach.
            if let Some(local) = self.unannounced_local(pid, seen) {
                if let Some(mut url) = urls.first().cloned() {
                    url.address = local.ip().to_string();
                    url.port = local.port();
                    urls.insert(0, url);
                    urls.truncate(MAX_URLS);
                }
            }
        } else if let (Some(seen), Some(port)) = (seen, self.forward_of(pid)) {
            let address = seen.to_string();
            let public = urls.iter().find(|u| u.address == address).or(urls.first()).cloned();
            if let Some(mut forwarded) = public.filter(|u| u.port != port) {
                forwarded.address = address;
                forwarded.port = port;
                urls.insert(0, forwarded);
                urls.truncate(MAX_URLS);
            }
        }
        urls
    }

    /// The player seen at `seen` whose game registered `local`, its peer-to-peer socket on its
    /// own network, and the public port of that socket if it registered that too.
    pub fn peer_at(&self, seen: IpAddr, local: SocketAddrV4) -> Option<(u32, Option<u16>)> {
        let inner = self.lock();
        let seen_text = seen.to_string();
        let (local_ip, local_port) = (local.ip().to_string(), local.port());
        inner.urls.iter().find_map(|(pid, (observed, kept))| {
            if observed.address != seen_text || !kept.iter().any(|u| u.address == local_ip && u.port == local_port) {
                return None;
            }
            let public = kept.iter().find(|u| u.address == seen_text).map(|u| u.port);
            Some((*pid, public))
        })
    }

    /// The address the server sees `pid`'s game at, once it registered its URLs.
    pub fn seen(&self, pid: u32) -> Option<IpAddr> {
        self.lock().urls.get(&pid)?.0.address.parse().ok()
    }

    /// The game's socket on `pid`'s network as its client reported it, when the game didn't
    /// announce that address (and its report holds there).
    fn unannounced_local(&self, pid: u32, seen: Option<IpAddr>) -> Option<SocketAddrV4> {
        let reachability = self.reachability(pid).filter(|r| Some(r.seen) == seen)?;
        let local = reachability.local;
        let inner = self.lock();
        let (_, kept) = inner.urls.get(&pid)?;
        let peer: Vec<&StationURL> = kept.iter().filter(|u| u.scheme == "udp").collect();
        let announced = peer.is_empty() || peer.iter().any(|u| u.address == local.ip().to_string() && u.port == local.port());
        (!announced).then_some(local)
    }

    /// For `pid`'s game, seen at `seen`: the public port it registered, and whether it announced
    /// `local` as its socket on its network. None when it's seen elsewhere (or not at all).
    pub fn socket_of(&self, pid: u32, seen: IpAddr, local: SocketAddrV4) -> Option<(Option<u16>, bool)> {
        let inner = self.lock();
        let (observed, kept) = inner.urls.get(&pid)?;
        let seen_text = seen.to_string();
        if observed.address != seen_text {
            return None;
        }
        // Its peer-to-peer station URLs only: at sign-in it registers its connection to the
        // server (prudp, another socket), not those, until it's in Multiplayer.
        let peer: Vec<&StationURL> = kept.iter().filter(|u| u.scheme == "udp").collect();
        let public = peer.iter().find(|u| u.address == seen_text).map(|u| u.port);
        let announced = peer.is_empty() || peer.iter().any(|u| u.address == local.ip().to_string() && u.port == local.port());
        Some((public, announced))
    }

    /// Whether `pid`'s game mostly can't punch through to a host: its router gives a port per
    /// destination (a symmetric NAT, as its client reports) and doesn't forward the game's port.
    /// It can join only a host whose announced address takes packets from anyone (`announce`).
    pub fn punches_poorly(&self, pid: u32) -> bool {
        self.reachability(pid).is_some_and(|r| r.nat == "symmetric") && self.forward_of(pid).is_none()
    }

    /// Notes what `pid`'s client said about its router.
    pub fn set_reachability(&self, pid: u32, reachability: Reachability) {
        self.lock().reachability.insert(pid, reachability);
    }

    /// What `pid`'s client last said about its router, while it holds.
    pub fn reachability(&self, pid: u32) -> Option<Reachability> {
        self.lock().reachability.get(&pid).filter(|r| r.at.elapsed() < REACHABILITY_HOLDS).cloned()
    }

    /// The public port `pid`'s router forwards to its game, while its report holds and it's
    /// still seen where it made it.
    pub fn forward_of(&self, pid: u32) -> Option<u16> {
        let reachability = self.reachability(pid)?;
        let seen: IpAddr = self.lock().urls.get(&pid)?.0.address.parse().ok()?;
        (reachability.seen == seen).then_some(reachability.forward?.0)
    }

    /// The URLs `pid` registered that may be passed on, without the server's view.
    pub fn registered(&self, pid: u32) -> Vec<StationURL> {
        self.lock().urls.get(&pid).map(|(_, kept)| kept.clone()).unwrap_or_default()
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
            if let Some(oldest) = oldest_to_end(&inner.sessions) {
                inner.sessions.remove(&oldest);
            }
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
                private: Vec::new(),
                offered: Vec::new(),
                wanting: Vec::new(),
                invited: Vec::new(),
                accepted: Vec::new(),
                joined: vec![host],
                refused: Vec::new(),
                removed: Vec::new(),
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
        // No longer one a search may offer (made invite-only, or full of private slots): the
        // players it was offered to, or who chose it, lose that.
        if !session.matches(&[]) {
            session.offered.clear();
            session.wanting.clear();
        }
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

    /// `pid` joins the session itself, if it may (see [`Session::may_join`]).
    pub fn join(&self, pid: u32, key: Key) -> Result<(), Error> {
        if !self.lock().sessions.get(&key).ok_or(Error::NotFound)?.may_join(pid) {
            return Err(Error::NotAllowed);
        }
        self.add(key, &[pid], &[])?;
        let mut inner = self.lock();
        if let Some(session) = inner.sessions.get_mut(&key) {
            if !session.joined.contains(&pid) {
                session.joined.push(pid);
            }
        }
        Ok(())
    }

    /// Notes that `sender`, a member, invited `pid` into the session.
    pub fn invite(&self, key: Key, sender: u32, pid: u32) {
        if let Some(session) = self.lock().sessions.get_mut(&key) {
            // The host asking a player it took out back in.
            if sender == session.host {
                session.removed.retain(|&p| p != pid);
            }
            session.invited.retain(|i| i.pid != pid && i.at.elapsed() < INVITED_FOR);
            if session.invited.len() < MAX_OFFERED {
                session.invited.push(Invited {
                    pid,
                    sender,
                    at: std::time::Instant::now(),
                });
            }
        }
    }

    /// `pid` answered its invitation into the session: accepted (the host may now add it in) or
    /// not. Accepting counts only with an invitation still open.
    pub fn answer(&self, key: Key, pid: u32, accepted: bool) {
        if let Some(session) = self.lock().sessions.get_mut(&key) {
            let open = session.invited.iter().any(|i| i.pid == pid && i.at.elapsed() < INVITED_FOR);
            session.invited.retain(|i| i.pid != pid);
            session.accepted.retain(|&p| p != pid);
            if accepted && open && session.accepted.len() < MAX_OFFERED {
                session.accepted.push(pid);
            }
        }
    }

    /// `sender` took back its invitation of `pid`.
    pub fn uninvite(&self, key: Key, sender: u32, pid: u32) {
        if let Some(session) = self.lock().sessions.get_mut(&key) {
            session.invited.retain(|i| !(i.pid == pid && i.sender == sender));
        }
    }

    /// Notes that a search offered the session to `pid`.
    pub fn offer(&self, key: Key, pid: u32) {
        if let Some(session) = self.lock().sessions.get_mut(&key) {
            session.offered.retain(|&p| p != pid);
            if session.offered.len() >= MAX_OFFERED {
                session.offered.remove(0);
            }
            session.offered.push(pid);
        }
    }

    /// `pid` asks to reach `host`: if a search offered `pid` a session `host` hosts, `pid` has
    /// chosen that one. Its other offers go, so one player wants one session at a time. The
    /// session chosen, if any.
    pub fn choose(&self, pid: u32, host: u32) -> Option<Key> {
        let mut inner = self.lock();
        let chosen = inner
            .sessions
            .iter()
            .find(|(_, s)| s.host == host && s.offered.contains(&pid) && s.matches(&[]))
            .map(|(k, _)| *k)?;
        for (k, session) in &mut inner.sessions {
            session.offered.retain(|&p| p != pid);
            session.wanting.retain(|&p| p != pid);
            if *k == chosen {
                session.wanting.push(pid);
            }
        }
        Some(chosen)
    }

    /// Adds players to public slots (`public`) or private ones (`private`). A player already in
    /// takes the kind of slot named now.
    pub fn add(&self, key: Key, public: &[u32], private: &[u32]) -> Result<(), Error> {
        let mut inner = self.lock();
        let session = inner.sessions.get_mut(&key).ok_or(Error::NotFound)?;
        for (&pid, is_private) in public.iter().map(|p| (p, false)).chain(private.iter().map(|p| (p, true))) {
            if !session.participants.contains(&pid) {
                if session.participants.len() >= MAX_PARTICIPANTS {
                    return Err(Error::Full);
                }
                session.participants.push(pid);
            }
            session.removed.retain(|&p| p != pid);
            session.private.retain(|&p| p != pid);
            if is_private {
                session.private.push(pid);
            }
            if session.wanting.contains(&pid) || session.accepted.contains(&pid) {
                session.wanting.retain(|&p| p != pid);
                session.invited.retain(|i| i.pid != pid);
                session.accepted.retain(|&p| p != pid);
                if !session.joined.contains(&pid) {
                    session.joined.push(pid);
                }
            }
        }
        Ok(())
    }

    /// `pid` signed in again: its game started afresh, so it's in none of the sessions its last
    /// game was in. It's taken out of each (as if it had left: the next player hosts, and a
    /// session ends with its last player), so a search never offers a lobby whose players have
    /// all gone, nor sends anyone to a host that isn't there. How many sessions it was in.
    pub fn signed_in_afresh(&self, pid: u32) -> usize {
        let keys: Vec<Key> = self.lock().sessions.iter().filter(|(_, s)| s.has(pid)).map(|(k, _)| *k).collect();
        for key in &keys {
            let _ = self.remove(*key, &[pid]);
        }
        keys.len()
    }

    /// The host took `pids` out: as [`Self::remove`], and they may not join again on their own.
    pub fn remove_by_host(&self, key: Key, pids: &[u32]) -> Result<(), Error> {
        if let Some(session) = self.lock().sessions.get_mut(&key) {
            for &pid in pids.iter().filter(|&&p| p != session.host) {
                session.removed.retain(|&p| p != pid);
                if session.removed.len() >= MAX_OFFERED {
                    session.removed.remove(0);
                }
                session.removed.push(pid);
            }
        }
        self.remove(key, pids)
    }

    /// Takes players out; the next one hosts if the host left, and the session ends when the
    /// last one leaves.
    pub fn remove(&self, key: Key, pids: &[u32]) -> Result<(), Error> {
        let mut inner = self.lock();
        let session = inner.sessions.get_mut(&key).ok_or(Error::NotFound)?;
        session.participants.retain(|p| !pids.contains(p));
        session.private.retain(|p| !pids.contains(p));
        session.offered.retain(|p| !pids.contains(p));
        session.wanting.retain(|p| !pids.contains(p));
        session.invited.retain(|i| !pids.contains(&i.pid) && !pids.contains(&i.sender));
        session.accepted.retain(|p| !pids.contains(p));
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

    /// `pid` takes over hosting `key` (the game's host migration): a player who joined it, once
    /// `gone(host)` says its host has gone. The game's players ask about 10 s after losing their
    /// host, so `gone` must say so sooner than that while a live host is never silent that long
    /// (see `game_session`). A player refused may not ask again for [`RETRY_AFTER`], so it
    /// can't keep asking until a live host happens to have been quiet long enough.
    /// True if `pid` hosts it now (or did).
    pub fn take_over(&self, key: Key, pid: u32, gone: impl Fn(u32) -> bool) -> bool {
        let mut inner = self.lock();
        let Some(session) = inner.sessions.get_mut(&key) else {
            return false;
        };
        if session.host == pid {
            return true;
        }
        session.refused.retain(|(_, at)| at.elapsed() < RETRY_AFTER);
        if !session.joined(pid) || session.refused.iter().any(|(p, _)| *p == pid) {
            return false;
        }
        if session.refused.len() >= MAX_PARTICIPANTS {
            return false;
        }
        if !gone(session.host) {
            session.refused.push((pid, std::time::Instant::now()));
            return false;
        }
        session.host = pid;
        true
    }

    /// Whether `pid` joined the session (or hosts it).
    pub fn is_member(&self, pid: u32, key: Key) -> bool {
        self.lock().sessions.get(&key).is_some_and(|s| s.joined(pid))
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
    #[cfg(test)]
    pub fn find(&self, type_id: u32, parameters: &[(u32, u32)], limit: usize) -> Vec<(Key, Session)> {
        self.find_where(type_id, parameters, limit, |_| true)
    }

    /// As [`Sessions::find`], of the sessions `keep` takes (before the limit, so sessions it
    /// leaves out don't take others' places).
    pub fn find_where(&self, type_id: u32, parameters: &[(u32, u32)], limit: usize, keep: impl Fn(&Session) -> bool) -> Vec<(Key, Session)> {
        let mut found: Vec<(Key, Session)> = self
            .lock()
            .sessions
            .iter()
            .filter(|(k, s)| k.0 == type_id && s.matches(parameters) && keep(s))
            .map(|(k, s)| (*k, s.clone()))
            .collect();
        found.sort_by_key(|(_, s)| (std::cmp::Reverse(s.participants.len()), s.created));
        found.truncate(limit);
        found
    }

    /// Whether `a` and `b` joined a session together (or host it), or one chose a session the
    /// other joined: a player a host merely added doesn't count, or adding someone would be a way
    /// to make their game reach you.
    ///
    /// `a` is the one asking to reach `b`: an invited player may reach the members, while they
    /// may not reach it until it's in.
    pub fn share(&self, a: u32, b: u32) -> bool {
        self.lock().sessions.values().any(|s| {
            if s.joined(a) && s.joined(b) {
                return true;
            }
            // A player who chose the session but isn't in yet reaches only its public members,
            // and only while a search could still offer it.
            let reaches = |w: u32, m: u32| s.wanting.contains(&w) && s.joined(m) && !s.private.contains(&m) && s.matches(&[]);
            let invited = s.invited.iter().any(|i| i.pid == a && i.at.elapsed() < INVITED_FOR) && s.joined(b);
            reaches(a, b) || reaches(b, a) || invited
        })
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
    fn a_search_is_offered_lobbies_whose_packs_it_has() {
        let sessions = Sessions::default();
        let lobby = |packs| {
            let key = sessions.create(10 + packs, 1, &[(3, 12), (7, 0), (116, 2), (PACKS, packs)]).unwrap();
            sessions.get(key).unwrap()
        };
        let (none, arctic, khyber, both) = (lobby(0), lobby(1), lobby(2), lobby(3));
        let search = |packs| [(3, 1), (116, 2), (PACKS, packs)];
        assert!(none.matches(&search(0)) && none.matches(&search(3)), "a base-game lobby is for everyone");
        assert!(arctic.matches(&search(1)) && arctic.matches(&search(3)));
        assert!(!arctic.matches(&search(0)) && !arctic.matches(&search(2)), "not without Arctic Strike");
        assert!(khyber.matches(&search(2)) && !khyber.matches(&search(1)));
        assert!(both.matches(&search(3)) && !both.matches(&search(1)) && !both.matches(&search(0)));
        assert!(!none.matches(&[(3, 1), (116, 3), (PACKS, 0)]), "the other properties still match exactly");
    }

    #[test]
    fn a_game_signing_in_again_is_in_none_of_its_old_sessions() {
        let sessions = Sessions::default();
        let lobby = sessions.create(10, 1, &[(3, 12), (7, 0)]).unwrap();
        let party = sessions.create(10, 1, &[(3, 0), (4, 4), (7, 1)]).unwrap();
        let shared = sessions.create(10, 1, &[(3, 12), (7, 0)]).unwrap();
        sessions.join(11, shared).unwrap();
        let other = sessions.create(12, 1, &[(3, 12), (7, 0)]).unwrap();
        sessions.join(10, other).unwrap();
        assert_eq!(sessions.signed_in_afresh(10), 4);
        assert!(sessions.get(lobby).is_none() && sessions.get(party).is_none(), "its own end");
        assert_eq!(sessions.get(shared).unwrap().host, 11, "a lobby with others in it goes to the next");
        assert!(!sessions.get(other).unwrap().has(10), "and it's out of others'");
        // Both players of a lobby restart, the joiner signing in first: the lobby ends all the same.
        assert_eq!(sessions.signed_in_afresh(11), 1);
        assert!(sessions.get(shared).is_none(), "nobody left in it");
    }

    #[test]
    fn a_joined_player_takes_over_only_from_a_host_who_has_gone() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 12)]).unwrap();
        sessions.join(11, key).unwrap();
        sessions.add(key, &[12], &[]).unwrap(); // added by the host, not joined
        assert!(!sessions.take_over(key, 12, |_| true), "only a player who joined");
        assert!(!sessions.take_over(key, 13, |_| true), "nor an outsider");
        assert!(!sessions.take_over(key, 11, |_| false), "the host is still there");
        assert!(!sessions.take_over(key, 11, |_| true), "and it may not ask again straight away");
        sessions.join(14, key).unwrap();
        assert!(sessions.take_over(key, 14, |p| p == 10), "another player, once the host has gone");
        assert_eq!(sessions.get(key).unwrap().host, 14);
        assert!(sessions.take_over(key, 14, |_| false), "the host asking again");
        assert!(!sessions.take_over((1, 999), 14, |_| true));
        assert!(sessions.is_member(11, key) && !sessions.is_member(12, key));
    }

    /// Joining takes an offer, a choice, an invitation, being added already or a free public
    /// slot; never once the host took the player out, until it asks them back.
    #[test]
    fn a_player_joins_only_a_session_it_may() {
        let sessions = Sessions::default();
        let party = sessions.create(10, 1, &[(7, 1), (3, 0), (4, 6)]).unwrap();
        assert_eq!(sessions.join(11, party), Err(Error::NotAllowed), "a key alone");
        assert_eq!(sessions.join(11, (1, 12345)), Err(Error::NotFound));
        sessions.offer(party, 11);
        sessions.join(11, party).unwrap();
        sessions.invite(party, 10, 12);
        sessions.join(12, party).unwrap();
        sessions.add(party, &[], &[13]).unwrap();
        sessions.join(13, party).unwrap();
        sessions.remove_by_host(party, &[12]).unwrap();
        assert_eq!(sessions.join(12, party), Err(Error::NotAllowed), "taken out by the host");
        sessions.invite(party, 11, 12);
        assert_eq!(sessions.join(12, party), Err(Error::NotAllowed), "not by another member's invitation");
        sessions.invite(party, 10, 12);
        sessions.join(12, party).unwrap();
        // Public slots: anyone, while one is free.
        let lobby = sessions.create(20, 1, &[(3, 2)]).unwrap();
        sessions.join(21, lobby).unwrap();
        assert_eq!(sessions.join(22, lobby), Err(Error::NotAllowed), "full");
        // A player leaving on its own may come back.
        sessions.remove(lobby, &[21]).unwrap();
        sessions.join(21, lobby).unwrap();
    }

    /// With as many sessions as can be kept, a new one ends the oldest with only its host in it.
    #[test]
    fn a_new_session_takes_the_place_of_the_oldest_lonely_one() {
        let sessions = Sessions::default();
        let busy = sessions.create(1, 1, &[(3, 12)]).unwrap();
        sessions.join(2, busy).unwrap();
        let lonely = sessions.create(3, 1, &[]).unwrap();
        // The rest, newer, straight into the table (made one by one, this took seconds).
        {
            let mut inner = sessions.lock();
            let model = inner.sessions[&lonely].clone();
            for n in 0..MAX_SESSIONS as u32 - 2 {
                let session = Session {
                    host: 1000 + n,
                    created: 1000 + u64::from(n),
                    ..model.clone()
                };
                inner.sessions.insert((1, 1_000_000 + n), session);
            }
        }
        assert_eq!(sessions.search(1, usize::MAX).len(), MAX_SESSIONS);
        sessions.create(99_999, 1, &[]).unwrap();
        assert_eq!(sessions.search(1, usize::MAX).len(), MAX_SESSIONS);
        assert!(sessions.get(lonely).is_none() && sessions.get(busy).is_some());
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
    fn hands_out_only_claims_that_can_be_passed_on() {
        let sessions = Sessions::default();
        let claimed = vec![url("192.168.1.20"), url("100.111.224.124"), url("8.8.8.8"), url("203.0.113.7"), url("not-an-ip")];
        sessions.set_urls(10, url("203.0.113.7"), claimed);
        let kept: Vec<String> = sessions.urls(10).iter().map(|u| u.address.clone()).collect();
        assert_eq!(kept, vec!["192.168.1.20", "100.111.224.124", "203.0.113.7"]);
    }

    #[test]
    fn the_games_urls_keep_their_parameters() {
        let sessions = Sessions::default();
        let game: StationURL = "udp:/address=192.168.0.11;port=2346;PID=1048576;RVCID=984263342".parse().unwrap();
        sessions.set_urls(10, "prudp:/address=192.168.0.11;port=2347".parse().unwrap(), vec![game.clone()]);
        assert_eq!(sessions.urls(10).iter().map(ToString::to_string).collect::<Vec<_>>(), vec![game.to_string()]);
    }

    #[test]
    fn a_player_behind_a_nat_gets_its_url_at_the_address_seen() {
        let sessions = Sessions::default();
        let game: StationURL = "udp:/address=8.8.8.8;port=2346;PID=7;RVCID=9".parse().unwrap();
        sessions.set_urls(10, "prudp:/address=203.0.113.7;port=40000".parse().unwrap(), vec![game]);
        let urls = sessions.urls(10);
        assert_eq!(urls.len(), 1);
        assert_eq!((urls[0].scheme.as_str(), urls[0].address.as_str(), urls[0].port), ("udp", "203.0.113.7", 2346));
        assert_eq!(urls[0].params.get("RVCID").map(String::as_str), Some("9"));
    }

    #[test]
    fn nothing_registered_gives_the_address_seen() {
        let sessions = Sessions::default();
        sessions.set_urls(10, url("203.0.113.7"), vec![]);
        assert_eq!(sessions.urls(10).iter().map(ToString::to_string).collect::<Vec<_>>(), vec![url("203.0.113.7").to_string()]);
        assert!(sessions.registered(10).is_empty());
        assert!(sessions.urls(11).is_empty());
    }

    #[test]
    fn players_at_one_house_are_given_each_others_private_addresses_first() {
        let sessions = Sessions::default();
        let url = |a: &str| format!("udp:/address={a};port=2346;PID=1;RVCID=2").parse::<StationURL>().unwrap();
        let observed: StationURL = "prudp:/address=198.51.100.7;port=2347".parse().unwrap();
        sessions.set_urls(1, observed, vec![url("198.51.100.7"), url("192.168.1.20")]);
        let addresses = |v: &str| sessions.urls_seen_from(1, v.parse().unwrap()).iter().map(|u| u.address.clone()).collect::<Vec<_>>();
        assert_eq!(addresses("198.51.100.7"), ["192.168.1.20", "198.51.100.7"], "same house");
        assert_eq!(addresses("203.0.113.9"), ["198.51.100.7", "192.168.1.20"], "elsewhere: as registered");
    }

    #[test]
    fn a_forwarded_port_comes_first_for_players_elsewhere() {
        let sessions = Sessions::default();
        let url = |a: &str, port: u16| format!("udp:/address={a};port={port};PID=1;RVCID=2").parse::<StationURL>().unwrap();
        let observed: StationURL = "prudp:/address=198.51.100.7;port=2347".parse().unwrap();
        sessions.set_urls(1, observed, vec![url("198.51.100.7", 56877), url("192.168.1.20", 2346)]);
        let local: SocketAddrV4 = "192.168.1.20:2346".parse().unwrap();
        assert_eq!(sessions.peer_at("198.51.100.7".parse().unwrap(), local), Some((1, Some(56877))));
        assert_eq!(sessions.peer_at("203.0.113.9".parse().unwrap(), local), None, "seen elsewhere");
        assert_eq!(sessions.peer_at("198.51.100.7".parse().unwrap(), "192.168.1.21:2346".parse().unwrap()), None);
        let shown = |v: &str| {
            sessions
                .urls_seen_from(1, v.parse().unwrap())
                .iter()
                .map(|u| format!("{}:{}", u.address, u.port))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown("203.0.113.9"), ["198.51.100.7:56877", "192.168.1.20:2346"], "no forward yet");
        let report = |forward: Option<u16>, seen: &str| Reachability {
            forward: forward.map(|p| (p, "UPnP".to_owned())),
            nat: "consistent".into(),
            seen: seen.parse().unwrap(),
            local: "192.168.1.20:2346".parse().unwrap(),
            at: Instant::now(),
        };
        sessions.set_reachability(1, report(Some(40000), "198.51.100.7"));
        assert_eq!(shown("203.0.113.9"), ["198.51.100.7:40000", "198.51.100.7:56877", "192.168.1.20:2346"]);
        let first = &sessions.urls_seen_from(1, "203.0.113.9".parse().unwrap())[0];
        assert_eq!(first.params.get("RVCID").map(String::as_str), Some("2"), "the game's parameters kept");
        assert_eq!(shown("198.51.100.7"), ["192.168.1.20:2346", "198.51.100.7:56877"], "same house: private first, no forward");
        sessions.set_reachability(1, report(Some(56877), "198.51.100.7"));
        assert_eq!(shown("203.0.113.9"), ["198.51.100.7:56877", "192.168.1.20:2346"], "the port it already had");
        sessions.set_reachability(1, report(Some(40000), "203.0.113.50"));
        assert_eq!(shown("203.0.113.9"), ["198.51.100.7:56877", "192.168.1.20:2346"], "made on another network");
        sessions.set_reachability(1, report(None, "198.51.100.7"));
        assert_eq!(sessions.forward_of(1), None);
        assert!(!sessions.punches_poorly(1), "consistent");
        let symmetric = Reachability {
            nat: "symmetric".into(),
            ..report(None, "198.51.100.7")
        };
        sessions.set_reachability(1, symmetric.clone());
        assert!(sessions.punches_poorly(1), "symmetric, no forward");
        sessions.set_reachability(
            1,
            Reachability {
                forward: Some((40000, "UPnP".into())),
                ..symmetric
            },
        );
        assert!(!sessions.punches_poorly(1), "symmetric, but forwarded");
    }

    /// A game that announced another adapter's address (a VPN's, a virtual machine's): the one
    /// its client found reaching the server comes first for housemates.
    #[test]
    fn housemates_get_the_address_the_client_found_when_the_game_announced_another() {
        let sessions = Sessions::default();
        let url = |a: &str, port: u16| format!("udp:/address={a};port={port};PID=1;RVCID=2").parse::<StationURL>().unwrap();
        let observed: StationURL = "prudp:/address=198.51.100.7;port=2347".parse().unwrap();
        sessions.set_urls(1, observed, vec![url("198.51.100.7", 2346), url("172.24.16.1", 2346)]);
        let lan: SocketAddrV4 = "192.168.1.20:2346".parse().unwrap();
        assert_eq!(sessions.socket_of(1, "198.51.100.7".parse().unwrap(), lan), Some((Some(2346), false)), "not announced");
        assert_eq!(sessions.socket_of(1, "203.0.113.9".parse().unwrap(), lan), None, "seen elsewhere");
        // Signed in, not yet in Multiplayer: only its connection to the server is registered,
        // which says nothing of its peer-to-peer socket.
        sessions.set_urls(
            3,
            "prudp:/address=198.51.100.7;port=2347".parse().unwrap(),
            vec!["prudp:/address=198.51.100.7;port=2347;sid=15;type=3".parse().unwrap()],
        );
        assert_eq!(
            sessions.socket_of(3, "198.51.100.7".parse().unwrap(), lan),
            Some((None, true)),
            "nothing to forward yet, nothing to warn of"
        );
        let report = Reachability {
            forward: None,
            nat: "consistent".into(),
            seen: "198.51.100.7".parse().unwrap(),
            local: "192.168.1.20:2346".parse().unwrap(),
            at: Instant::now(),
        };
        sessions.set_reachability(1, report);
        let shown = |v: &str| {
            sessions
                .urls_seen_from(1, v.parse().unwrap())
                .iter()
                .map(|u| format!("{}:{}", u.address, u.port))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown("198.51.100.7"), ["192.168.1.20:2346", "172.24.16.1:2346", "198.51.100.7:2346"], "housemates");
        assert_eq!(shown("203.0.113.9"), ["198.51.100.7:2346", "172.24.16.1:2346"], "elsewhere: as registered");
        let first = &sessions.urls_seen_from(1, "198.51.100.7".parse().unwrap())[0];
        assert_eq!(first.params.get("RVCID").map(String::as_str), Some("2"), "the game's parameters kept");
    }

    #[test]
    fn taken_slots_are_counted_by_kind() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 12), (4, 0), (5, 99)]).unwrap();
        sessions.add(key, &[11, 12], &[13]).unwrap();
        let props = sessions.get(key).unwrap().properties();
        assert_eq!(props, vec![(3, 12), (4, 0), (5, 3), (6, 1)]);
        // The host moved to a private slot, and a player leaving frees theirs.
        sessions.add(key, &[], &[10]).unwrap();
        sessions.remove(key, &[13]).unwrap();
        let props = sessions.get(key).unwrap().properties();
        assert_eq!(&props[2..], &[(5, 2), (6, 1)]);
    }

    #[test]
    fn a_lobby_at_its_size_is_not_offered_though_public_slots_are_free() {
        let sessions = Sessions::default();
        // A squad member's lobby for 2: 2 public slots and 12 invite-only, the host in one of those.
        let key = sessions.create(10, 1, &[(7, 0), (3, 2), (4, 12), (116, 2), (114, 2)]).unwrap();
        sessions.add(key, &[], &[10]).unwrap();
        let search = [(3, 1), (116, 2), (114, 12)];
        assert!(sessions.get(key).unwrap().matches(&search));
        sessions.add(key, &[11], &[]).unwrap();
        assert!(!sessions.get(key).unwrap().matches(&search), "2 in a lobby for 2");
    }

    #[test]
    fn a_smaller_lobby_is_offered_to_searches_for_its_size() {
        let sessions = Sessions::default();
        let small = sessions.create(10, 1, &[(7, 0), (3, 4), (4, 0), (116, 2), (114, 4)]).unwrap();
        let search = [(3, 1), (116, 2), (114, 12)];
        assert!(sessions.get(small).unwrap().matches(&search));
        // Not a bigger one to a smaller search, and not once its own slots are taken.
        assert!(!sessions.get(small).unwrap().matches(&[(3, 1), (116, 2), (114, 2)]));
        sessions.add(small, &[11, 12, 13], &[]).unwrap();
        assert!(!sessions.get(small).unwrap().matches(&search));
    }

    #[test]
    fn a_player_joins_by_choosing_an_offered_session_and_being_added() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 12)]).unwrap();
        let other = sessions.create(20, 1, &[(3, 12)]).unwrap();
        // Added without asking: nothing.
        sessions.add(key, &[11], &[]).unwrap();
        assert!(!sessions.share(10, 11));
        // Offered both by a search: still nothing, and neither host may add them in.
        sessions.offer(key, 12);
        sessions.offer(other, 12);
        sessions.add(other, &[12], &[]).unwrap();
        assert!(!sessions.share(20, 12));
        // Choosing one (asking to reach its host) lets it reach that session's members only.
        sessions.choose(12, 10);
        assert!(sessions.share(10, 12));
        assert!(!sessions.share(20, 12));
        // A host it wasn't offered can't be chosen.
        sessions.choose(12, 30);
        assert!(sessions.share(10, 12));
        sessions.add(key, &[12], &[]).unwrap();
        assert!(sessions.get(key).unwrap().joined(12));
        // After the host leaves, the players who joined still share it.
        sessions.offer(key, 13);
        sessions.choose(13, 10);
        sessions.add(key, &[13], &[]).unwrap();
        sessions.remove(key, &[10]).unwrap();
        assert!(sessions.share(12, 13));
    }

    #[test]
    fn a_choice_reaches_only_public_members_of_a_session_still_offered() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 12)]).unwrap();
        sessions.offer(key, 14);
        sessions.choose(14, 10);
        sessions.add(key, &[11], &[12]).unwrap();
        sessions.offer(key, 11);
        sessions.choose(11, 10);
        sessions.add(key, &[11], &[]).unwrap();
        assert!(sessions.share(14, 11), "a public member");
        sessions.offer(key, 12);
        sessions.choose(12, 10);
        sessions.add(key, &[], &[12]).unwrap();
        assert!(!sessions.share(14, 12), "a private member");
        // Made invite-only: the choice goes, and an old offer can't be chosen.
        sessions.offer(key, 15);
        sessions.update(10, key, &[(3, 0)]).unwrap();
        assert!(!sessions.share(14, 10));
        sessions.choose(15, 10);
        assert!(!sessions.share(15, 10));
    }

    #[test]
    fn an_invited_player_reaches_the_members_and_joins_only_once_it_accepts() {
        let sessions = Sessions::default();
        let party = sessions.create(10, 1, &[(7, 1), (3, 0), (4, 6)]).unwrap();
        assert!(!sessions.share(11, 10));
        sessions.invite(party, 10, 11);
        assert!(sessions.share(11, 10), "the invited player reaches the members");
        assert!(!sessions.share(10, 11), "but not the other way");
        // Added before accepting: a participant, not a member.
        sessions.add(party, &[], &[11]).unwrap();
        assert!(!sessions.get(party).unwrap().joined(11));
        sessions.answer(party, 11, true);
        sessions.add(party, &[], &[11]).unwrap();
        assert!(sessions.get(party).unwrap().joined(11));
        // Declined, or taken back: nothing.
        sessions.invite(party, 10, 12);
        sessions.answer(party, 12, false);
        assert!(!sessions.share(12, 10));
        sessions.invite(party, 10, 13);
        sessions.uninvite(party, 10, 13);
        assert!(!sessions.share(13, 10));
        // Accepting without an invitation does nothing.
        sessions.answer(party, 14, true);
        sessions.add(party, &[14], &[]).unwrap();
        assert!(!sessions.get(party).unwrap().joined(14));
        // The inviter leaving takes its invitations with it.
        sessions.invite(party, 11, 15);
        sessions.remove(party, &[11]).unwrap();
        assert!(!sessions.share(15, 10));
    }

    #[test]
    fn two_players_merely_wanting_a_session_dont_share_it() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 12)]).unwrap();
        for p in [12, 13] {
            sessions.offer(key, p);
            sessions.choose(p, 10);
        }
        assert!(!sessions.share(12, 13));
    }

    #[test]
    fn only_players_who_joined_count_as_joined() {
        let sessions = Sessions::default();
        let key = sessions.create(10, 1, &[(3, 12)]).unwrap();
        sessions.add(key, &[11], &[]).unwrap();
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
        sessions.add(party, &[12], &[]).unwrap();
        assert!(!sessions.share(10, 12), "added by the host, not joined");
        assert!(!sessions.hosts_public(10), "a party isn't public");
        sessions.create(20, 1, &QUICK_MATCH).unwrap();
        assert!(sessions.hosts_public(20));
        assert!(!sessions.hosts_public(21));
    }
}
