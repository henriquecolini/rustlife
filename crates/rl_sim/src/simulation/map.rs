//! The map's logic in the tick: what players did to it, its movers moving
//! through them, and what it does back.
//!
//! The logic itself (`crate::logic`) knows the map's wiring but not the
//! players. Each tick the simulation tells it who pressed use on what, who
//! touches which mover, what damage reached movers and where everyone is;
//! runs it; moves the movers; and applies what it asked for.
//!
//! A moving door or button pushes the players in its way: each is carried
//! as the part of the mover touching them moves, if there is room. If
//! there is not, the mover is blocked: it stays put this tick, hurts the
//! player by its `dmg`, and — if it does that — turns back. Players ride
//! movers that rise beneath them, since a floor moving up into a player
//! pushes them up.

use glam::{Quat, Vec3};
use rl_data::ExplosionConfig;
use serde::{Deserialize, Serialize};

use super::{PlayerId, Simulation, Spawn, TickEvents};
use crate::collision::Hull;
use crate::collision::contents::MASK_PLAYER_SOLID;
use crate::combat::{Cause, Damage};
use crate::logic::{Effect, LogicState, Senses};
use crate::movement::{PlayerState, view_direction};
use crate::scene::Scene;

/// How far beyond the touching surface a pushed player is carried, so the
/// next tick's trace starts clear of the mover.
const PUSH_CLEARANCE: f32 = 0.125;

/// Margin within which a player counts as touching a mover.
const TOUCH_MARGIN: f32 = 1.0;

/// Something the map's logic did that players see or hear.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MapEvent {
    /// A door or button began to move.
    MoverStarted {
        /// Its BSP model.
        model: u32,
    },
    /// A door or button came to rest.
    MoverStopped {
        /// Its BSP model.
        model: u32,
    },
    /// A button was pressed.
    Pressed {
        /// Its BSP model.
        model: u32,
    },
    /// A breakable broke.
    Broke {
        /// Its BSP model.
        model: u32,
    },
    /// A player was teleported.
    Teleported {
        /// Who.
        player: PlayerId,
        /// Where to, and the way they now face.
        spawn: Spawn,
    },
}

/// A mover's pose as last given to collision and physics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct MoverPose {
    origin: Vec3,
    rotation: Quat,
    solid: bool,
    moving: bool,
}

impl Simulation {
    /// Runs the map's logic for the tick (see the [module docs](self)).
    pub(super) fn run_logic(&mut self, events: &mut TickEvents) {
        let players = self.living_boxes();
        let uses = self.used_movers();
        let touches = self.touched_movers(&players);
        let damage = std::mem::take(&mut self.brush_damage);
        let mut effects = Vec::new();
        self.logic.step(
            self.tick,
            &Senses {
                players: &players,
                uses: &uses,
                touches: &touches,
                damage: &damage,
            },
            &mut effects,
        );
        self.move_movers(events);
        for effect in effects {
            self.apply_effect(effect, events);
        }
        self.sync_movers_reporting(Some(events));
    }

    /// Puts the map's logic back as the map placed it, for a new match.
    pub(super) fn reset_logic(&mut self) {
        for effect in self.logic.reset() {
            if let Effect::Hazard { index, enabled } = effect
                && let Some(hazard) = self.hazards.get_mut(index)
            {
                hazard.enabled = enabled;
            }
        }
        self.sync_movers();
    }

    /// Takes on the server's view of the map's logic (a client's replica)
    /// and moves the movers' collision to match.
    pub fn set_logic_state(&mut self, state: &LogicState) {
        self.logic.restore(state);
        self.sync_movers();
        self.physics.refresh();
    }

    /// Gives collision and physics every mover's current pose.
    pub(super) fn sync_movers(&mut self) {
        self.sync_movers_reporting(None);
    }

    /// As [`Simulation::sync_movers`], reporting movers that started or
    /// stopped moving to `events`.
    fn sync_movers_reporting(&mut self, mut events: Option<&mut TickEvents>) {
        let count = self.logic.movers().len();
        self.mover_poses.resize(
            count,
            MoverPose {
                origin: Vec3::NAN,
                rotation: Quat::IDENTITY,
                solid: false,
                moving: false,
            },
        );
        for index in 0..count {
            let Some((origin, rotation)) = self.logic.mover_pose(index) else {
                continue;
            };
            let mover = &self.logic.movers()[index];
            let pose = MoverPose {
                origin,
                rotation,
                solid: mover.solid(),
                moving: mover.moving(),
            };
            let previous = self.mover_poses[index];
            if pose == previous {
                continue;
            }
            let model = mover.model;
            if let Some(events) = events.as_deref_mut()
                && pose.moving != previous.moving
            {
                let model = u32::try_from(model).unwrap_or(u32::MAX);
                events.map.push(if pose.moving {
                    MapEvent::MoverStarted { model }
                } else {
                    MapEvent::MoverStopped { model }
                });
            }
            self.mover_poses[index] = pose;
            self.world.set_mover(model, origin, rotation, pose.solid);
            self.physics.set_mover(model, origin, rotation, pose.solid);
        }
    }

