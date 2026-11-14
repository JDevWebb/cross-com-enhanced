//! Who's online: the players signed in to the secure server, for the launcher's count
//! (`GET /api/info`).
//!
//! The secure server keeps its connections on its own thread; it calls in here as a player signs
//! in and as their last connection closes or times out (a game that stops without a goodbye
//! counts for up to a minute more). A player is one pid however many connections their game
//! holds. The game's shared telemetry account isn't a player and isn't counted.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::ticket::TRACKING_PID;

#[derive(Default)]
pub struct Online {
    pids: Mutex<HashSet<u32>>,
}

impl Online {
    pub fn signed_in(&self, pid: u32) {
        if pid != TRACKING_PID {
            self.pids().insert(pid);
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_players_once_and_not_the_telemetry_account() {
        let online = Online::default();
        online.signed_in(0x10_0000);
        online.signed_in(0x10_0000);
        online.signed_in(0x10_0001);
        online.signed_in(TRACKING_PID);
        assert_eq!(online.count(), 2);
        online.gone(0x10_0000);
        online.gone(0x10_0000);
        assert_eq!(online.count(), 1);
    }
}
