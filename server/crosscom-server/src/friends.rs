//! Friends, friend requests and blocks, in the server's database.
//!
//! How the game shows a relationship (worked out from its friend list code): 1 a friend, 2 a
//! friend request, 3 a blocked player. A player sees their friends, the requests others sent
//! them (which they may accept or decline), and whom they blocked. A request someone sent is
//! not shown to them as a relationship, and nobody learns that they were blocked: a request to
//! someone who blocked you seems to go out, and is dropped.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::params;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

/// How the game numbers relationships.
pub const FRIEND: u8 = 1;
pub const REQUEST: u8 = 2;
pub const BLOCKED: u8 = 3;

/// Friends a player may have, requests they may have waiting for an answer, and the longest
/// message sent with a request.
const MAX_FRIENDS: i64 = 500;
const MAX_OPEN_REQUESTS: i64 = 100;
pub const MAX_MESSAGE_LEN: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Yourself,
    /// You blocked them: unblock them first.
    YouBlocked,
    TooMany,
    Db,
}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Error::Db
    }
}

/// What a friend request did.
#[derive(Debug, PartialEq, Eq)]
pub enum Requested {
    /// It waits for their answer (or, unknown to the sender, was dropped: they blocked them).
    Sent,
    /// They had asked too: now friends.
    Friends,
    /// Already friends, or already asked.
    Already,
    /// They blocked the sender: kept from them, and to the sender it looks as if it went out.
    Dropped,
}

pub struct Friends {
    db: Mutex<Connection>,
}

impl Friends {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::with(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> rusqlite::Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        // `state` is a relationship from `a`'s side: FRIEND (kept both ways), REQUEST (a asked
        // b) or BLOCKED (a blocked b).
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS relationships (
                a INTEGER NOT NULL,
                b INTEGER NOT NULL,
                state INTEGER NOT NULL,
                since INTEGER NOT NULL DEFAULT (unixepoch()),
                PRIMARY KEY (a, b)
            ) WITHOUT ROWID;
            CREATE INDEX IF NOT EXISTS relationships_b ON relationships (b, state);
            CREATE TABLE IF NOT EXISTS friend_messages (
                sender INTEGER NOT NULL,
                recipient INTEGER NOT NULL,
                body TEXT NOT NULL,
                PRIMARY KEY (sender, recipient)
            ) WITHOUT ROWID;",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn state(db: &Connection, a: u32, b: u32) -> Result<Option<u8>, Error> {
        Ok(db
            .query_row("SELECT state FROM relationships WHERE a = ?1 AND b = ?2", params![a, b], |r| r.get(0))
            .optional()?)
    }

    fn count(db: &Connection, a: u32, state: u8) -> Result<i64, Error> {
        Ok(db.query_row("SELECT COUNT(*) FROM relationships WHERE a = ?1 AND state = ?2", params![a, state], |r| r.get(0))?)
    }

    fn befriend(db: &Connection, a: u32, b: u32) -> Result<(), Error> {
        if Self::count(db, a, FRIEND)? >= MAX_FRIENDS || Self::count(db, b, FRIEND)? >= MAX_FRIENDS {
            return Err(Error::TooMany);
        }
        db.execute(
            "DELETE FROM friend_messages WHERE (sender = ?1 AND recipient = ?2) OR (sender = ?2 AND recipient = ?1)",
            params![a, b],
        )?;
        db.execute(
            "INSERT OR REPLACE INTO relationships (a, b, state) VALUES (?1, ?2, ?3), (?2, ?1, ?3)",
            params![a, b, FRIEND],
        )?;
        Ok(())
    }

    /// `from` asks `to` to be friends.
    pub fn request(&self, from: u32, to: u32) -> Result<Requested, Error> {
        if from == to {
            return Err(Error::Yourself);
        }
        let mut db = self.lock();
        let tx = db.transaction()?;
        let result = match (Self::state(&tx, from, to)?, Self::state(&tx, to, from)?) {
            (Some(BLOCKED), _) => Err(Error::YouBlocked),
            (Some(FRIEND | REQUEST), _) => Ok(Requested::Already),
            (_, Some(BLOCKED)) => Ok(Requested::Dropped),
            (_, Some(REQUEST)) => Self::befriend(&tx, from, to).map(|()| Requested::Friends),
            _ => {
                if Self::count(&tx, from, REQUEST)? >= MAX_OPEN_REQUESTS {
                    return Err(Error::TooMany);
                }
                tx.execute("INSERT INTO relationships (a, b, state) VALUES (?1, ?2, ?3)", params![from, to, REQUEST])?;
                Ok(Requested::Sent)
            }
        };
        tx.commit()?;
        result
    }

