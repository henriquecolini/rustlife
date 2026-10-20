//! Life and death in the tick: damage, kills, respawning, pickups, hazards
//! and the match's turns.

use std::sync::Arc;

use glam::Vec3;

use super::{DamageEvent, Kill, Life, PlayerId, RespawnEvent, Simulation, Spawn, TickEvents};
use crate::combat::{Cause, Damage, Vitals};
use crate::hazards::DOSE_INTERVAL;
use crate::movement::{Buttons, PlayerState, UserCmd};
use crate::physics::Impact;
use crate::rng::Rng;
use crate::rules::{self, MatchEvent};
use crate::tools::manipulator::Manipulator;
use crate::weapons::Inventory;

/// Two players spawning closer than this would overlap, units.
const SPAWN_CLEARANCE: f32 = 48.0;

impl Simulation {
    /// Deals `damage` to a living player, armor first; kills them if their
    /// health runs out. Nobody is hurt between matches. A player's own
    /// weapon hurts them less, by the rules' self-damage factor.
    pub(super) fn hurt(&mut self, victim: PlayerId, mut damage: Damage, events: &mut TickEvents) {
        if !self.game.playing() {
            return;
        }
        let Some(player) = self.players.get_mut(victim.index()) else {
            return;
        };
        if !player.alive() || damage.amount <= 0.0 {
            return;
        }
        if damage.attacker == Some(victim) && matches!(damage.cause, Cause::Weapon(_)) {
            damage.amount *= self.data.rules.self_damage;
        }
        let health_lost = player.vitals.take(damage.amount, &self.data.player.armor);
        events.damage.push(DamageEvent {
            victim,
            damage,
            health_lost,
        });
        if !player.vitals.alive() {
            self.kill(victim, &damage, events);
        }
    }

    /// A player dies: they drop what they hold, leave the physics world
    /// and the match scores it.
    fn kill(&mut self, victim: PlayerId, damage: &Damage, events: &mut TickEvents) {
        let Some(player) = self.players.get_mut(victim.index()) else {
            return;
        };
        player.life = Life::Dead {
            since: self.tick,
            killer: damage.attacker,
        };
        player.state.velocity = Vec3::ZERO;
        player.manipulator.drop_held(&mut self.physics);
        self.physics.set_player_enabled(player.body, false);
        events.kills.push(Kill {
            victim,
            killer: damage.attacker,
            cause: damage.cause,
        });
        if let Some(event) = self
            .game
            .record_death(victim, damage.attacker, &self.data.rules)
        {
            self.on_match_event(event, events);
        }
    }

    /// Brings back the dead who asked to (fire or jump after the respawn
    /// delay) or have waited out the forced delay.
    pub(super) fn respawn_waiting(
        &mut self,
        commands: &[(PlayerId, UserCmd)],
        events: &mut TickEvents,
    ) {
        let respawn = self.data.rules.respawn;
        for index in 0..self.players.len() {
            let id = PlayerId::from_index(index);
            let Life::Dead { since, .. } = self.players[index].life else {
                continue;
            };
            let dead_for = self.tick.saturating_sub(since) as f32 * self.dt;
            let asked = commands.iter().any(|(player, cmd)| {
                *player == id
                    && (cmd.buttons.contains(Buttons::ATTACK)
                        || cmd.buttons.contains(Buttons::JUMP))
            });
            if (asked && dead_for >= respawn.delay) || dead_for >= respawn.force {
                self.respawn(id, events);
            }
        }
    }

    /// Puts a player back in the game at a spawn point, with fresh health
    /// and the loadout.
    pub(super) fn respawn(&mut self, id: PlayerId, events: &mut TickEvents) {
        let home = self.players.get(id.index()).map(|player| player.spawn);
        let Some(spawn) = self.choose_spawn(Some(id)).or(home) else {
            return;
        };
        let origin = self.free_position(spawn.origin).unwrap_or(spawn.origin);
        let spawn = Spawn { origin, ..spawn };
        let arsenal = Arc::clone(&self.arsenal);
        let Some(player) = self.players.get_mut(id.index()) else {
            return;
        };
        player.manipulator.drop_held(&mut self.physics);
        *player = super::Player {
            state: PlayerState::at(origin),
            body: player.body,
            manipulator: Manipulator::default(),
            vitals: Vitals::spawn(&self.data.player),
            inventory: Inventory::loadout(&arsenal, &self.data.rules.loadout),
            life: Life::Alive,
            spawn,
            look: (0.0, spawn.yaw),
        };
        let hull = player.state.hull(&self.data.movement);
        self.physics.set_player_enabled(player.body, true);
        self.physics
            .set_player(player.body, origin, Vec3::ZERO, &hull);
        events.respawns.push(RespawnEvent { player: id, spawn });
    }

