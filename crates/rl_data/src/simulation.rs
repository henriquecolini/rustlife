//! `simulation.ron`: simulation timing.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Fastest tick rate accepted: beyond this a tick is shorter than a
/// millisecond and float precision of the time step becomes a concern.
const MAX_TICK_RATE: u32 = 1000;

/// Timing shared by the server and every client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Simulation", deny_unknown_fields)]
pub struct SimulationConfig {
    /// Fixed simulation ticks per second.
    pub tick_rate: u32,
}

impl SimulationConfig {
    /// Length of one tick, seconds.
    #[must_use]
    pub fn tick_interval(&self) -> f32 {
        1.0 / self.tick_rate.max(1) as f32
    }

    pub(crate) fn validate(&self, check: &mut Validator) {
        if !(1..=MAX_TICK_RATE).contains(&self.tick_rate) {
            check.fail(
                "tick_rate",
                &format!("must be within 1..={MAX_TICK_RATE}, got {}", self.tick_rate),
            );
        }
    }
}