    /// `me` accepts `other`'s request; false if there was none.
    pub fn accept(&self, me: u32, other: u32) -> Result<bool, Error> {
        let mut db = self.lock();
        let tx = db.transaction()?;
        if Self::state(&tx, other, me)? != Some(REQUEST) {
            return Ok(false);
        }
        Self::befriend(&tx, me, other)?;
        tx.commit()?;
        Ok(true)
    }

    /// `me` declines `other`'s request; false if there was none.
    pub fn decline(&self, me: u32, other: u32) -> Result<bool, Error> {
        let db = self.lock();
        db.execute("DELETE FROM friend_messages WHERE sender = ?1 AND recipient = ?2", params![other, me])?;
        Ok(db.execute("DELETE FROM relationships WHERE a = ?1 AND b = ?2 AND state = ?3", params![other, me, REQUEST])? > 0)
    }

    /// `me` ends whatever they have with `other`: a friendship, a request either way, or a block.
    pub fn clear(&self, me: u32, other: u32) -> Result<bool, Error> {
        let db = self.lock();
        db.execute(
            "DELETE FROM friend_messages WHERE (sender = ?1 AND recipient = ?2) OR (sender = ?2 AND recipient = ?1)",
            params![me, other],
        )?;
        let mine = db.execute("DELETE FROM relationships WHERE a = ?1 AND b = ?2", params![me, other])?;
        let theirs = db.execute(
            "DELETE FROM relationships WHERE a = ?1 AND b = ?2 AND state IN (?3, ?4)",
            params![other, me, FRIEND, REQUEST],
        )?;
        Ok(mine + theirs > 0)
    }

    /// `me` blocks `other`, ending any friendship or request between them.
    pub fn block(&self, me: u32, other: u32) -> Result<(), Error> {
        if me == other {
            return Err(Error::Yourself);
        }
        self.clear(me, other)?;
        self.lock()
            .execute("INSERT OR REPLACE INTO relationships (a, b, state) VALUES (?1, ?2, ?3)", params![me, other, BLOCKED])?;
        Ok(())
    }

    /// `me` unblocks `other`; false if they weren't blocked.
    pub fn unblock(&self, me: u32, other: u32) -> Result<bool, Error> {
        Ok(self
            .lock()
            .execute("DELETE FROM relationships WHERE a = ?1 AND b = ?2 AND state = ?3", params![me, other, BLOCKED])?
            > 0)
    }

    pub fn are_friends(&self, a: u32, b: u32) -> bool {
        Self::state(&self.lock(), a, b).ok().flatten() == Some(FRIEND)
    }

    /// Whether `a` blocked `b`.
    pub fn blocked(&self, a: u32, b: u32) -> bool {
        Self::state(&self.lock(), a, b).ok().flatten() == Some(BLOCKED)
    }

