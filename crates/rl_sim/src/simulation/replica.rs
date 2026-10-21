//! The client's side of the simulation.
//!
//! A client does not run the tick: the server does, and sends snapshots of
//! what came of it. The client keeps a [`Simulation`] of the same map as a
//! *replica* — the server's players, props, pickups, projectiles and match,
//! written in as each snapshot arrives — so everything that draws the game
//! reads one model whichever side computed it.
//!
//! The one thing a client runs itself is its own player, ahead of the
//! server ([`Simulation::predict`]): movement and the weapon in hand,
//! against the map and the props where the server last put them. That is
//! the same deterministic code the server runs, so when the server's answer
//! for a command comes back it almost always matches the prediction; where
//! it does not (another player's blast, a prop that moved), the client
//! starts again from the server's state and replays the commands the
//! server has not answered yet.
//!
//! Prediction never hurts anyone, pushes props or uses the Manipulator:
//! those effects are the server's to decide. It does say where predicted
//! bullets and swings go, so the shooter sees their tracers at once.

use std::sync::Arc;

use glam::Vec3;
use rl_data::Shot;

use super::{Player, PlayerId, ShotEvent, Simulation, Spawn};
use crate::movement::{MoveEvents, UserCmd, simulate};
use crate::physics::{BodyId, BodyPose};
use crate::pickups::Pickup;
use crate::rules::Match;
use crate::scene::Scene;
use crate::tools::manipulator::Aim;
use crate::weapons::WeaponTick;
use crate::weapons::projectile::Projectile;

/// What predicting one of the local player's commands did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prediction {
    /// The player's movement events.
    pub moved: MoveEvents,
    /// What the weapon in hand did.
    pub weapon: WeaponTick,
    /// Where predicted bullets and swings went. Nobody is hurt by them:
    /// damage is the server's to deal.
    pub shots: Vec<ShotEvent>,
}

impl Simulation {
    /// Sets how many ticks have run, to the server's count.
    pub fn set_tick_count(&mut self, tick: u64) {
        self.tick = tick;
    }

    /// The player with id `id`, created (standing at the origin with a
    /// fresh loadout) if the slot is empty: for writing in the server's
    /// description of them.
    pub fn replica_player(&mut self, id: PlayerId) -> &mut Player {
        let player = if let Some(player) = self.players.get_mut(id.index()).and_then(Option::take) {
            player
        } else {
            self.game.add_player(id, &self.data.rules);
            self.new_player(Spawn {
                origin: Vec3::ZERO,
                yaw: 0.0,
            })
        };
        self.store_player(id, player)
    }

    /// Replaces the match: scores and clock.
    pub fn set_match(&mut self, game: Match) {
        self.game = game;
    }

    /// The map's pickups, to update their state.
    pub fn pickups_mut(&mut self) -> &mut [Pickup] {
        &mut self.pickups
    }

    /// Replaces the projectiles in flight.
    pub fn set_projectiles(&mut self, projectiles: Vec<Projectile>) {
        self.projectiles = projectiles;
    }

    /// Places props where the server says they are, and brings collision
    /// up to date with them.
    pub fn set_prop_poses(&mut self, poses: impl IntoIterator<Item = (BodyId, BodyPose)>) {
        for (body, pose) in poses {
            self.physics.set_pose(body, pose);
        }
        self.physics.refresh();
    }

    /// Runs one of a player's commands ahead of the server: their movement
    /// and the weapon in hand. Nothing else happens — nobody is hurt, no
    /// prop is pushed, the Manipulator is left to the server — and the
    /// dead, and everyone between matches, stay as they are.
    pub fn predict(&mut self, id: PlayerId, cmd: &UserCmd) -> Prediction {
        let mut prediction = Prediction::default();
        if self.game.intermission() {
            return prediction;
        }
        let arsenal = Arc::clone(&self.arsenal);
        let Some(player) = self
            .players
            .get_mut(id.index())
            .and_then(Option::as_mut)
            .filter(|player| player.alive())
        else {
            return prediction;
        };
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: player.manipulator.held(),
        };
        let (state, moved) = simulate(&player.state, cmd, &self.data.movement, self.dt, &scene);
        player.state = state;
        player.look = (cmd.pitch, cmd.yaw);
        prediction.moved = moved;
        prediction.weapon = player.inventory.tick(&arsenal, cmd, self.dt);

        let Some((weapon, mode)) = prediction.weapon.fired else {
            return prediction;
        };
        let aim = Aim {
            eye: self.eye_position(id).unwrap_or(state.origin),
            pitch: cmd.pitch,
            yaw: cmd.yaw,
        };
        let shot = |trace| ShotEvent {
            player: id,
            weapon,
            start: aim.eye,
            trace,
        };
        match arsenal.fire(weapon, mode).map(|fire| &fire.shot) {
            Some(Shot::Hitscan(bullets)) => {
                prediction.shots = self
                    .bullets(id, mode, bullets, &aim, cmd)
                    .into_iter()
                    .map(|bullet| shot(bullet.trace))
                    .collect();
            }
            Some(Shot::Melee(swing)) => {
                prediction.shots = vec![shot(self.swing_trace(id, swing, &aim, cmd))];
            }
            Some(Shot::Projectile(_) | Shot::Manipulator) | None => {}
        }
        prediction
    }
}
