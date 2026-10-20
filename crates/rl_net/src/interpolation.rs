//! The world states a client received, and drawing between them.
//!
//! [`StateBuffer`] keeps the last second or so of world states, oldest
//! first. They serve twice: as baselines the next snapshots may be sent
//! against, and as keyframes for drawing everything but the client's own
//! player. [`StateBuffer::sample`] places each player, prop and projectile
//! at a render tick between two states — positions linearly, orientations
//! along the shortest turn — exactly as the server's lag compensation
//! assumes the client drew them.
//!
//! A player who died, respawned or otherwise jumped across the map between
//! the two states is shown where the newer state has them rather than
//! sliding there. Past the newest state, everything holds at it.

use std::collections::{BTreeMap, VecDeque};

use glam::Vec3;
use rl_sim::physics::{BodyId, BodyPose};
use rl_sim::simulation::{Life, PlayerId};
use rl_sim::weapons::projectile::Projectile;

use crate::snapshot::WorldState;

/// A player moving farther than this between two states teleported, units.
const TELEPORT_DISTANCE: f32 = 256.0;

/// Received world states, oldest first.
#[derive(Debug, Clone)]
pub struct StateBuffer {
    states: VecDeque<WorldState>,
    capacity: usize,
}

/// Where a player is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerPose {
    /// Their feet, Source space.
    pub feet: Vec3,
    /// View pitch, degrees.
    pub pitch: f32,
    /// View yaw, degrees.
    pub yaw: f32,
    /// Crouch progress, `0` standing to `1` down.
    pub duck_amount: f32,
    /// Whether they are alive.
    pub alive: bool,
}

/// Everything but the client's own player, at a render tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Interpolated {
    /// The render tick.
    pub tick: f64,
    /// Players.
    pub players: BTreeMap<PlayerId, PlayerPose>,
    /// Props that can move.
    pub props: BTreeMap<BodyId, BodyPose>,
    /// Projectiles in flight, placed along their flight.
    pub projectiles: Vec<Projectile>,
}

impl StateBuffer {
    /// A buffer keeping at most `capacity` states.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            states: VecDeque::with_capacity(capacity),
            capacity: capacity.max(2),
        }
    }

    /// Keeps `state`, in tick order; a state already kept, or older than
    /// everything kept in a full buffer, is ignored. Returns whether it was
    /// kept.
    pub fn insert(&mut self, state: WorldState) -> bool {
        let at = self.states.partition_point(|kept| kept.tick < state.tick);
        if self
            .states
            .get(at)
            .is_some_and(|kept| kept.tick == state.tick)
            || (at == 0 && self.states.len() >= self.capacity)
        {
            return false;
        }
        self.states.insert(at, state);
        while self.states.len() > self.capacity {
            self.states.pop_front();
        }
        true
    }

    /// The state of `tick`, if kept.
    #[must_use]
    pub fn get(&self, tick: u64) -> Option<&WorldState> {
        let at = self.states.partition_point(|kept| kept.tick < tick);
        self.states.get(at).filter(|kept| kept.tick == tick)
    }

    /// The newest state.
    #[must_use]
    pub fn latest(&self) -> Option<&WorldState> {
        self.states.back()
    }

    /// Forgets every state.
    pub fn clear(&mut self) {
        self.states.clear();
    }

    /// Everything at `tick`, between the states either side of it.
    #[must_use]
    pub fn sample(&self, tick: f64) -> Option<Interpolated> {
        let newest = self.states.back()?;
        let after = self
            .states
            .iter()
            .position(|state| state.tick as f64 >= tick)
            .unwrap_or(self.states.len() - 1);
        let to = &self.states[after];
        let from = after
            .checked_sub(1)
            .map_or(to, |before| &self.states[before]);
        let span = to.tick.saturating_sub(from.tick) as f64;
        let alpha = if span > 0.0 {
            ((tick - from.tick as f64) / span).clamp(0.0, 1.0) as f32
        } else {
            1.0
        };
        let (from, to) = if tick >= newest.tick as f64 {
            (newest, newest)
        } else {
            (from, to)
        };
        Some(Interpolated {
            tick,
            players: to
                .players
                .iter()
                .map(|(&id, view)| {
                    let now = pose(&view.motion.state, view.motion.look, view.life);
                    let then = from
                        .players
                        .get(&id)
                        .map(|old| pose(&old.motion.state, old.motion.look, old.life));
                    let pose = match then {
                        Some(then)
                            if then.alive == now.alive
                                && then.feet.distance(now.feet) < TELEPORT_DISTANCE =>
                        {
                            blend_player(&then, &now, alpha)
                        }
                        _ => now,
                    };
                    (id, pose)
                })
                .collect(),
            props: to
                .props
                .iter()
                .map(|(&body, &now)| {
                    let pose = from.props.get(&body).map_or(now, |then| BodyPose {
                        position: then.position.lerp(now.position, alpha),
                        rotation: then.rotation.slerp(now.rotation, alpha),
                    });
                    (body, pose)
                })
                .collect(),
            projectiles: to
                .projectiles
                .iter()
                .map(|now| {
                    let mut projectile = now.clone();
                    if let Some(then) = from.projectiles.iter().find(|then| then.id == now.id) {
                        projectile.position = then.position.lerp(now.position, alpha);
                    }
                    projectile
                })
                .collect(),
        })
    }
}

fn pose(state: &rl_sim::movement::PlayerState, look: (f32, f32), life: Life) -> PlayerPose {
    PlayerPose {
        feet: state.origin,
        pitch: look.0,
        yaw: look.1,
        duck_amount: state.duck_amount,
        alive: life == Life::Alive,
    }
}

