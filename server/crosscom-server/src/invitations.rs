//! Invitations into game sessions (GameSession methods 12-19), kept in memory: a party's host
//! inviting friends, a friend asked into a custom match.
//!
//! A player may invite only friends, into a session they joined. An invitation lasts
//! `LIFETIME`, or until its session ends; the one invited is told at once (see `notify`), and
//! the game then joins the session itself.

use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use crate::sessions::Key;

/// How long an invitation waits for an answer.
const LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Invitations one player may have out, one player may have waiting, and in all. Past each, a new
/// invitation takes the place of the oldest one (the sender's, the recipient's, or anyone's):
/// refusing new ones once a few hundred accounts had filled the table stopped everyone inviting.
const MAX_PER_SENDER: usize = 50;
const MAX_PER_RECIPIENT: usize = 50;
const MAX_INVITATIONS: usize = 10_000;
pub const MAX_MESSAGE_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    pub key: Key,
    pub sender: u32,
    pub recipient: u32,
    pub message: String,
    /// When it was sent, in Quazal's packed date-time.
    pub created: u64,
    at: Instant,
}

#[derive(Default)]
pub struct Invitations {
    inner: Mutex<Vec<Invitation>>,
}

impl Invitations {
    fn lock(&self, now: Instant) -> std::sync::MutexGuard<'_, Vec<Invitation>> {
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.retain(|i| now.duration_since(i.at) < LIFETIME);
        inner
    }

    /// Invites `recipient` into `key` (replacing an earlier invitation from the same sender), in
    /// place of the oldest invitation past a limit (see [`MAX_PER_SENDER`]).
    pub fn send(&self, key: Key, sender: u32, recipient: u32, message: &str, created: u64, now: Instant) {
        let mut inner = self.lock(now);
        inner.retain(|i| !(i.key == key && i.sender == sender && i.recipient == recipient));
        // Oldest first: they're kept in the order sent.
        make_room(&mut inner, MAX_PER_SENDER, |i| i.sender == sender);
        make_room(&mut inner, MAX_PER_RECIPIENT, |i| i.recipient == recipient);
        make_room(&mut inner, MAX_INVITATIONS, |_| true);
        inner.push(Invitation {
            key,
            sender,
            recipient,
            message: message.chars().take(MAX_MESSAGE_LEN).collect(),
            created,
            at: now,
        });
    }

    /// Invitations `recipient` has of a session type, still open (`live` says whether their session
    /// still is).
    pub fn received(&self, recipient: u32, type_id: u32, live: impl Fn(Key) -> bool, now: Instant) -> Vec<Invitation> {
        self.lock(now)
            .iter()
            .filter(|i| i.recipient == recipient && i.key.0 == type_id && live(i.key))
            .cloned()
            .collect()
    }

    pub fn sent(&self, sender: u32, type_id: u32, live: impl Fn(Key) -> bool, now: Instant) -> Vec<Invitation> {
        self.lock(now).iter().filter(|i| i.sender == sender && i.key.0 == type_id && live(i.key)).cloned().collect()
    }

    /// Takes away the invitation from `sender` to `recipient` into `key`; whether there was one.
    pub fn remove(&self, key: Key, sender: u32, recipient: u32, now: Instant) -> bool {
        let mut inner = self.lock(now);
        let before = inner.len();
        inner.retain(|i| !(i.key == key && i.sender == sender && i.recipient == recipient));
        inner.len() < before
    }
}

/// Takes out the oldest of the invitations `counted` picks while there are `max` or more of them.
fn make_room(invitations: &mut Vec<Invitation>, max: usize, counted: impl Fn(&Invitation) -> bool) {
    let mut over = (invitations.iter().filter(|i| counted(i)).count() + 1).saturating_sub(max);
    invitations.retain(|i| {
        if over > 0 && counted(i) {
            over -= 1;
            return false;
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitations_are_seen_by_both_sides_until_answered_or_old() {
        let invitations = Invitations::default();
        let now = Instant::now();
        let key = (1, 77);
        invitations.send(key, 10, 20, "join us", 5, now);
        invitations.send(key, 10, 20, "join us now", 6, now);
        let live = |_| true;
        let got = invitations.received(20, 1, live, now);
        assert_eq!(got.len(), 1, "a second invitation replaces the first");
        assert_eq!(got[0].message, "join us now");
        assert_eq!(invitations.sent(10, 1, live, now).len(), 1);
        assert!(invitations.received(20, 2, live, now).is_empty(), "another session type");
        assert!(invitations.received(20, 1, |_| false, now).is_empty(), "the session ended");
        assert!(invitations.remove(key, 10, 20, now));
        assert!(!invitations.remove(key, 10, 20, now));
        invitations.send(key, 10, 21, "", 7, now);
        assert!(invitations.received(21, 1, live, now + LIFETIME).is_empty(), "too old");
    }

    /// Past a player's limit, their oldest invitation goes; others' stay.
    #[test]
    fn a_player_can_only_have_so_many_out_or_waiting() {
        let invitations = Invitations::default();
        let now = Instant::now();
        let live = |_| true;
        for r in 0..=MAX_PER_SENDER as u32 {
            invitations.send((1, 1), 10, r, "", 0, now);
        }
        let sent = invitations.sent(10, 1, live, now);
        assert_eq!(sent.len(), MAX_PER_SENDER);
        assert!(sent.iter().all(|i| i.recipient != 0), "the oldest went");
        invitations.send((1, 1), 11, 999, "", 0, now);
        assert_eq!(invitations.sent(11, 1, live, now).len(), 1);
        for s in 0..=MAX_PER_RECIPIENT as u32 {
            invitations.send((1, 1), 100 + s, 7, "", 0, now);
        }
        let waiting = invitations.received(7, 1, live, now);
        assert_eq!(waiting.len(), MAX_PER_RECIPIENT);
        assert!(waiting.iter().all(|i| i.sender != 100));
    }

    /// Full, a new invitation takes the place of the oldest of all.
    #[test]
    fn a_full_table_makes_room_for_a_new_invitation() {
        let invitations = Invitations::default();
        let now = Instant::now();
        // Straight into the table (sent one by one, this took seconds).
        invitations.inner.lock().unwrap().extend((0..MAX_INVITATIONS as u32).map(|n| Invitation {
            key: (1, 1),
            sender: n / 40,
            recipient: n,
            message: String::new(),
            created: 0,
            at: now,
        }));
        assert_eq!(invitations.received(0, 1, |_| true, now).len(), 1);
        invitations.send((1, 1), 999_999, 999_998, "", 0, now);
        assert_eq!(invitations.received(999_998, 1, |_| true, now).len(), 1);
        assert!(invitations.received(0, 1, |_| true, now).is_empty(), "the oldest went");
        assert_eq!(invitations.received(1, 1, |_| true, now).len(), 1, "only the oldest");
    }
}