    /// Everyone `me` has a relationship with, as the game shows them: friends, the requests
    /// others sent `me`, and whom `me` blocked. In the order they began.
    pub fn relationships(&self, me: u32) -> Result<Vec<(u32, u8)>, Error> {
        let db = self.lock();
        let mut query = db.prepare_cached(
            "SELECT b, state, since FROM relationships WHERE a = ?1 AND state IN (?2, ?3)
             UNION ALL SELECT a, state, since FROM relationships WHERE b = ?1 AND state = ?4
             ORDER BY since, 1",
        )?;
        let rows = query.query_map(params![me, FRIEND, BLOCKED, REQUEST], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One kind of relationship: from `me`'s side, or with `reversed`, others' towards `me`.
    /// Others' blocks are never told.
    pub fn list(&self, me: u32, relationship: u8, reversed: bool) -> Result<Vec<u32>, Error> {
        if reversed && relationship == BLOCKED {
            return Ok(vec![]);
        }
        let db = self.lock();
        let sql = if reversed {
            "SELECT a FROM relationships WHERE b = ?1 AND state = ?2 ORDER BY since"
        } else {
            "SELECT b FROM relationships WHERE a = ?1 AND state = ?2 ORDER BY since"
        };
        let mut query = db.prepare_cached(sql)?;
        let rows = query.query_map(params![me, relationship], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Keeps the message `sender` sent with their open request to `recipient` (the latest one);
    /// false when there's no such request.
    pub fn set_message(&self, sender: u32, recipient: u32, body: &str) -> Result<bool, Error> {
        let db = self.lock();
        if Self::state(&db, sender, recipient)? != Some(REQUEST) {
            return Ok(false);
        }
        let body: String = body.chars().take(MAX_MESSAGE_LEN).collect();
        db.execute(
            "INSERT OR REPLACE INTO friend_messages (sender, recipient, body) VALUES (?1, ?2, ?3)",
            params![sender, recipient, body],
        )?;
        Ok(true)
    }

    /// The messages sent to `recipient` with requests still open: (sender, message).
    pub fn messages(&self, recipient: u32) -> Result<Vec<(u32, String)>, Error> {
        let db = self.lock();
        let mut query = db.prepare_cached(
            "SELECT m.sender, m.body FROM friend_messages m JOIN relationships r ON r.a = m.sender AND r.b = m.recipient AND r.state = ?2
             WHERE m.recipient = ?1 ORDER BY m.sender",
        )?;
        let rows = query.query_map(params![recipient, REQUEST], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn remove_message(&self, recipient: u32, sender: u32) -> Result<(), Error> {
        self.lock()
            .execute("DELETE FROM friend_messages WHERE sender = ?1 AND recipient = ?2", params![sender, recipient])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_accepted_makes_friends_both_ways() {
        let f = Friends::in_memory().unwrap();
        assert_eq!(f.request(1, 2), Ok(Requested::Sent));
        assert_eq!(f.request(1, 2), Ok(Requested::Already));
        assert_eq!(f.relationships(2).unwrap(), vec![(1, REQUEST)], "2 sees 1's request");
        assert!(f.relationships(1).unwrap().is_empty(), "1 doesn't see its own request as a relationship");
        assert_eq!(f.list(1, REQUEST, false).unwrap(), vec![2]);
        assert_eq!(f.list(2, REQUEST, true).unwrap(), vec![1]);
        assert!(!f.accept(1, 2).unwrap(), "only the one asked can accept");
        assert!(f.accept(2, 1).unwrap());
        assert!(f.are_friends(1, 2) && f.are_friends(2, 1));
        assert_eq!(f.relationships(1).unwrap(), vec![(2, FRIEND)]);
        assert!(f.clear(1, 2).unwrap());
        assert!(!f.are_friends(2, 1));
    }

    #[test]
    fn asking_each_other_makes_friends_and_declining_ends_it() {
        let f = Friends::in_memory().unwrap();
        f.request(1, 2).unwrap();
        assert_eq!(f.request(2, 1), Ok(Requested::Friends));
        f.request(3, 4).unwrap();
        assert!(f.decline(4, 3).unwrap());
        assert!(f.relationships(4).unwrap().is_empty());
        assert_eq!(f.request(5, 5), Err(Error::Yourself));
    }

    #[test]
    fn a_block_is_never_told() {
        let f = Friends::in_memory().unwrap();
        f.request(1, 2).unwrap();
        f.accept(2, 1).unwrap();
        f.block(2, 1).unwrap();
        assert!(!f.are_friends(1, 2), "a block ends the friendship");
        assert_eq!(f.relationships(2).unwrap(), vec![(1, BLOCKED)]);
        assert!(f.relationships(1).unwrap().is_empty());
        assert_eq!(f.request(1, 2), Ok(Requested::Dropped), "the protocol answers it as sent");
        assert!(f.relationships(2).unwrap().iter().all(|&(_, r)| r == BLOCKED), "but isn't kept");
        assert!(f.list(1, BLOCKED, true).unwrap().is_empty());
        assert_eq!(f.request(2, 1), Err(Error::YouBlocked));
        assert!(f.unblock(2, 1).unwrap());
        assert_eq!(f.request(2, 1), Ok(Requested::Sent));
    }

    #[test]
    fn requests_and_friends_are_capped() {
        let f = Friends::in_memory().unwrap();
        for to in 0..MAX_OPEN_REQUESTS as u32 {
            f.request(1000, to).unwrap();
        }
        assert_eq!(f.request(1000, 999), Err(Error::TooMany));
    }

    #[test]
    fn messages_go_with_open_requests_only() {
        let f = Friends::in_memory().unwrap();
        assert!(!f.set_message(1, 2, "hi").unwrap(), "no request");
        f.request(1, 2).unwrap();
        assert!(f.set_message(1, 2, &"x".repeat(1000)).unwrap());
        let messages = f.messages(2).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].1.len(), MAX_MESSAGE_LEN);
        f.accept(2, 1).unwrap();
        assert!(f.messages(2).unwrap().is_empty(), "answered");
        f.request(3, 2).unwrap();
        f.set_message(3, 2, "hello").unwrap();
        f.remove_message(2, 3).unwrap();
        assert!(f.messages(2).unwrap().is_empty());
    }
}
