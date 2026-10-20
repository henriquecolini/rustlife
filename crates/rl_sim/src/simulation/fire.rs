//! Weapons in the tick: firing, swinging, launching projectiles, the
//! Manipulator, and what projectiles do when they land.

use std::sync::Arc;

use glam::Vec3;
use rl_data::{ExplosionConfig, HitscanConfig, MeleeConfig, ProjectileConfig, Shot};

use super::{ExplosionEvent, PlayerId, ShotEvent, Simulation, TickEvents};
use crate::collision::Hull;
use crate::combat::hitscan::{self, ShotTrace, Struck};
use crate::combat::{Cause, Damage, blast};
use crate::movement::{UserCmd, view_direction};
use crate::rng::Rng;
use crate::scene::Scene;
use crate::tools::manipulator::{Aim, ToolEvent};
use crate::weapons::projectile::{Flight, Projectile};
use crate::weapons::{Mode, WeaponId};

/// One bullet of a hitscan shot.
#[derive(Debug, Clone, Copy)]
pub(super) struct Bullet {
    /// The way it flew.
    pub(super) direction: Vec3,
    /// Where it went.
    pub(super) trace: ShotTrace,
    /// The damage it deals, after falloff.
    pub(super) damage: f32,
}

impl Simulation {
    /// Runs the weapon in hand of a living player for the tick: the
    /// inventory decides whether the trigger is pulled, the shot goes off;
    /// with the Manipulator in hand, the tool runs instead.
    pub(super) fn run_weapon(&mut self, id: PlayerId, cmd: &UserCmd, events: &mut TickEvents) {
        let arsenal = Arc::clone(&self.arsenal);
        let movement_eye = self.eye_position(id);
        let Some(player) = self.players.get_mut(id.index()).and_then(Option::as_mut) else {
            return;
        };
        let pulled = player.inventory.tick(&arsenal, cmd, self.dt);
        let aim = Aim {
            eye: movement_eye.unwrap_or(player.state.origin),
            pitch: cmd.pitch,
            yaw: cmd.yaw,
        };

        let tool_event = match player.inventory.tool(&arsenal) {
            Some(intervals) => player.manipulator.tick(
                player.body,
                &aim,
                cmd.buttons,
                intervals,
                self.dt,
                &self.world,
                &mut self.physics,
                &self.data.manipulator,
            ),
            // Putting the tool away lets go of what it held.
            None => player
                .manipulator
                .drop_held(&mut self.physics)
                .map(ToolEvent::Dropped),
        };
        if let Some(event) = tool_event {
            if let ToolEvent::Launched(body)
            | ToolEvent::Punted(body)
            | ToolEvent::Dropped(body)
            | ToolEvent::GripBroke(body) = event
            {
                self.thrown.retain(|&(thrown, ..)| thrown != body);
                self.thrown.push((body, id, self.tick));
            }
            events.tools.push((id, event));
        }

        if let Some((weapon, mode)) = pulled.fired {
            events.fired.push((id, weapon, mode));
            let Some(fire) = arsenal.fire(weapon, mode) else {
                return;
            };
            match &fire.shot {
                Shot::Hitscan(bullets) => {
                    self.fire_bullets(id, weapon, mode, bullets, &aim, cmd, events);
                }
                Shot::Melee(swing) => self.swing(id, weapon, swing, &aim, cmd, events),
                Shot::Projectile(projectile) => {
                    self.launch(id, weapon, mode, projectile, &aim);
                }
                Shot::Manipulator => {}
            }
        }
    }

    /// Fires a hitscan shot's bullets, each with its own spread.
    fn fire_bullets(
        &mut self,
        id: PlayerId,
        weapon: WeaponId,
        mode: Mode,
        bullets: &HitscanConfig,
        aim: &Aim,
        cmd: &UserCmd,
        events: &mut TickEvents,
    ) {
        for bullet in self.bullets(id, mode, bullets, aim, cmd) {
            self.strike(
                id,
                weapon,
                bullet.trace.struck,
                bullet.damage,
                bullet.direction * bullets.force,
                bullet.trace.end,
                events,
            );
            events.shots.push(ShotEvent {
                player: id,
                weapon,
                start: aim.eye,
                trace: bullet.trace,
            });
        }
    }