    /// Every living player's box.
    fn living_boxes(&self) -> Vec<(PlayerId, Vec3, Vec3)> {
        self.players()
            .filter(|(_, player)| player.alive())
            .map(|(id, player)| {
                let hull = player.state.hull(&self.data.movement);
                (
                    id,
                    player.state.origin + hull.mins,
                    player.state.origin + hull.maxs,
                )
            })
            .collect()
    }

    /// The movers players pressed use on this tick: what each looks at
    /// within reach — a mover even if players pass through it (as some
    /// valves and switches do), unless something solid is in front.
    fn used_movers(&mut self) -> Vec<(PlayerId, usize)> {
        let users = std::mem::take(&mut self.users);
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
        };
        users
            .into_iter()
            .filter_map(|id| {
                let player = self.player(id)?;
                let eye = self.eye_position(id)?;
                let (pitch, yaw) = player.look;
                let end = eye + view_direction(pitch, yaw) * self.data.map_logic.use_range;
                let trace = crate::collision::CollisionQuery::trace_hull(
                    &scene,
                    eye,
                    end,
                    &Hull::POINT,
                    MASK_PLAYER_SOLID,
                );
                match self.world.trace_movers(eye, end) {
                    Some((fraction, model)) if fraction <= trace.fraction => Some((id, model)),
                    _ => Some((id, trace.model?)),
                }
            })
            .collect()
    }

    /// The movers opened by touch that a player touches.
    fn touched_movers(&self, players: &[(PlayerId, Vec3, Vec3)]) -> Vec<(PlayerId, usize)> {
        let mut touches = Vec::new();
        for mover in self
            .logic
            .movers()
            .iter()
            .filter(|mover| mover.touched_by_players() && !mover.gone())
        {
            let Some((mins, maxs)) = self.world.mover_bounds(mover.model) else {
                continue;
            };
            for &(id, player_mins, player_maxs) in players {
                let margin = Vec3::splat(TOUCH_MARGIN);
                if (player_mins - margin).cmple(maxs).all()
                    && mins.cmple(player_maxs + margin).all()
                {
                    touches.push((id, mover.model));
                }
            }
        }
        touches
    }

    /// Moves every moving mover a tick, pushing players in its way.
    fn move_movers(&mut self, events: &mut TickEvents) {
        for index in 0..self.logic.movers().len() {
            let mover = &self.logic.movers()[index];
            let Some(next) = mover.next_position(self.dt) else {
                continue;
            };
            let (model, solid, crush) = (mover.model, mover.solid(), mover.crush_damage);
            let Some(before) = self.logic.mover_pose(index) else {
                continue;
            };
            let after = self.logic.mover_pose_with(index, &|i| {
                if i == index {
                    next
                } else {
                    self.logic
                        .movers()
                        .get(i)
                        .map_or(0.0, |mover| mover.position)
                }
            });
            let Some(after) = after else {
                continue;
            };
            let blockers = if solid {
                self.push_players(model, before, after)
            } else {
                Vec::new()
            };
            if blockers.is_empty() {
                if let Some(mover) = self.logic.mover_mut(index)
                    && let Some(output) = mover.arrive(next, self.tick)
                {
                    self.logic.fire_mover(index, output);
                }
                continue;
            }
            // Blocked: back where it was, hurting whoever is in the way.
            self.world.set_mover(model, before.0, before.1, solid);
            if let Some(mover) = self.logic.mover_mut(index)
                && let Some(output) = mover.blocked(self.tick)
            {
                self.logic.fire_mover(index, output);
            }
            if crush > 0.0 {
                for id in blockers {
                    let damage = Damage {
                        amount: crush,
                        attacker: None,
                        cause: Cause::Hazard,
                    };
                    self.hurt(id, damage, events);
                }
            }
        }
    }

    /// Places mover `model` at `after` (from `before`) and carries the
    /// living players it now overlaps along with it. Returns the players
    /// that could not be moved out of its way; when there are any, nobody
    /// is moved.
    fn push_players(
        &mut self,
        model: usize,
        before: (Vec3, Quat),
        after: (Vec3, Quat),
    ) -> Vec<PlayerId> {
        self.world.set_mover(model, after.0, after.1, true);
        let (before_origin, before_rotation) = before;
        let (after_origin, after_rotation) = after;
        let carry = |point: Vec3| {
            after_origin + after_rotation * (before_rotation.inverse() * (point - before_origin))
        };
        let mut moves = Vec::new();
        let mut blocked = Vec::new();
        for (id, player) in self.players().filter(|(_, player)| player.alive()) {
            let hull = player.state.hull(&self.data.movement);
            let origin = player.state.origin;
            if !self.world.mover_overlaps(model, origin, &hull) {
                continue;
            }
            let center = origin + hull.center();
            let shift = carry(center) - center;
            let target = origin + shift + shift.normalize_or_zero() * PUSH_CLEARANCE;
            let scene = Scene {
                world: &self.world,
                physics: &self.physics,
                ignore: player.manipulator.held(),
            };
            let stuck = crate::collision::CollisionQuery::trace_hull(
                &scene,
                target,
                target,
                &hull,
                MASK_PLAYER_SOLID,
            )
            .start_solid;
            if stuck {
                blocked.push(id);
            } else {
                moves.push((id, target, shift));
            }
        }
        if !blocked.is_empty() {
            return blocked;
        }
        for (id, target, shift) in moves {
            if let Some(player) = self.player_mut(id) {
                player.state.origin = target;
                // Carried upwards, a player leaves the ground with the
                // mover's speed rather than snapping back down to it.
                if shift.z > 0.0 {
                    player.state.velocity.z = player.state.velocity.z.max(0.0);
                }
            }
        }
        Vec::new()
    }

    /// Does what the logic asked for.
    fn apply_effect(&mut self, effect: Effect, events: &mut TickEvents) {
        match effect {
            Effect::Explosion {
                position,
                damage,
                radius,
            } => {
                let tuning = &self.data.map_logic.explosion;
                let explosion = ExplosionConfig {
                    radius,
                    damage,
                    knockback: damage * tuning.knockback_per_damage,
                    force: damage * tuning.force_per_damage,
                };
                self.explode(None, None, position, &explosion, events);
            }
            Effect::Teleport {
                player,
                origin,
                yaw,
            } => {
                if self.teleport_player(player, origin) {
                    let origin = self.player(player).map_or(origin, |p| p.state.origin);
                    if let Some(moved) = self.player_mut(player) {
                        moved.look.1 = yaw;
                    }
                    events.map.push(MapEvent::Teleported {
                        player,
                        spawn: Spawn { origin, yaw },
                    });
                }
            }
            Effect::Push { player, velocity } => {
                if let Some(player) = self.player_mut(player) {
                    push(&mut player.state, velocity);
                }
            }
            Effect::Hazard { index, enabled } => {
                if let Some(hazard) = self.hazards.get_mut(index) {
                    hazard.enabled = enabled;
                }
            }
            Effect::Broke { model } => events.map.push(MapEvent::Broke {
                model: u32::try_from(model).unwrap_or(u32::MAX),
            }),
            Effect::Pressed { model } => events.map.push(MapEvent::Pressed {
                model: u32::try_from(model).unwrap_or(u32::MAX),
            }),
        }
    }
}

/// Brings `state`'s speed along `velocity`'s direction up to its length,
/// as wind does: a player already faster that way keeps their speed.
fn push(state: &mut PlayerState, velocity: Vec3) {
    let speed = velocity.length();
    let Some(direction) = velocity.try_normalize() else {
        return;
    };
    let along = state.velocity.dot(direction);
    if along < speed {
        state.velocity += direction * (speed - along);
    }
    if state.velocity.z > 0.0 {
        state.on_ground = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pushes_raise_speed_along_their_direction_only() {
        let mut state = PlayerState::at(Vec3::ZERO);
        state.velocity = Vec3::new(100.0, 50.0, 0.0);
        state.on_ground = true;
        push(&mut state, Vec3::new(0.0, 0.0, 300.0));
        assert_eq!(state.velocity, Vec3::new(100.0, 50.0, 300.0));
        assert!(!state.on_ground);

        push(&mut state, Vec3::new(40.0, 0.0, 0.0));
        assert_eq!(state.velocity.x, 100.0, "already faster than the push");
    }
}
