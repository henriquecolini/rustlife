//! `net.ron`: how the server and its clients talk.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Networking tuning, shared by the server and its clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Net", deny_unknown_fields)]
pub struct NetConfig {
    /// UDP port a server listens on by default.
    pub port: u16,
    /// Most players a server admits.
    pub max_players: u32,
    /// Ticks between snapshots sent to each client.
    pub snapshot_interval: u32,
    /// How far in the past clients draw everything but themselves,
    /// seconds.
    pub interpolation_delay: f32,
    /// Latest commands repeated in each command packet.
    pub command_redundancy: u32,
    /// Most ticks of a player's commands the server runs at once.
    pub command_backlog: u32,
    /// How far back hit detection may look, seconds.
    pub max_rewind: f32,
    /// Seconds of silence after which a client is dropped.
    pub timeout: f32,
}

impl NetConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        if self.port == 0 {
            check.fail("port", "must not be 0");
        }
        if self.max_players == 0 {
            check.fail("max_players", "must be at least 1");
        }
        if !(1..=60).contains(&self.snapshot_interval) {
            check.fail(
                "snapshot_interval",
                &format!("must be within 1..=60, got {}", self.snapshot_interval),
            );
        }
        check.range("interpolation_delay", self.interpolation_delay, 0.0, 1.0);
        if !(1..=32).contains(&self.command_redundancy) {
            check.fail(
                "command_redundancy",
                &format!("must be within 1..=32, got {}", self.command_redundancy),
            );
        }
        if self.command_backlog == 0 {
            check.fail("command_backlog", "must be at least 1");
        }
        check.range("max_rewind", self.max_rewind, 0.0, 5.0);
        check.positive("timeout", self.timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> NetConfig {
        NetConfig {
            port: 27015,
            max_players: 16,
            snapshot_interval: 1,
            interpolation_delay: 0.1,
            command_redundancy: 4,
            command_backlog: 8,
            max_rewind: 1.0,
            timeout: 10.0,
        }
    }

    #[test]
    fn sensible_values_pass() {
        let mut check = Validator::default();
        config().validate(&mut check);
        assert!(check.finish().is_empty());
    }

    #[test]
    fn every_bad_value_is_reported() {
        let mut check = Validator::default();
        NetConfig {
            port: 0,
            max_players: 0,
            snapshot_interval: 0,
            interpolation_delay: -1.0,
            command_redundancy: 0,
            command_backlog: 0,
            max_rewind: 10.0,
            timeout: 0.0,
        }
        .validate(&mut check);
        assert_eq!(check.finish().len(), 8);
    }
}