fn blend_player(from: &PlayerPose, to: &PlayerPose, alpha: f32) -> PlayerPose {
    PlayerPose {
        feet: from.feet.lerp(to.feet, alpha),
        pitch: from.pitch + (to.pitch - from.pitch) * alpha,
        yaw: lerp_degrees(from.yaw, to.yaw, alpha),
        duck_amount: from.duck_amount + (to.duck_amount - from.duck_amount) * alpha,
        alive: to.alive,
    }
}

/// Turns from angle `from` towards `to` the short way round, degrees.
fn lerp_degrees(from: f32, to: f32, alpha: f32) -> f32 {
    let turn = (to - from + 180.0).rem_euclid(360.0) - 180.0;
    (from + turn * alpha).rem_euclid(360.0)
}

#[cfg(test)]
mod tests {
    use glam::Quat;
    use rl_sim::movement::PlayerState;
    use rl_sim::rules::Match;

    use super::*;
    use crate::snapshot::{Motion, PlayerView};

    fn state(tick: u64, x: f32, yaw: f32, life: Life) -> WorldState {
        let view = PlayerView {
            motion: Motion {
                state: PlayerState::at(Vec3::new(x, 0.0, 0.0)),
                look: (0.0, yaw),
                weapon: None,
            },
            life,
            private: None,
        };
        WorldState {
            tick,
            players: BTreeMap::from([(PlayerId::from_index(0), view)]),
            props: BTreeMap::from([(
                BodyId::from_index(3),
                BodyPose {
                    position: Vec3::new(0.0, x, 0.0),
                    rotation: Quat::IDENTITY,
                },
            )]),
            pickups: Vec::new(),
            projectiles: Vec::new(),
            game: Match::new(),
        }
    }

    fn buffer() -> StateBuffer {
        let mut buffer = StateBuffer::new(8);
        for tick in [10, 12, 14] {
            buffer.insert(state(tick, tick as f32 * 10.0, 0.0, Life::Alive));
        }
        buffer
    }

    fn feet_x(buffer: &StateBuffer, tick: f64) -> f32 {
        buffer.sample(tick).expect("states").players[&PlayerId::from_index(0)]
            .feet
            .x
    }

    #[test]
    fn keeps_states_in_order_without_duplicates() {
        let mut buffer = StateBuffer::new(3);
        assert!(buffer.insert(state(5, 0.0, 0.0, Life::Alive)));
        assert!(buffer.insert(state(3, 0.0, 0.0, Life::Alive)));
        assert!(!buffer.insert(state(5, 0.0, 0.0, Life::Alive)));
        assert!(buffer.insert(state(7, 0.0, 0.0, Life::Alive)));
        assert!(!buffer.insert(state(1, 0.0, 0.0, Life::Alive)), "too old");
        assert!(buffer.insert(state(9, 0.0, 0.0, Life::Alive)));
        assert!(buffer.get(3).is_none(), "the oldest went");
        assert_eq!(buffer.get(7).map(|state| state.tick), Some(7));
        assert_eq!(buffer.latest().map(|state| state.tick), Some(9));
    }

    #[test]
    fn places_players_and_props_between_states() {
        let buffer = buffer();
        assert!((feet_x(&buffer, 11.0) - 110.0).abs() < 1e-3);
        assert!((feet_x(&buffer, 13.5) - 135.0).abs() < 1e-3);
        let sample = buffer.sample(13.0).expect("states");
        let prop = sample.props[&BodyId::from_index(3)];
        assert!((prop.position.y - 130.0).abs() < 1e-3);
    }

    #[test]
    fn holds_outside_the_states() {
        let buffer = buffer();
        assert!((feet_x(&buffer, 5.0) - 100.0).abs() < 1e-3);
        assert!((feet_x(&buffer, 20.0) - 140.0).abs() < 1e-3);
        assert!(StateBuffer::new(4).sample(1.0).is_none());
    }

    #[test]
    fn motion_is_monotonic_along_the_render_ticks() {
        let buffer = buffer();
        let mut last = f32::MIN;
        for step in 0..=60 {
            let x = feet_x(&buffer, 9.0 + f64::from(step) * 0.1);
            assert!(x >= last, "{x} < {last}");
            last = x;
        }
    }

    #[test]
    fn respawns_and_teleports_are_not_slid_across() {
        let mut buffer = StateBuffer::new(4);
        buffer.insert(state(1, 0.0, 0.0, Life::Alive));
        buffer.insert(state(2, 5000.0, 0.0, Life::Alive));
        assert!((feet_x(&buffer, 1.5) - 5000.0).abs() < 1e-3);
        let mut buffer = StateBuffer::new(4);
        buffer.insert(state(
            1,
            0.0,
            0.0,
            Life::Dead {
                since: 0,
                killer: None,
            },
        ));
        buffer.insert(state(2, 10.0, 0.0, Life::Alive));
        assert!((feet_x(&buffer, 1.5) - 10.0).abs() < 1e-3);
    }

    #[test]
    fn yaw_turns_the_short_way() {
        assert!((lerp_degrees(350.0, 10.0, 0.5) - 0.0).abs() < 1e-3);
        assert!((lerp_degrees(10.0, 350.0, 0.25) - 5.0).abs() < 1e-3);
        assert!((lerp_degrees(90.0, 180.0, 0.5) - 135.0).abs() < 1e-3);
    }
}