    /// Where a hitscan shot's bullets go, each with its own spread, and the
    /// damage each deals after falloff. The spread is drawn from the
    /// command's number, so a client predicting its own shot sees the same
    /// bullets the server fires; other players are where the shooter saw
    /// them.
    pub(super) fn bullets(
        &self,
        id: PlayerId,
        mode: Mode,
        bullets: &HitscanConfig,
        aim: &Aim,
        cmd: &UserCmd,
    ) -> Vec<Bullet> {
        let targets = self.targets_seen(cmd.view_tick);
        let forward = view_direction(aim.pitch, aim.yaw);
        let mut rng = Rng::seeded(&[u64::from(cmd.number), id.index() as u64, mode as u64]);
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
        };
        (0..bullets.pellets)
            .map(|_| {
                let direction = hitscan::spread(forward, bullets.spread, &mut rng);
                let end = aim.eye + direction * bullets.range;
                let trace = hitscan::trace(&scene, &targets, id, aim.eye, end, &Hull::POINT);
                let falloff = bullets
                    .falloff
                    .map_or(1.0, |falloff| falloff.scale(aim.eye.distance(trace.end)));
                Bullet {
                    direction,
                    trace,
                    damage: bullets.damage * falloff,
                }
            })
            .collect()
    }

    /// Swings a melee weapon: a small box swept along the view.
    fn swing(
        &mut self,
        id: PlayerId,
        weapon: WeaponId,
        swing: &MeleeConfig,
        aim: &Aim,
        cmd: &UserCmd,
        events: &mut TickEvents,
    ) {
        let forward = view_direction(aim.pitch, aim.yaw);
        let trace = self.swing_trace(id, swing, aim, cmd);
        self.strike(
            id,
            weapon,
            trace.struck,
            swing.damage,
            forward * swing.force,
            trace.end,
            events,
        );
        events.shots.push(ShotEvent {
            player: id,
            weapon,
            start: aim.eye,
            trace,
        });
    }

    /// Where a swing goes: a small box swept along the view, against other
    /// players where the swinger saw them.
    pub(super) fn swing_trace(
        &self,
        id: PlayerId,
        swing: &MeleeConfig,
        aim: &Aim,
        cmd: &UserCmd,
    ) -> ShotTrace {
        let targets = self.targets_seen(cmd.view_tick);
        let forward = view_direction(aim.pitch, aim.yaw);
        let hull = Hull::new(Vec3::splat(-swing.size), Vec3::splat(swing.size));
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
        };
        let end = aim.eye + forward * swing.range;
        hitscan::trace(&scene, &targets, id, aim.eye, end, &hull)
    }

    /// A bullet or swing landing: hurts a player, or shoves a prop with
    /// `impulse` at `point`.
    fn strike(
        &mut self,
        id: PlayerId,
        weapon: WeaponId,
        struck: Struck,
        damage: f32,
        impulse: Vec3,
        point: Vec3,
        events: &mut TickEvents,
    ) {
        match struck {
            Struck::Player(victim) => {
                let damage = Damage {
                    amount: damage,
                    attacker: Some(id),
                    cause: Cause::Weapon(weapon),
                };
                self.hurt(victim, damage, events);
            }
            Struck::Prop(body) => self.physics.apply_impulse(body, impulse, point),
            Struck::World | Struck::Nothing => {}
        }
    }

    /// Launches a projectile from the eyes along the view.
    fn launch(
        &mut self,
        id: PlayerId,
        weapon: WeaponId,
        mode: Mode,
        projectile: &ProjectileConfig,
        aim: &Aim,
    ) {
        let forward = view_direction(aim.pitch, aim.yaw);
        self.projectiles.push(Projectile {
            id: self.next_projectile,
            weapon,
            mode,
            owner: id,
            position: aim.eye,
            velocity: forward * projectile.speed + Vec3::Z * projectile.lift,
            age: 0.0,
            resting: false,
        });
        self.next_projectile = self.next_projectile.wrapping_add(1);
    }

    /// Flies every projectile one tick; those that strike, burst or expire
    /// are resolved and removed.
    pub(super) fn advance_projectiles(&mut self, events: &mut TickEvents) {
        if self.projectiles.is_empty() {
            return;
        }
        let arsenal = Arc::clone(&self.arsenal);
        let targets = self.targets();
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
        };
        let mut ended = Vec::new();
        self.projectiles.retain_mut(|projectile| {
            let Some(Shot::Projectile(config)) = arsenal
                .fire(projectile.weapon, projectile.mode)
                .map(|fire| &fire.shot)
            else {
                return false;
            };
            match projectile.advance(config, self.data.physics.gravity, self.dt, &scene, &targets) {
                Flight::Flying | Flight::Bounced => true,
                flight => {
                    ended.push((projectile.clone(), flight));
                    false
                }
            }
        });
        for (projectile, flight) in ended {
            let Some(Shot::Projectile(config)) = arsenal
                .fire(projectile.weapon, projectile.mode)
                .map(|fire| &fire.shot)
            else {
                continue;
            };
            self.land(&projectile, config, flight, events);
        }
    }

    /// Resolves a projectile's end: a direct hit's damage, then its burst.
    fn land(
        &mut self,
        projectile: &Projectile,
        config: &ProjectileConfig,
        flight: Flight,
        events: &mut TickEvents,
    ) {
        let center = match flight {
            Flight::Struck { struck, point, .. } => {
                if let Struck::Player(victim) = struck
                    && config.hit_damage > 0.0
                {
                    let damage = Damage {
                        amount: config.hit_damage,
                        attacker: Some(projectile.owner),
                        cause: Cause::Weapon(projectile.weapon),
                    };
                    self.hurt(victim, damage, events);
                }
                point
            }
            Flight::Fused => projectile.position,
            Flight::Expired | Flight::Flying | Flight::Bounced => return,
        };
        if let Some(explosion) = &config.explosion {
            self.explode(
                projectile.owner,
                projectile.weapon,
                center,
                explosion,
                events,
            );
        }
    }

    /// A blast at `center`: damages and throws the players it reaches,
    /// shoves the props.
    fn explode(
        &mut self,
        owner: PlayerId,
        weapon: WeaponId,
        center: Vec3,
        explosion: &ExplosionConfig,
        events: &mut TickEvents,
    ) {
        events.explosions.push(ExplosionEvent {
            position: center,
            radius: explosion.radius,
            weapon,
        });
        for reach in blast::props(center, explosion, &self.physics) {
            let impulse = reach.direction * explosion.force * reach.scale;
            self.physics
                .apply_impulse(reach.body, impulse, reach.center);
        }
        let targets = self.targets();
        for reach in blast::players(center, explosion, &targets, &self.world) {
            if let Some(player) = self.player_mut(reach.player) {
                player.state.velocity += reach.direction * explosion.knockback * reach.scale;
            }
            let damage = Damage {
                amount: explosion.damage * reach.scale,
                attacker: Some(owner),
                cause: Cause::Weapon(weapon),
            };
            self.hurt(reach.player, damage, events);
        }
    }
}
