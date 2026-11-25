//! Squads (protocol 110, the game's clans): made, joined by invitation, run by their leader,
//! kept in the server's database. The game's own protocol, not among its declarations; its
//! methods and their layouts were worked out from the game's code:
//!
//! ```text
//!  1 CreateClan(name, tag, motto)          -> clan id, result
//!  2 UpdateClan(tag, motto, u8 mask)       -> result        (bit 0 tag, bit 1 motto)
//!  3 GetClanList(list<clan id>)            -> list<ClanInfo>
//!  4 DeleteClan()                          -> result        (the game never calls it)
//!  5 Invite(pid, u32 7)                    -> result
//!  6 InviteByName(name, u32 7)             -> result
//!  7 RemoveInvite(pid, clan id or 0)       -> result        (decline, or cancel one sent)
//!  8 Join(clan id)                         -> result        (accept an invitation)
//!  9 Leave(pid)                            -> result        (one's own pid: leave; else kick)
//! 11 ChangeRole(pid, role)                 -> result        (role 0: hand over the lead)
//! 12 ChangePrivilege(role, mask)           -> result
//! 13 GetCurUserClan()                      -> clan id (0: none)
//! 14 GetMembers(clan id)                   -> list<Member>
//! 15 SetRoom(clan id, session, u8 room, u8 state, u8) -> result
//! 16 GetRooms(u8 first, u8 count)          -> list<Room>    (Room: u8 room, u8 state, session)
//! 17 JoinRoom(u8 room)                     -> nothing
//! 18 SetRoomState(u32, u8, u8)             -> nothing
//! 19 Leaderboard(u8, u32, u32)             -> list, u32, u32 (none yet)
//! 20 SetProperty(clan id, list<Change>)    -> nothing   (Change: u32 value, u8 id, u8 how)
//! 21 RemoveEvent(event id)                 -> result
//! 22 SetEvent(ClanEvent)                   -> result        (the message of the day: type 1)
//! 23 GetEvents(clan id, u8 type, from, to) -> list<ClanEvent>
//! 24 ChangeEventType(event id, u8 type)    -> result
//! 26 GetClanIds(list<pid>)                 -> list<clan id>, one per player
//! 27 GetClanTags(list<clan id>)            -> list<tag>
//! 28 GetInvitesWithNames(clan id or 0)     -> list<InviteWithName>, result
//! ```
//!
//! A ClanInfo carries the squad's member count (a u8 after the motto: the invitation screen's
//! "Number of Members") and its properties, a list of u32 by id that the game itself keeps up
//! after squad matches with SetProperty: 0 rating, 1 wins, 2 losses, 4 and 5 experience (the
//! screen adds them), 6 to 12 wins per mode, 20 level. A change adds to the value (how 0), keeps
//! the larger (3) or the smaller (4).
//!
//! Squad rooms: room 1 is Squad HQ (the first squadmate to open it hosts it), rooms 2 to 5 the
//! "Squad Parties" 1 to 4 under Play Squad Quick Match. A squad member who opens one becomes its
//! host, makes a game session for it, and tells the server with SetRoom: the squad, the room, the
//! session as the game writes it (its own serialised session handle, kept as it came and never
//! read here) and the room's state (0 open, 1 its match under way; 2 or 4 closed, with an empty
//! session). Squadmates list the rooms with GetRooms ("Party N [Vacancy]"); choosing one, a game
//! looks the session up itself and joins it, the server having its host probe them (`nat.rs`),
//! then says so with JoinRoom. Rooms are kept in memory only, as they last no longer than their
//! host's game: one room per host, cleared when the host sets an empty session, and left out of
//! the list once the host is offline or no longer in the squad.
//!
//! Roles: 0 leader, 1 lieutenant, 2 sergeant, 3 recruit; lower is higher. A leader may leave
//! only once someone else leads (the game then asks who, and hands over with ChangeRole), or
//! as the last member, which ends the squad.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use quazal::prudp::ClientRegistry;
use quazal::rmc::Error;
use quazal::rmc::Protocol;
use quazal::rmc::Request;
use quazal::ClientInfo;
use quazal::Context;
use rusqlite::params;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use slog::debug;
use slog::info;
use slog::Logger;

use crate::accounts::Accounts;
use crate::online::Online;

/// The game's result codes (it shows a message for each).
pub mod result {
    pub const OK: u32 = 0;
    pub const FAILED: u32 = 2;
    pub const NAME_TAKEN: u32 = 801;
    pub const ALREADY_IN_A_SQUAD: u32 = 802;
    pub const FULL: u32 = 803;
    pub const NOT_A_MEMBER: u32 = 804;
    pub const ALREADY_INVITED: u32 = 805;
    pub const NOT_ALLOWED: u32 = 806;
    pub const INVITATION_GONE: u32 = 807;
    pub const NOT_ON_THE_LEADER: u32 = 808;
    pub const TOO_MANY_INVITES_FROM_SQUAD: u32 = 812;
    pub const TOO_MANY_INVITES_TO_PLAYER: u32 = 813;
}

pub const LEADER: u8 = 0;
pub const LIEUTENANT: u8 = 1;
pub const SERGEANT: u8 = 2;
pub const RECRUIT: u8 = 3;

/// Limits. The game checks the lengths in characters before sending (name 30, tag 3, motto 80),
/// and copies these strings into fixed buffers: what doesn't fit would crash it, so the server
/// holds them to the buffers too (in bytes, with the NUL).
const MAX_NAME_CHARS: usize = 30;
const MAX_TAG_CHARS: usize = 3;
const MAX_MOTTO_CHARS: usize = 80;
const MAX_NAME_BYTES: usize = 63;
const MAX_TAG_BYTES: usize = 5;
const MAX_TEXT_BYTES: usize = 255;
const MAX_MEMBERS: i64 = 64;
const MAX_INVITES_FROM_SQUAD: i64 = 50;
const MAX_INVITES_TO_PLAYER: i64 = 20;
/// How long an invitation stays open (the game sends 7 with each: days, it seems).
const INVITE_DAYS: i64 = 7;
/// The events (messages of the day) a squad keeps.
const MAX_EVENTS: i64 = 20;
/// The squad properties the game keeps (by id).
const MAX_PROPERTIES: usize = 21;
/// Properties the game holds to a range: the rating (100 to 3000) and the level (up to 20).
const RATING: u8 = 0;
const RATING_RANGE: (u32, u32) = (100, 3000);
const LEVEL: u8 = 20;
const MAX_LEVEL: u32 = 20;
/// The most one change may add (a match brings a few thousand experience at most), or raise a
/// value by when it keeps the larger (how 3); for the counts of matches (wins and losses, and
/// wins per mode), a few matches' worth.
const MAX_ADDED: u32 = 100_000;
const MAX_COUNT_ADDED: u32 = 10;
/// The properties that count matches: 1 wins, 2 losses, 6 to 12 wins per mode.
const COUNTS: [u8; 9] = [1, 2, 6, 7, 8, 9, 10, 11, 12];
/// The most any other property may be (experience, mostly): a squad of 50 playing every day for
/// years stays well within it.
const MAX_VALUE: u32 = 1_000_000_000;
/// The most a count of matches may be.
const MAX_COUNT: u32 = 10_000_000;

