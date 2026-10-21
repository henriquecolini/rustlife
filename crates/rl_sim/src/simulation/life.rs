//! Life and death in the tick: damage, kills, respawning, pickups, hazards
//! and the match's turns.

use std::sync::Arc;

use glam::Vec3;

use super::{DamageEvent, Kill, Life, PlayerId, RespawnEvent, Simulation, Spawn, TickEvents};
use crate::collision::contents::MASK_SHOT;
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
/// How far behind the wall a station's origin may lie and still be seen
/// by a player looking at it, units.
const STATION_DEPTH: f32 = 16.0;

impl Simulation {
    /// Deals `damage` to a living player, armor first; kills them if their
    /// health runs out. Nobody is hurt between matches. A player's own
    /// weapon hurts them less, by the rules' self-damage factor.
    pub(super) fn hurt(&mut self, victim: PlayerId, mut damage: Damage, events: &mut TickEvents) {
        if !self.game.playing() {
            return;
        }
        let Some(player) = self
            .players
            .get_mut(victim.index())
            .and_then(Option::as_mut)
        else {
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
        let Some(player) = self
            .players
            .get_mut(victim.index())
            .and_then(Option::as_mut)
        else {
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
        let points = self.kill_points(damage);
        if let Some(event) =
            self.game
                .record_death(victim, damage.attacker, points, &self.data.rules)
        {
            self.on_match_event(event, events);
        }
    }

    /// What killing with `damage` is worth to the killer, by the rules.
    fn kill_points(&self, damage: &Damage) -> u32 {
        let points = &self.data.rules.points;
        match damage.cause {
            Cause::Weapon(weapon) => self
                .arsenal
                .weapon_name(weapon)
                .map_or(points.kill, |name| points.for_weapon(name)),
            Cause::Prop => points.prop,
            Cause::Fall | Cause::Hazard => points.environment,
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
            let Some(Life::Dead { since, .. }) = self.player(id).map(|player| player.life) else {
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
        let home = self.player(id).map(|player| player.spawn);
        let Some(spawn) = self.choose_spawn(Some(id)).or(home) else {
            return;
        };
        let origin = self.free_position(spawn.origin).unwrap_or(spawn.origin);
        let spawn = Spawn { origin, ..spawn };
        let arsenal = Arc::clone(&self.arsenal);
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
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
        if self.game.intermission() {
            return;
        }
        self.use_stations(events);
        let arsenal = Arc::clone(&self.arsenal);
        for (index, player) in self.players.iter_mut().enumerate() {
            let Some(player) = player.as_mut().filter(|player| player.alive()) else {
                continue;
            };
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

    /// Lets each player holding use draw from the station they look at.
    fn use_stations(&mut self, events: &mut TickEvents) {
        let holders = std::mem::take(&mut self.holders);
        for id in holders {
            let Some(index) = self.station_in_view(id) else {
                continue;
            };
            let (dt, config) = (self.dt, &self.data.player);
            let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
                continue;
            };
            if self.pickups[index].dispense(&mut player.vitals, config, dt) > 0.0 {
                events.charging.push((id, index));
            }
        }
    }

    /// The available station nearest a living player's view: within use
    /// range of their eyes, within the use cone of where they look, and in
    /// plain sight.
    fn station_in_view(&self, id: PlayerId) -> Option<usize> {
        let player = self.player(id).filter(|player| player.alive())?;
        let eye = self.eye_position(id)?;
        let (pitch, yaw) = player.look;
        let forward = crate::movement::view_direction(pitch, yaw);
        let logic = &self.data.map_logic;
        let cone = logic.use_cone.to_radians().cos();
        self.pickups
            .iter()
            .enumerate()
            .filter(|(_, pickup)| pickup.station.is_some() && pickup.available())
            .filter_map(|(index, pickup)| {
                let to = pickup.origin - eye;
                let distance = to.length();
                let facing = to.normalize_or_zero().dot(forward);
                (distance <= logic.use_range && facing >= cone).then_some((index, distance))
            })
            .filter(|&(index, distance)| {
                // Chargers sit on walls: reaching the wall at the charger
                // counts as seeing it.
                let trace = self
                    .world
                    .trace_ray(eye, self.pickups[index].origin, MASK_SHOT);
                !trace.hit() || trace.fraction * distance >= distance - STATION_DEPTH
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
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
                .filter(|hazard| hazard.enabled && hazard.touches(mins, maxs))
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
        if !matches!(event, MatchEvent::Started | MatchEvent::Restarted) {
            return;
        }
        self.projectiles.clear();
        self.thrown.clear();
        for pickup in &mut self.pickups {
            pickup.away = 0.0;
        }
        self.reset_logic();
        let ids: Vec<PlayerId> = self.players().map(|(id, _)| id).collect();
        for id in ids {
            self.respawn(id, events);
        }
    }
}
