//! The client's commands the server has not answered yet.
//!
//! Each tick the client numbers its command ([`Predictor::issue`]), runs it
//! at once for its own player, and keeps it. When a snapshot says which
//! command the server ran last (its `ack`), the older ones are forgotten
//! ([`Predictor::acknowledge`]): the snapshot's state already includes
//! them. The rest are replayed on top of that state to get back to the
//! present. The latest few go in every command packet
//! ([`Predictor::latest`]), so a lost packet is covered by the next.

use std::collections::VecDeque;

use rl_sim::movement::UserCmd;

/// Commands kept at most: a few seconds' worth. Beyond that the server is
/// not answering and the oldest are dropped.
const MAX_PENDING: usize = 512;

/// See the [module docs](self).
#[derive(Debug, Clone, Default)]
pub struct Predictor {
    pending: VecDeque<UserCmd>,
    /// The number of the last command issued.
    last: u32,
}

impl Predictor {
    /// Numbers `cmd` (the next number after the last) and keeps it until
    /// the server answers it.
    pub fn issue(&mut self, mut cmd: UserCmd) -> UserCmd {
        self.last = self.last.wrapping_add(1);
        cmd.number = self.last;
        self.pending.push_back(cmd);
        while self.pending.len() > MAX_PENDING {
            self.pending.pop_front();
        }
        cmd
    }

    /// The server ran every command up to number `ack`: forgets them.
    pub fn acknowledge(&mut self, ack: u32) {
        while self.pending.front().is_some_and(|cmd| cmd.number <= ack) {
            self.pending.pop_front();
        }
    }

    /// The commands still unanswered, oldest first.
    pub fn pending(&self) -> impl Iterator<Item = &UserCmd> {
        self.pending.iter()
    }

    /// The latest `count` commands issued and unanswered, oldest first.
    #[must_use]
    pub fn latest(&self, count: usize) -> Vec<UserCmd> {
        let skip = self.pending.len().saturating_sub(count);
        self.pending.iter().skip(skip).copied().collect()
    }

    /// The number of the last command issued.
    #[must_use]
    pub fn last_number(&self) -> u32 {
        self.last
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_commands_and_forgets_the_answered() {
        let mut predictor = Predictor::default();
        let numbers: Vec<u32> = (0..5)
            .map(|_| predictor.issue(UserCmd::default()).number)
            .collect();
        assert_eq!(numbers, [1, 2, 3, 4, 5]);
        predictor.acknowledge(3);
        let left: Vec<u32> = predictor.pending().map(|cmd| cmd.number).collect();
        assert_eq!(left, [4, 5]);
        predictor.acknowledge(1);
        assert_eq!(predictor.pending().count(), 2, "an old ack changes nothing");
        assert_eq!(predictor.last_number(), 5);
    }

    #[test]
    fn packets_repeat_the_latest_commands() {
        let mut predictor = Predictor::default();
        for _ in 0..10 {
            predictor.issue(UserCmd::default());
        }
        let latest: Vec<u32> = predictor.latest(3).iter().map(|cmd| cmd.number).collect();
        assert_eq!(latest, [8, 9, 10]);
        assert_eq!(predictor.latest(50).len(), 10);
    }

    #[test]
    fn an_unanswering_server_does_not_grow_the_queue_forever() {
        let mut predictor = Predictor::default();
        for _ in 0..(MAX_PENDING + 100) {
            predictor.issue(UserCmd::default());
        }
        assert_eq!(predictor.pending().count(), MAX_PENDING);
    }
}