/// A squad property's new value after a member's change (`how` 0 adds `value`, 3 keeps the
/// larger, 4 the smaller), or none for a change the game doesn't make (an unknown `how`, or an
/// addition of more than one match brings). The game's own changes only: whatever a member
/// sends, the rating stays within 100 to 3000, the level within 20, a count of matches grows by
/// a few at a time, and nothing past [`MAX_COUNT`] or [`MAX_VALUE`].
fn changed_property(id: u8, old: u32, value: u32, how: u8) -> Option<u32> {
    let counts = COUNTS.contains(&id);
    let step = if counts { MAX_COUNT_ADDED } else { MAX_ADDED };
    let new = match how {
        0 if value <= step => old.saturating_add(value),
        3 => old.max(value),
        4 if old == 0 => value,
        4 => old.min(value),
        _ => return None,
    };
    // Raised by at most a step at once, however it was asked.
    let new = new.min(old.saturating_add(step));
    Some(match id {
        RATING => new.clamp(RATING_RANGE.0, RATING_RANGE.1),
        LEVEL => new.min(MAX_LEVEL),
        _ if counts => new.min(MAX_COUNT),
        _ => new.min(MAX_VALUE),
    })
}
/// The most property changes one SetProperty may carry.
const MAX_CHANGES: u32 = 64;
/// The most squads or players one request may ask about.
const MAX_ASKED: u32 = 256;