    /// A spawn point for `player` (`None`: someone joining) away from the
    /// other living players, with room to stand and nobody on it.
    pub(super) fn choose_spawn(&self, player: Option<PlayerId>) -> Option<Spawn> {
        let others: Vec<Vec3> = self
            .players()
            .filter(|&(id, other)| Some(id) != player && other.alive())
            .map(|(_, other)| other.state.origin)
            .collect();
        let points: Vec<Vec3> = self.spawns.iter().map(|spawn| spawn.origin).collect();
        let seed = player.map_or(u64::MAX, |id| id.index() as u64);
        let mut rng = Rng::seeded(&[self.tick, seed]);
        let index = rules::choose_spawn(
            &points,
            &others,
            self.data.rules.respawn.candidates,
            &mut rng,
            |point| {
                others
                    .iter()
                    .all(|other| other.distance(point) > SPAWN_CLEARANCE)
                    && self.free_position(point).is_some()
            },
        )?;
        self.spawns.get(index).copied()
    }

    /// A prop hitting a player: the damage is the thrower's if they threw
    /// (or punted, or dropped) it recently.
    pub(super) fn prop_impact(&mut self, impact: Impact, events: &mut TickEvents) {
        let Some(victim) = self
            .players()
            .find(|(_, player)| player.body == impact.player)
            .map(|(id, _)| id)
        else {
            return;
        };
        events.impacts.push((victim, impact));
        let credit = (self.data.rules.prop_credit / self.dt) as u64;
        let attacker = self
            .thrown
            .iter()
            .find(|&&(body, _, tick)| body == impact.prop && self.tick - tick <= credit)
            .map(|&(_, thrower, _)| thrower);
        let damage = Damage {
            amount: impact.damage,
            attacker,
            cause: Cause::Prop,
        };
        self.hurt(victim, damage, events);
        self.thrown
            .retain(|&(_, _, tick)| self.tick.saturating_sub(tick) <= credit);
    }

    /// Lets pickups come back, and living players take those they touch.
    pub(super) fn touch_pickups(&mut self, events: &mut TickEvents) {
        for pickup in &mut self.pickups {
            pickup.tick(self.dt);
        }
        if !self.game.playing() {
            return;
        }
        let arsenal = Arc::clone(&self.arsenal);
        for (index, player) in self.players.iter_mut().enumerate() {
            if !player.alive() {
                continue;
            }
            let hull = player.state.hull(&self.data.movement);
            let (mins, maxs) = (
                player.state.origin + hull.mins,
                player.state.origin + hull.maxs,
            );
            for (pickup_index, pickup) in self.pickups.iter_mut().enumerate() {
                if pickup.available()
                    && pickup.touches(mins, maxs)
                    && pickup.take(
                        &mut player.vitals,
                        &mut player.inventory,
                        &arsenal,
                        &self.data.player,
                    )
                {
                    events
                        .pickups
                        .push((PlayerId::from_index(index), pickup_index));
                }
            }
        }
    }

    /// Doses everyone inside a hazard, twice a second.
    pub(super) fn apply_hazards(&mut self, events: &mut TickEvents) {
        if self.hazards.is_empty() {
            return;
        }
        self.hazard_clock += self.dt;
        if self.hazard_clock < DOSE_INTERVAL {
            return;
        }
        self.hazard_clock -= DOSE_INTERVAL;
        let mut doses = Vec::new();
        for (id, player) in self.players().filter(|(_, player)| player.alive()) {
            let hull = player.state.hull(&self.data.movement);
            let (mins, maxs) = (
                player.state.origin + hull.mins,
                player.state.origin + hull.maxs,
            );
            for hazard in self
                .hazards
                .iter()
                .filter(|hazard| hazard.touches(mins, maxs))
            {
                doses.push((id, hazard.dose()));
            }
        }
        for (id, amount) in doses {
            let damage = Damage {
                amount,
                attacker: None,
                cause: Cause::Hazard,
            };
            self.hurt(id, damage, events);
        }
    }

    /// Reports a match ending; a restart puts everyone back at a spawn
    /// point with the map reset.
    pub(super) fn on_match_event(&mut self, event: MatchEvent, events: &mut TickEvents) {
        events.match_event = Some(event);
        if event != MatchEvent::Restarted {
            return;
        }
        self.projectiles.clear();
        self.thrown.clear();
        for pickup in &mut self.pickups {
            pickup.away = 0.0;
        }
        for index in 0..self.players.len() {
            self.respawn(PlayerId::from_index(index), events);
        }
    }
}