/// What a role may do: invite and cancel invitations (sergeant and up), change the squad's tag,
/// motto and message, and kick or promote those below (lieutenant and up).
const INVITES: u8 = SERGEANT;
const MANAGES: u8 = LIEUTENANT;
/// Each role's privileges, as the game is told them (it decides its menus by rank).
const ROLE_PRIVILEGES: [u16; 4] = [0xffff, 0x00ff, 0x000f, 0];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clan {
    pub id: u32,
    pub name: String,
    pub tag: String,
    pub motto: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub pid: u32,
    pub role: u8,
    pub joined: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub clan: u32,
    pub invitee: u32,
    pub inviter: u32,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: u32,
    pub clan: u32,
    pub author: u32,
    pub text: String,
    pub kind: u8,
    pub at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn fits(s: &str, chars: usize, bytes: usize) -> bool {
    s.chars().count() <= chars && s.len() <= bytes && !s.contains('\0')
}

/// The squads, in the server's database.
pub struct Store {
    db: Mutex<Connection>,
}

type Res<T> = rusqlite::Result<T>;

impl Store {
    pub fn open(path: &Path) -> Res<Self> {
        Self::with(Connection::open(path)?)
    }

    #[cfg(test)]
    fn in_memory() -> Res<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> Res<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS clans (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                tag TEXT NOT NULL,
                motto TEXT NOT NULL,
                created INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS clan_members (
                pid INTEGER PRIMARY KEY,
                clan INTEGER NOT NULL,
                role INTEGER NOT NULL,
                joined INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS clan_members_clan ON clan_members (clan);
            CREATE TABLE IF NOT EXISTS clan_invites (
                clan INTEGER NOT NULL,
                invitee INTEGER NOT NULL,
                inviter INTEGER NOT NULL,
                at INTEGER NOT NULL,
                PRIMARY KEY (clan, invitee)
            ) WITHOUT ROWID;
            CREATE INDEX IF NOT EXISTS clan_invites_invitee ON clan_invites (invitee);
            CREATE TABLE IF NOT EXISTS clan_events (
                id INTEGER PRIMARY KEY,
                clan INTEGER NOT NULL,
                author INTEGER NOT NULL,
                text TEXT NOT NULL,
                kind INTEGER NOT NULL,
                at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS clan_properties (
                clan INTEGER NOT NULL,
                prop INTEGER NOT NULL,
                value INTEGER NOT NULL,
                PRIMARY KEY (clan, prop)
            ) WITHOUT ROWID;",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn membership(db: &Connection, pid: u32) -> Res<Option<(u32, u8)>> {
        db.query_row("SELECT clan, role FROM clan_members WHERE pid = ?1", [pid], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
    }

    fn drop_old_invites(db: &Connection) -> Res<()> {
        db.execute("DELETE FROM clan_invites WHERE at < ?1", [now() - INVITE_DAYS * 86_400])?;
        Ok(())
    }

    /// The squad `pid` is in (0: none).
    pub fn clan_of(&self, pid: u32) -> Res<u32> {
        Ok(Self::membership(&self.lock(), pid)?.map_or(0, |(c, _)| c))
    }

    /// Makes a squad led by `pid`: (clan id, result).
    pub fn create(&self, pid: u32, name: &str, tag: &str, motto: &str) -> Res<(u32, u32)> {
        let name = name.trim();
        if name.is_empty() || !fits(name, MAX_NAME_CHARS, MAX_NAME_BYTES) || !fits(tag, MAX_TAG_CHARS, MAX_TAG_BYTES) || !fits(motto, MAX_MOTTO_CHARS, MAX_TEXT_BYTES) {
            return Ok((0, result::FAILED));
        }
        let mut db = self.lock();
        let tx = db.transaction()?;
        if Self::membership(&tx, pid)?.is_some() {
            return Ok((0, result::ALREADY_IN_A_SQUAD));
        }
        if tx.query_row("SELECT 1 FROM clans WHERE name = ?1", [name], |_| Ok(())).optional()?.is_some() {
            return Ok((0, result::NAME_TAKEN));
        }
        tx.execute("INSERT INTO clans (name, tag, motto, created) VALUES (?1, ?2, ?3, ?4)", params![name, tag, motto, now()])?;
        let id = u32::try_from(tx.last_insert_rowid()).unwrap_or(0);
        tx.execute(
            "INSERT INTO clan_members (pid, clan, role, joined) VALUES (?1, ?2, ?3, ?4)",
            params![pid, id, LEADER, now()],
        )?;
        tx.execute("DELETE FROM clan_invites WHERE invitee = ?1", [pid])?;
        tx.commit()?;
        Ok((id, result::OK))
    }

    /// Changes the tag and/or motto of `pid`'s squad (`mask` bit 0 tag, bit 1 motto).
    pub fn update(&self, pid: u32, tag: &str, motto: &str, mask: u8) -> Res<u32> {
        let db = self.lock();
        let Some((clan, role)) = Self::membership(&db, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if role > MANAGES {
            return Ok(result::NOT_ALLOWED);
        }
        if (mask & 1 != 0 && !fits(tag, MAX_TAG_CHARS, MAX_TAG_BYTES)) || (mask & 2 != 0 && !fits(motto, MAX_MOTTO_CHARS, MAX_TEXT_BYTES)) {
            return Ok(result::FAILED);
        }
        if mask & 1 != 0 {
            db.execute("UPDATE clans SET tag = ?1 WHERE id = ?2", params![tag, clan])?;
        }
        if mask & 2 != 0 {
            db.execute("UPDATE clans SET motto = ?1 WHERE id = ?2", params![motto, clan])?;
        }
        Ok(result::OK)
    }

    pub fn clan(&self, id: u32) -> Res<Option<Clan>> {
        self.lock()
            .query_row("SELECT id, name, tag, motto FROM clans WHERE id = ?1", [id], |r| {
                Ok(Clan {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    tag: r.get(2)?,
                    motto: r.get(3)?,
                })
            })
            .optional()
    }

    pub fn member_count(&self, clan: u32) -> Res<u32> {
        self.lock().query_row("SELECT COUNT(*) FROM clan_members WHERE clan = ?1", [clan], |r| r.get(0))
    }

    /// A squad's properties by id, up to the last one kept (unkept ones are 0).
    pub fn properties(&self, clan: u32) -> Res<Vec<u32>> {
        let db = self.lock();
        let mut q = db.prepare_cached("SELECT prop, value FROM clan_properties WHERE clan = ?1")?;
        let mut props = Vec::new();
        for row in q.query_map([clan], |r| Ok((r.get::<_, usize>(0)?, r.get::<_, u32>(1)?)))? {
            let (id, value) = row?;
            if id >= props.len() {
                props.resize(id + 1, 0);
            }
            props[id] = value;
        }
        Ok(props)
    }

    /// Changes to a squad's properties, from one of its members after a match; others' are
    /// ignored, as are ids the game doesn't keep.
    pub fn change_properties(&self, pid: u32, clan: u32, changes: &[(u32, u8, u8)]) -> Res<()> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        if Self::membership(&tx, pid)?.map(|(c, _)| c) != Some(clan) {
            return Ok(());
        }
        for &(value, id, how) in changes {
            if usize::from(id) >= MAX_PROPERTIES {
                continue;
            }
            let old: u32 = tx
                .query_row("SELECT value FROM clan_properties WHERE clan = ?1 AND prop = ?2", params![clan, id], |r| r.get(0))
                .optional()?
                .unwrap_or(0);
            let Some(new) = changed_property(id, old, value, how) else {
                continue;
            };
            tx.execute(
                "INSERT INTO clan_properties (clan, prop, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (clan, prop) DO UPDATE SET value = excluded.value",
                params![clan, id, new],
            )?;
        }
        tx.commit()
    }

    pub fn members(&self, clan: u32) -> Res<Vec<Member>> {
        let db = self.lock();
        let mut q = db.prepare_cached("SELECT pid, role, joined FROM clan_members WHERE clan = ?1 ORDER BY role, joined")?;
        let rows = q.query_map([clan], |r| {
            Ok(Member {
                pid: r.get(0)?,
                role: r.get(1)?,
                joined: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// `pid` invites `invitee` into `pid`'s squad.
    pub fn invite(&self, pid: u32, invitee: u32) -> Res<u32> {
        let db = self.lock();
        Self::drop_old_invites(&db)?;
        let Some((clan, role)) = Self::membership(&db, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if role > INVITES {
            return Ok(result::NOT_ALLOWED);
        }
        if invitee == pid {
            return Ok(result::FAILED);
        }
        if let Some((theirs, _)) = Self::membership(&db, invitee)? {
            return Ok(if theirs == clan { result::FAILED } else { result::ALREADY_IN_A_SQUAD });
        }
        if db
            .query_row("SELECT 1 FROM clan_invites WHERE clan = ?1 AND invitee = ?2", [clan, invitee], |_| Ok(()))
            .optional()?
            .is_some()
        {
            return Ok(result::ALREADY_INVITED);
        }
        let count = |sql: &str, id: u32| -> Res<i64> { db.query_row(sql, [id], |r| r.get(0)) };
        if count("SELECT COUNT(*) FROM clan_invites WHERE clan = ?1", clan)? >= MAX_INVITES_FROM_SQUAD {
            return Ok(result::TOO_MANY_INVITES_FROM_SQUAD);
        }
        if count("SELECT COUNT(*) FROM clan_invites WHERE invitee = ?1", invitee)? >= MAX_INVITES_TO_PLAYER {
            return Ok(result::TOO_MANY_INVITES_TO_PLAYER);
        }
        db.execute(
            "INSERT INTO clan_invites (clan, invitee, inviter, at) VALUES (?1, ?2, ?3, ?4)",
            params![clan, invitee, pid, now()],
        )?;
        Ok(result::OK)
    }

    /// Takes away an invitation: the invitee declining `clan`'s (when `pid` is the invitee), or a
    /// member cancelling one their squad sent `invitee` (clan 0: their own squad).
    pub fn remove_invite(&self, pid: u32, invitee: u32, clan: u32) -> Res<u32> {
        let db = self.lock();
        if invitee == pid {
            let gone = db.execute("DELETE FROM clan_invites WHERE invitee = ?1 AND clan = ?2", [pid, clan])?;
            return Ok(if gone > 0 { result::OK } else { result::INVITATION_GONE });
        }
        let Some((mine, role)) = Self::membership(&db, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if (clan != 0 && clan != mine) || role > INVITES {
            return Ok(result::NOT_ALLOWED);
        }
        let gone = db.execute("DELETE FROM clan_invites WHERE invitee = ?1 AND clan = ?2", [invitee, mine])?;
        Ok(if gone > 0 { result::OK } else { result::INVITATION_GONE })
    }

    /// `pid` accepts `clan`'s invitation.
    pub fn join(&self, pid: u32, clan: u32) -> Res<u32> {
        let mut db = self.lock();
        Self::drop_old_invites(&db)?;
        let tx = db.transaction()?;
        if tx
            .query_row("SELECT 1 FROM clan_invites WHERE clan = ?1 AND invitee = ?2", [clan, pid], |_| Ok(()))
            .optional()?
            .is_none()
        {
            return Ok(result::INVITATION_GONE);
        }
        if Self::membership(&tx, pid)?.is_some() {
            return Ok(result::ALREADY_IN_A_SQUAD);
        }
        let members: i64 = tx.query_row("SELECT COUNT(*) FROM clan_members WHERE clan = ?1", [clan], |r| r.get(0))?;
        if members >= MAX_MEMBERS {
            return Ok(result::FULL);
        }
        tx.execute(
            "INSERT INTO clan_members (pid, clan, role, joined) VALUES (?1, ?2, ?3, ?4)",
            params![pid, clan, RECRUIT, now()],
        )?;
        tx.execute("DELETE FROM clan_invites WHERE invitee = ?1", [pid])?;
        tx.commit()?;
        Ok(result::OK)
    }

    /// `pid` leaves (`target` is `pid`) or kicks `target`. A leader leaves only as the last
    /// member, which ends the squad, or once someone else leads.
    pub fn leave(&self, pid: u32, target: u32) -> Res<u32> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some((clan, role)) = Self::membership(&tx, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if target != pid {
            let Some((theirs, their_role)) = Self::membership(&tx, target)? else {
                return Ok(result::NOT_A_MEMBER);
            };
            if theirs != clan || role > MANAGES || their_role <= role {
                return Ok(result::NOT_ALLOWED);
            }
            tx.execute("DELETE FROM clan_members WHERE pid = ?1", [target])?;
            tx.commit()?;
            return Ok(result::OK);
        }
        let others: i64 = tx.query_row("SELECT COUNT(*) FROM clan_members WHERE clan = ?1 AND pid != ?2", [clan, pid], |r| r.get(0))?;
        if role == LEADER && others > 0 {
            return Ok(result::NOT_ON_THE_LEADER);
        }
        tx.execute("DELETE FROM clan_members WHERE pid = ?1", [pid])?;
        if others == 0 {
            tx.execute("DELETE FROM clans WHERE id = ?1", [clan])?;
            tx.execute("DELETE FROM clan_invites WHERE clan = ?1", [clan])?;
            tx.execute("DELETE FROM clan_events WHERE clan = ?1", [clan])?;
            tx.execute("DELETE FROM clan_properties WHERE clan = ?1", [clan])?;
        }
        tx.commit()?;
        Ok(result::OK)
    }

    /// The leader ends the squad (the game never asks; kept for completeness).
    pub fn delete(&self, pid: u32) -> Res<u32> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some((clan, role)) = Self::membership(&tx, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if role != LEADER {
            return Ok(result::NOT_ALLOWED);
        }
        for table in ["clan_members", "clan_invites", "clan_events", "clan_properties"] {
            tx.execute(&format!("DELETE FROM {table} WHERE clan = ?1"), [clan])?;
        }
        tx.execute("DELETE FROM clans WHERE id = ?1", [clan])?;
        tx.commit()?;
        Ok(result::OK)
    }

    /// `pid` gives `target` a role below their own; the leader giving role 0 hands over the lead
    /// (and becomes a lieutenant).
    pub fn change_role(&self, pid: u32, target: u32, role: u8) -> Res<u32> {
        if role > RECRUIT {
            return Ok(result::FAILED);
        }
        let mut db = self.lock();
        let tx = db.transaction()?;
        let Some((clan, mine)) = Self::membership(&tx, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        let Some((theirs, current)) = Self::membership(&tx, target)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if theirs != clan || target == pid {
            return Ok(result::NOT_ALLOWED);
        }
        if role == LEADER {
            if mine != LEADER {
                return Ok(result::NOT_ALLOWED);
            }
            tx.execute("UPDATE clan_members SET role = ?1 WHERE pid = ?2", params![LIEUTENANT, pid])?;
        } else if mine > MANAGES || current <= mine || role <= mine {
            return Ok(result::NOT_ALLOWED);
        }
        tx.execute("UPDATE clan_members SET role = ?1 WHERE pid = ?2", params![role, target])?;
        tx.commit()?;
        Ok(result::OK)
    }

    /// Open invitations: those `pid` received (`clan` 0), or those `pid`'s squad sent.
    pub fn invites(&self, pid: u32, clan: u32) -> Res<Vec<Invite>> {
        let db = self.lock();
        Self::drop_old_invites(&db)?;
        let (sql, id) = if clan == 0 {
            ("SELECT clan, invitee, inviter, at FROM clan_invites WHERE invitee = ?1 ORDER BY at", pid)
        } else {
            match Self::membership(&db, pid)? {
                Some((mine, _)) if mine == clan => ("SELECT clan, invitee, inviter, at FROM clan_invites WHERE clan = ?1 ORDER BY at", clan),
                _ => return Ok(Vec::new()),
            }
        };
        let mut q = db.prepare_cached(sql)?;
        let rows = q.query_map([id], |r| {
            Ok(Invite {
                clan: r.get(0)?,
                invitee: r.get(1)?,
                inviter: r.get(2)?,
                at: r.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Adds an event to `pid`'s squad, or replaces the one with `id` (the message of the day).
    pub fn set_event(&self, pid: u32, id: u32, text: &str, kind: u8) -> Res<u32> {
        if text.len() > MAX_TEXT_BYTES || text.contains('\0') {
            return Ok(result::FAILED);
        }
        let db = self.lock();
        let Some((clan, role)) = Self::membership(&db, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if role > MANAGES {
            return Ok(result::NOT_ALLOWED);
        }
        let replaced = db.execute(
            "UPDATE clan_events SET text = ?1, kind = ?2, author = ?3, at = ?4 WHERE id = ?5 AND clan = ?6",
            params![text, kind, pid, now(), id, clan],
        )?;
        if replaced == 0 {
            db.execute(
                "INSERT INTO clan_events (clan, author, text, kind, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![clan, pid, text, kind, now()],
            )?;
            db.execute(
                "DELETE FROM clan_events WHERE clan = ?1 AND id NOT IN (SELECT id FROM clan_events WHERE clan = ?1 ORDER BY id DESC LIMIT ?2)",
                params![clan, MAX_EVENTS],
            )?;
        }
        Ok(result::OK)
    }

    pub fn remove_event(&self, pid: u32, id: u32) -> Res<u32> {
        let db = self.lock();
        let Some((clan, role)) = Self::membership(&db, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if role > MANAGES {
            return Ok(result::NOT_ALLOWED);
        }
        db.execute("DELETE FROM clan_events WHERE id = ?1 AND clan = ?2", params![id, clan])?;
        Ok(result::OK)
    }

    pub fn change_event_type(&self, pid: u32, id: u32, kind: u8) -> Res<u32> {
        let db = self.lock();
        let Some((clan, role)) = Self::membership(&db, pid)? else {
            return Ok(result::NOT_A_MEMBER);
        };
        if role > MANAGES {
            return Ok(result::NOT_ALLOWED);
        }
        db.execute("UPDATE clan_events SET kind = ?1 WHERE id = ?2 AND clan = ?3", params![kind, id, clan])?;
        Ok(result::OK)
    }

    /// A squad's events of a type (0: any), oldest first; only its members see them.
    pub fn events(&self, pid: u32, clan: u32, kind: u8) -> Res<Vec<Event>> {
        let db = self.lock();
        if Self::membership(&db, pid)?.map(|(c, _)| c) != Some(clan) {
            return Ok(Vec::new());
        }
        let mut q = db.prepare_cached("SELECT id, clan, author, text, kind, at FROM clan_events WHERE clan = ?1 AND (?2 = 0 OR kind = ?2) ORDER BY id")?;
        let rows = q.query_map(params![clan, kind], |r| {
            Ok(Event {
                id: r.get(0)?,
                clan: r.get(1)?,
                author: r.get(2)?,
                text: r.get(3)?,
                kind: r.get(4)?,
                at: r.get(5)?,
            })
        })?;
        rows.collect()
    }
}

// Encoding: little-endian; a String is a u16 length counting its NUL, the bytes and the NUL (an
// empty one is just a 0 length); a list is a u32 count and its items; a date-time is Quazal's
// packed u64.

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.extend_from_slice(&[0, 0]);
        return;
    }
    #[allow(clippy::cast_possible_truncation)] // limited to a few hundred bytes when kept
    out.extend_from_slice(&((s.len() + 1) as u16).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// A String of bytes kept as they came (a room's session), with its NUL.
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.is_empty() {
        out.extend_from_slice(&[0, 0]);
        return;
    }
    #[allow(clippy::cast_possible_truncation)] // at most MAX_SESSION_BYTES
    out.extend_from_slice(&((bytes.len() + 1) as u16).to_le_bytes());
    out.extend_from_slice(bytes);
    out.push(0);
}

fn put_time(out: &mut Vec<u8>, unix: i64) {
    out.extend_from_slice(&crate::fixed::packed_date_time(u64::try_from(unix).unwrap_or(0)).to_le_bytes());
}

#[allow(clippy::cast_possible_truncation)] // every list is capped well below u32::MAX
fn put_len(out: &mut Vec<u8>, n: usize) {
    put_u32(out, n as u32);
}

fn clan_info(out: &mut Vec<u8>, clan: &Clan, members: u32, properties: &[u32]) {
    put_u32(out, clan.id);
    put_str(out, &clan.name);
    put_str(out, &clan.tag);
    put_str(out, &clan.motto);
    out.push(u8::try_from(members).unwrap_or(u8::MAX));
    put_len(out, ROLE_PRIVILEGES.len());
    for p in ROLE_PRIVILEGES {
        out.extend_from_slice(&p.to_le_bytes());
    }
    put_len(out, properties.len());
    for &p in properties {
        put_u32(out, p);
    }
    put_str(out, "");
}

/// A request's parameters.
struct Params<'a>(Cursor<&'a [u8]>);

impl<'a> Params<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self(Cursor::new(data))
    }

    fn u8(&mut self) -> Result<u8, Error> {
        self.0.read_u8().map_err(|_| Error::ParsingError)
    }

    fn u32(&mut self) -> Result<u32, Error> {
        self.0.read_u32::<LittleEndian>().map_err(|_| Error::ParsingError)
    }

    fn string(&mut self) -> Result<String, Error> {
        let len = usize::from(self.0.read_u16::<LittleEndian>().map_err(|_| Error::ParsingError)?);
        if len == 0 {
            return Ok(String::new());
        }
        let start = usize::try_from(self.0.position()).map_err(|_| Error::ParsingError)?;
        let bytes = self.0.get_ref().get(start..start + len).ok_or(Error::ParsingError)?;
        self.0.set_position((start + len) as u64);
        let text = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        String::from_utf8(text.to_vec()).map_err(|_| Error::ParsingError)
    }

    /// A String's bytes as they are, without its NUL (a room's session).
    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let len = usize::from(self.0.read_u16::<LittleEndian>().map_err(|_| Error::ParsingError)?);
        let start = usize::try_from(self.0.position()).map_err(|_| Error::ParsingError)?;
        let bytes = self.0.get_ref().get(start..start + len).ok_or(Error::ParsingError)?;
        self.0.set_position((start + len) as u64);
        Ok(bytes.strip_suffix(&[0]).unwrap_or(bytes).to_vec())
    }

    fn ids(&mut self) -> Result<Vec<u32>, Error> {
        let n = self.u32()?;
        if n > MAX_ASKED {
            return Err(Error::ParsingError);
        }
        (0..n).map(|_| self.u32()).collect()
    }
}

/// The most a room's session may take (the game's are well under this).
const MAX_SESSION_BYTES: usize = 1024;
/// Rooms a squad can have (the game shows four; it asks for up to ten).
const MAX_ROOMS: u8 = 16;

/// A squad room: who hosts it, its session as the game sent it, and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Room {
    host: u32,
    state: u8,
    session: Vec<u8>,
}

/// The squads' rooms, by squad and room number, and the room each player is in now (hosting or
/// joined), which goes out with the squad's members (the party list's "Squad Members in
/// Party"). Shared with NAT traversal, which lets a room's host be probed for its squadmates
/// (`nat.rs`).
#[derive(Default)]
pub struct Rooms(Mutex<HashMap<(u32, u8), Room>>, Mutex<HashMap<u32, u8>>);

impl Rooms {
    /// The room `pid` says it's in (0: none).
    fn joined(&self, pid: u32, room: u8) {
        let mut members = self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if room == 0 {
            members.remove(&pid);
        } else {
            members.insert(pid, room);
        }
    }

    /// The room `pid` is in (0: none).
    fn room_of(&self, pid: u32) -> u8 {
        self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&pid).copied().unwrap_or(0)
    }

    /// The squad whose room `host` hosts, if any.
    pub fn hosted_by(&self, host: u32) -> Option<u32> {
        self.lock().iter().find(|(_, r)| r.host == host).map(|((clan, _), _)| *clan)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(u32, u8), Room>> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `host` opens `room` of `clan` with `session`, leaving any other room it held; an empty
    /// session closes the host's room. Another host's room isn't taken over while it's listed.
    fn set(&self, clan: u32, room: u8, host: u32, state: u8, session: Vec<u8>, still_there: impl Fn(u32) -> bool) -> bool {
        let mut rooms = self.lock();
        if let Some(other) = rooms.get(&(clan, room)) {
            if other.host != host && !session.is_empty() && still_there(other.host) {
                return false;
            }
        }
        rooms.retain(|_, r| r.host != host);
        drop(rooms);
        if session.is_empty() {
            self.joined(host, 0);
        } else {
            self.lock().insert((clan, room), Room { host, state, session });
            self.joined(host, room);
        }
        true
    }

    /// The rooms of `clan` numbered `first` to `first + count - 1` whose host is still there,
    /// in room order; rooms whose host isn't are dropped.
    fn list(&self, clan: u32, first: u8, count: u8, still_there: impl Fn(u32) -> bool) -> Vec<(u8, Room)> {
        let mut rooms = self.lock();
        rooms.retain(|_, r| still_there(r.host));
        let last = u32::from(first) + u32::from(count);
        let mut listed: Vec<(u8, Room)> = rooms
            .iter()
            .filter(|((c, n), _)| *c == clan && u32::from(*n) >= u32::from(first) && u32::from(*n) < last)
            .map(|((_, n), r)| (*n, r.clone()))
            .collect();
        listed.sort_by_key(|(n, _)| *n);
        listed
    }
}

pub struct Clans {
    store: Arc<Store>,
    accounts: Arc<Accounts>,
    online: Arc<Online>,
    rooms: Arc<Rooms>,
}

impl Clans {
    pub fn new(store: Arc<Store>, accounts: Arc<Accounts>, online: Arc<Online>, rooms: Arc<Rooms>) -> Self {
        Self { store, accounts, online, rooms }
    }

    /// The squad room calls (15 to 18), logged; `None` for any other method.
    fn room_call(&self, logger: &Logger, pid: u32, method: u32, p: &mut Params) -> Option<Result<Vec<u8>, Error>> {
        let clan = || self.store.clan_of(pid).map_err(|_| Error::InternalError);
        let still_there = |clan: u32| move |host: u32| self.online.is_online(host) && self.store.clan_of(host).is_ok_and(|c| c == clan);
        let answer = match method {
            15 => (|| {
                let (asked, session, room, state, other) = (p.u32()?, p.bytes()?, p.u8()?, p.u8()?, p.u8()?);
                let mine = clan()?;
                let mut out = Vec::new();
                if mine == 0 || asked != mine || room >= MAX_ROOMS || session.len() > MAX_SESSION_BYTES {
                    info!(logger, "{pid:#x} can't set room {room} of squad {asked} (in squad {mine})");
                    put_u32(&mut out, result::NOT_ALLOWED);
                } else if self.rooms.set(mine, room, pid, state, session.clone(), still_there(mine)) {
                    if session.is_empty() {
                        info!(logger, "{pid:#x} left room {room} of squad {mine} (state {state}, {other})");
                    } else {
                        info!(
                            logger,
                            "{pid:#x} hosts room {room} of squad {mine} (state {state}, {other}; session {} bytes)",
                            session.len()
                        );
                    }
                    put_u32(&mut out, result::OK);
                } else {
                    info!(logger, "{pid:#x} can't take room {room} of squad {mine}: another member hosts it");
                    put_u32(&mut out, result::NOT_ALLOWED);
                }
                Ok(out)
            })(),
            16 => (|| {
                let (first, count) = (p.u8()?, p.u8()?);
                let mine = clan()?;
                let listed = if mine == 0 {
                    Vec::new()
                } else {
                    self.rooms.list(mine, first, count, still_there(mine))
                };
                debug!(logger, "{pid:#x} lists rooms {first}+{count} of squad {mine}: {}", listed.len());
                let mut out = Vec::new();
                put_len(&mut out, listed.len());
                for (n, room) in &listed {
                    out.push(*n);
                    out.push(room.state);
                    put_bytes(&mut out, &room.session);
                }
                Ok(out)
            })(),
            17 => p.u8().map(|room| {
                info!(logger, "{pid:#x} joins squad room {room}");
                self.rooms.joined(pid, room);
                Vec::new()
            }),
            18 => (|| {
                let (a, b, c) = (p.u32()?, p.u8()?, p.u8()?);
                info!(logger, "{pid:#x} sets squad room state ({a}, {b}, {c})");
                Ok(Vec::new())
            })(),
            _ => return None,
        };
        Some(answer)
    }

    fn name(&self, pid: u32) -> String {
        self.accounts.name(pid).ok().flatten().unwrap_or_default()
    }

    /// Answers one call; `Ok(None)` for a method with no answer.
    #[allow(clippy::too_many_lines)]
    fn answer(&self, pid: u32, method: u32, p: &mut Params) -> Result<Option<Vec<u8>>, rusqlite::Error> {
        let mut out = Vec::new();
        let bad = |_: Error| rusqlite::Error::InvalidQuery;
        match method {
            1 => {
                let (name, tag, motto) = (p.string().map_err(bad)?, p.string().map_err(bad)?, p.string().map_err(bad)?);
                let (id, r) = self.store.create(pid, &name, &tag, &motto)?;
                put_u32(&mut out, id);
                put_u32(&mut out, r);
            }
            2 => {
                let (tag, motto, mask) = (p.string().map_err(bad)?, p.string().map_err(bad)?, p.u8().map_err(bad)?);
                put_u32(&mut out, self.store.update(pid, &tag, &motto, mask)?);
            }
            // One entry per id asked, in order, each with that id: the game files each in its
            // cache by id without checking (an entry it didn't ask for corrupts its memory), and
            // waits for every id it asked for. A squad that no longer exists comes back blank.
            3 => {
                let ids = p.ids().map_err(bad)?;
                put_len(&mut out, ids.len());
                for id in ids {
                    match self.store.clan(id)? {
                        Some(clan) => clan_info(&mut out, &clan, self.store.member_count(id)?, &self.store.properties(id)?),
                        None => {
                            let blank = Clan {
                                id,
                                name: String::new(),
                                tag: String::new(),
                                motto: String::new(),
                            };
                            clan_info(&mut out, &blank, 0, &[]);
                        }
                    }
                }
            }
            4 => put_u32(&mut out, self.store.delete(pid)?),
            5 => put_u32(&mut out, self.store.invite(pid, p.u32().map_err(bad)?)?),
            6 => {
                let name = p.string().map_err(bad)?;
                let r = match self.accounts.pid(&name).ok().flatten() {
                    Some(invitee) => self.store.invite(pid, invitee)?,
                    None => result::FAILED,
                };
                put_u32(&mut out, r);
            }
            7 => {
                let (who, clan) = (p.u32().map_err(bad)?, p.u32().map_err(bad)?);
                put_u32(&mut out, self.store.remove_invite(pid, who, clan)?);
            }
            8 => put_u32(&mut out, self.store.join(pid, p.u32().map_err(bad)?)?),
            9 => put_u32(&mut out, self.store.leave(pid, p.u32().map_err(bad)?)?),
            11 => {
                let (who, role) = (p.u32().map_err(bad)?, p.u32().map_err(bad)?);
                put_u32(&mut out, self.store.change_role(pid, who, u8::try_from(role).unwrap_or(u8::MAX))?);
            }
            // Role privileges are fixed (see ROLE_PRIVILEGES).
            12 => put_u32(&mut out, result::NOT_ALLOWED),
            13 => put_u32(&mut out, self.store.clan_of(pid)?),
            14 => {
                let members = self.store.members(p.u32().map_err(bad)?)?;
                put_len(&mut out, members.len());
                for m in &members {
                    put_u32(&mut out, m.pid);
                    out.push(m.role);
                    put_time(&mut out, m.joined);
                    put_time(&mut out, m.joined);
                    put_str(&mut out, &self.name(m.pid));
                    // The room the member is in now, while online.
                    out.push(if self.online.is_online(m.pid) { self.rooms.room_of(m.pid) } else { 0 });
                }
            }
            19 => {
                put_len(&mut out, 0);
                put_u32(&mut out, 0);
                put_u32(&mut out, 0);
            }
            20 => {
                let clan = p.u32().map_err(bad)?;
                let n = p.u32().map_err(bad)?;
                if n > MAX_CHANGES {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let changes = (0..n).map(|_| Ok((p.u32()?, p.u8()?, p.u8()?))).collect::<Result<Vec<_>, Error>>().map_err(bad)?;
                self.store.change_properties(pid, clan, &changes)?;
            }
            25 => {}
            21 => put_u32(&mut out, self.store.remove_event(pid, p.u32().map_err(bad)?)?),
            22 => {
                let id = p.u32().map_err(bad)?;
                let _clan = p.u32().map_err(bad)?;
                let _author = p.u32().map_err(bad)?;
                let text = p.string().map_err(bad)?;
                let kind = p.u8().map_err(bad)?;
                put_u32(&mut out, self.store.set_event(pid, id, &text, kind)?);
            }
            23 => {
                let (clan, kind) = (p.u32().map_err(bad)?, p.u8().map_err(bad)?);
                let events = self.store.events(pid, clan, kind)?;
                put_len(&mut out, events.len());
                for e in &events {
                    put_u32(&mut out, e.id);
                    put_u32(&mut out, e.clan);
                    put_u32(&mut out, e.author);
                    put_str(&mut out, &e.text);
                    out.push(e.kind);
                    put_time(&mut out, e.at);
                    put_time(&mut out, e.at);
                }
            }
            24 => {
                let (id, kind) = (p.u32().map_err(bad)?, p.u8().map_err(bad)?);
                put_u32(&mut out, self.store.change_event_type(pid, id, kind)?);
            }
            26 => {
                let pids = p.ids().map_err(bad)?;
                put_len(&mut out, pids.len());
                for who in pids {
                    put_u32(&mut out, self.store.clan_of(who)?);
                }
            }
            27 => {
                let ids = p.ids().map_err(bad)?;
                put_len(&mut out, ids.len());
                for id in ids {
                    let tag = self.store.clan(id)?.map(|c| c.tag).unwrap_or_default();
                    put_str(&mut out, &tag);
                }
            }
            28 => {
                let clan = p.u32().map_err(bad)?;
                let invites = self.store.invites(pid, clan)?;
                put_len(&mut out, invites.len());
                for i in &invites {
                    put_u32(&mut out, i.invitee);
                    put_u32(&mut out, i.clan);
                    put_u32(&mut out, i.inviter);
                    put_time(&mut out, i.at);
                    // The other player: who invited me, or whom my squad invited.
                    put_str(&mut out, &self.name(if clan == 0 { i.inviter } else { i.invitee }));
                }
                put_u32(&mut out, result::OK);
            }
            _ => return Ok(None),
        }
        Ok(Some(out))
    }
}

const NAMES: &[(u32, &str)] = &[
    (1, "CreateClan"),
    (2, "UpdateClan"),
    (3, "GetClanList"),
    (4, "DeleteClan"),
    (5, "Invite"),
    (6, "InviteByName"),
    (7, "RemoveInvite"),
    (8, "Join"),
    (9, "Leave"),
    (11, "ChangeRole"),
    (12, "ChangePrivilege"),
    (13, "GetCurUserClan"),
    (14, "GetMembers"),
    (15, "SetRoom"),
    (16, "GetRooms"),
    (17, "JoinRoom"),
    (18, "SetRoomState"),
    (19, "GetLeaderboard"),
    (20, "SetProperties"),
    (21, "RemoveEvent"),
    (22, "SetEvent"),
    (23, "GetEvents"),
    (24, "ChangeEventType"),
    (25, "SendMessage"),
    (26, "GetClanIds"),
    (27, "GetClanTags"),
    (28, "GetInvitesWithNames"),
];

impl<CI> Protocol<CI> for Clans {
    fn id(&self) -> u16 {
        110
    }

    fn name(&self) -> String {
        "ClansProtocol".into()
    }

    fn num_methods(&self) -> u32 {
        33
    }

    fn handle(
        &self,
        logger: &Logger,
        _ctx: &Context,
        ci: &mut ClientInfo<CI>,
        request: &Request,
        _client_registry: &ClientRegistry<CI>,
        _socket: &std::net::UdpSocket,
    ) -> Result<Vec<u8>, Error> {
        let pid = crate::ticket::player(ci)?;
        let mut params = Params::new(&request.parameters);
        if let Some(answer) = self.room_call(logger, pid, request.method_id, &mut params) {
            return answer;
        }
        match self.answer(pid, request.method_id, &mut params) {
            Ok(Some(reply)) => {
                debug!(logger, "Clans.{}: {} bytes", request.method_id, reply.len());
                if matches!(request.method_id, 1 | 5 | 6 | 7 | 8 | 9 | 11) {
                    info!(
                        logger,
                        "{pid:#x} Clans.{}: result {}",
                        request.method_id,
                        reply.get(reply.len().saturating_sub(4)..).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    );
                }
                Ok(reply)
            }
            Ok(None) => Err(Error::UnknownMethod),
            Err(rusqlite::Error::InvalidQuery) => Err(Error::ParsingError),
            Err(_) => Err(Error::InternalError),
        }
    }

    fn method_name(&self, method_id: u32) -> Option<String> {
        NAMES.iter().find(|(id, _)| *id == method_id).map(|(_, n)| (*n).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host opens a room; squadmates list it with its session as it came; moving to another
    /// room, closing it, going offline or leaving the squad each take it off the list.
    #[test]
    fn squad_rooms_list_their_hosts_sessions() {
        let rooms = Rooms::default();
        let there = |_: u32| true;
        assert!(rooms.set(7, 0, 0x100001, 1, b"session-a".to_vec(), there));
        assert!(rooms.set(7, 2, 0x100003, 0, b"session-c".to_vec(), there));
        assert!(rooms.set(8, 0, 0x100009, 1, b"other squad".to_vec(), there));
        let listed = rooms.list(7, 0, 10, there);
        assert_eq!(
            listed.iter().map(|(n, r)| (*n, r.host, r.session.as_slice())).collect::<Vec<_>>(),
            vec![(0, 0x100001, b"session-a".as_slice()), (2, 0x100003, b"session-c".as_slice()),]
        );
        assert_eq!(rooms.list(7, 1, 1, there), vec![], "only the rooms asked for");
        assert_eq!(listed[0].1.state, 1);
        assert_eq!(rooms.hosted_by(0x100003), Some(7), "the host's squad, for its probes");
        assert_eq!(rooms.hosted_by(0x100004), None);
        assert_eq!(rooms.room_of(0x100003), 2, "a host is in its room");
        rooms.joined(0x100005, 2);
        assert_eq!(rooms.room_of(0x100005), 2, "and so is whoever joins it");
        rooms.joined(0x100005, 0);
        assert_eq!(rooms.room_of(0x100005), 0);

        // The host moves to room 1: room 0 is free again.
        assert!(rooms.set(7, 1, 0x100001, 1, b"session-a2".to_vec(), there));
        assert_eq!(rooms.list(7, 0, 10, there).iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![1, 2]);
        // An empty session closes the host's room.
        assert!(rooms.set(7, 1, 0x100001, 1, Vec::new(), there));
        assert_eq!(rooms.list(7, 0, 10, there).iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![2]);

        // Another member can't take a room whose host is still there, but can once it's gone.
        assert!(!rooms.set(7, 2, 0x100004, 0, b"session-d".to_vec(), there));
        let gone = |host: u32| host != 0x100003;
        assert!(rooms.set(7, 2, 0x100004, 0, b"session-d".to_vec(), gone));
        assert_eq!(rooms.list(7, 0, 10, gone)[0].1.host, 0x100004);

        // A host offline (or out of the squad) drops out of the list, and stays out.
        assert_eq!(rooms.list(7, 0, 10, |host| host != 0x100004), vec![]);
        assert_eq!(rooms.list(7, 0, 10, there), vec![]);
        assert_eq!(rooms.list(8, 0, 10, there).len(), 1, "other squads untouched");
    }

    /// GetRooms' answer: a list of {u8 room, u8 state, String session}, the session's bytes as
    /// they came with a NUL after them.
    #[test]
    fn a_room_goes_out_as_the_game_reads_it() {
        let mut out = Vec::new();
        put_len(&mut out, 1);
        out.push(3);
        out.push(1);
        put_bytes(&mut out, &[0xab, 0x00, 0xcd]);
        assert_eq!(out, [1, 0, 0, 0, 3, 1, 4, 0, 0xab, 0x00, 0xcd, 0]);
        let mut p = Params::new(&out[6..]);
        assert_eq!(p.bytes().unwrap(), vec![0xab, 0x00, 0xcd], "read back as sent");
    }

    /// Whatever a member sends, a squad's properties stay within what matches could bring.
    #[test]
    fn squad_properties_stay_within_bounds() {
        assert_eq!(changed_property(1, 5, 1, 0), Some(6), "a win");
        assert_eq!(changed_property(1, 5, 50, 0), None, "50 wins at once");
        assert_eq!(changed_property(1, 5, u32::MAX, 3), Some(5 + MAX_COUNT_ADDED), "keeping the larger, a few at a time");
        assert_eq!(changed_property(7, 0, u32::MAX, 4), Some(MAX_COUNT_ADDED), "the smaller of nothing");
        assert_eq!(changed_property(1, MAX_COUNT, 1, 0), Some(MAX_COUNT));
        assert_eq!(changed_property(4, 10, 2000, 0), Some(2010), "experience");
        assert_eq!(changed_property(4, MAX_VALUE - 1, 2000, 0), Some(MAX_VALUE));
        assert_eq!(changed_property(RATING, 0, 1500, 3), Some(1500));
        assert_eq!(changed_property(RATING, 1500, u32::MAX, 3), Some(RATING_RANGE.1));
        assert_eq!(changed_property(RATING, 1500, 0, 4), Some(RATING_RANGE.0));
        assert_eq!(changed_property(LEVEL, 3, 99, 3), Some(MAX_LEVEL));
        assert_eq!(changed_property(1, 5, 1, 1), None, "an unknown change");
    }

    #[test]
    fn a_squad_keeps_its_numbers_and_tells_its_size() {
        let s = Store::in_memory().unwrap();
        let (id, _) = s.create(1, "Night Owls", "OWL", "").unwrap();
        s.invite(1, 2).unwrap();
        s.join(2, id).unwrap();
        assert_eq!(s.member_count(id).unwrap(), 2);
        assert!(s.properties(id).unwrap().is_empty());
        s.change_properties(1, id, &[(5, 4, 0), (1, 1, 0), (1500, 0, 3)]).unwrap();
        s.change_properties(2, id, &[(7, 4, 0), (1200, 0, 4), (3, 20, 3), (9, 21, 0)]).unwrap();
        s.change_properties(3, id, &[(1000, 4, 0)]).unwrap(); // not a member
        s.change_properties(1, id, &[(1_000_000, 4, 0), (9, 4, 1), (99, 20, 3)]).unwrap(); // too much, unknown, out of range
        let props = s.properties(id).unwrap();
        assert_eq!(props.len(), 21);
        assert_eq!((props[0], props[1], props[4], props[20]), (1200, 1, 12, 20));

        let clan = s.clan(id).unwrap().unwrap();
        let mut out = Vec::new();
        clan_info(&mut out, &clan, 2, &props);
        let after_motto = 4 + 2 + 11 + 2 + 4 + 2;
        assert_eq!(out[after_motto], 2, "the member count follows the motto");

        s.leave(2, 2).unwrap();
        s.leave(1, 1).unwrap();
        assert!(s.properties(id).unwrap().is_empty(), "gone with the squad");
    }

    #[test]
    fn a_squad_is_made_joined_by_invitation_and_led() {
        let s = Store::in_memory().unwrap();
        let (id, r) = s.create(1, "Night Owls", "OWL", "we fly at night").unwrap();
        assert_eq!(r, result::OK);
        assert_eq!(s.clan_of(1).unwrap(), id);
        assert_eq!(s.create(2, "night owls", "N", "").unwrap().1, result::NAME_TAKEN);
        assert_eq!(s.create(1, "Other", "O", "").unwrap().1, result::ALREADY_IN_A_SQUAD);
        assert_eq!(s.create(2, "Too long a name for any squad at all", "X", "").unwrap().1, result::FAILED);
        assert_eq!(s.create(2, "Tags", "TOOLONG", "").unwrap().1, result::FAILED);

        assert_eq!(s.join(2, id).unwrap(), result::INVITATION_GONE, "not invited");
        assert_eq!(s.invite(1, 2).unwrap(), result::OK);
        assert_eq!(s.invite(1, 2).unwrap(), result::ALREADY_INVITED);
        assert_eq!(s.invites(2, 0).unwrap().len(), 1);
        assert_eq!(s.invites(1, id).unwrap().len(), 1, "the squad's sent invitations");
        assert!(s.invites(3, id).unwrap().is_empty(), "not for outsiders");
        assert_eq!(s.join(2, id).unwrap(), result::OK);
        assert!(s.invites(2, 0).unwrap().is_empty());
        assert_eq!(s.members(id).unwrap().iter().map(|m| (m.pid, m.role)).collect::<Vec<_>>(), vec![(1, LEADER), (2, RECRUIT)]);

        // A recruit can't invite or kick; the leader can promote.
        assert_eq!(s.invite(2, 3).unwrap(), result::NOT_ALLOWED);
        assert_eq!(s.leave(2, 1).unwrap(), result::NOT_ALLOWED);
        assert_eq!(s.change_role(1, 2, SERGEANT).unwrap(), result::OK);
        assert_eq!(s.invite(2, 3).unwrap(), result::OK);
        assert_eq!(s.remove_invite(3, 3, id).unwrap(), result::OK, "declined");

        // The leader can't leave while others remain; handing over the lead lets them.
        assert_eq!(s.leave(1, 1).unwrap(), result::NOT_ON_THE_LEADER);
        assert_eq!(s.change_role(1, 2, LEADER).unwrap(), result::OK);
        assert_eq!(s.leave(1, 1).unwrap(), result::OK);
        assert_eq!(s.clan_of(1).unwrap(), 0);
        // The last member leaving ends the squad.
        assert_eq!(s.leave(2, 2).unwrap(), result::OK);
        assert!(s.clan(id).unwrap().is_none());
    }

    #[test]
    fn the_message_of_the_day_is_one_event_replaced() {
        let s = Store::in_memory().unwrap();
        let (id, _) = s.create(1, "Owls", "OWL", "").unwrap();
        assert_eq!(s.set_event(1, 0, "first", 1).unwrap(), result::OK);
        let first = s.events(1, id, 1).unwrap()[0].id;
        assert_eq!(s.set_event(1, first, "second", 1).unwrap(), result::OK);
        let events = s.events(1, id, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].text, "second");
        assert!(s.events(2, id, 1).unwrap().is_empty(), "members only");
        assert_eq!(s.set_event(2, 0, "x", 1).unwrap(), result::NOT_A_MEMBER);
    }

    #[test]
    fn requests_are_read_as_the_game_sends_them() {
        // CreateClan as the game sent it.
        let sent = [
            0x0b, 0x00, b'N', b'i', b'g', b'h', b't', b' ', b'O', b'w', b'l', b's', 0, 0x04, 0x00, b'O', b'W', b'L', 0, 0x01, 0x00, 0,
        ];
        let mut p = Params::new(&sent);
        assert_eq!(p.string().unwrap(), "Night Owls");
        assert_eq!(p.string().unwrap(), "OWL");
        assert_eq!(p.string().unwrap(), "");
        let mut out = Vec::new();
        put_str(&mut out, "Night Owls");
        assert_eq!(&out[..], &sent[..13]);
    }
}
