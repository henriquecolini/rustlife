//! Weapons in the tick: firing, swinging, launching projectiles, the
//! Manipulator, and what projectiles do when they land.

use std::sync::Arc;

use glam::Vec3;
use rl_data::{ExplosionConfig, HitscanConfig, MeleeConfig, ProjectileConfig, Shot};

use super::breakables::Blow;
use super::{BlastSource, ExplosionEvent, PlayerId, ShotEvent, Simulation, TickEvents};
use crate::collision::Hull;
use crate::combat::hitscan::{self, ShotTrace, Struck};
use crate::combat::{Cause, Damage, Target, blast};
use crate::movement::{UserCmd, view_direction};
use crate::rng::Rng;
use crate::scene::Scene;
use crate::tools::manipulator::{Aim, ToolEvent};
use crate::weapons::projectile::{Flight, Projectile};
use crate::weapons::{Mode, WeaponId};

/// How far a launcher's laser reaches, units: Source's longest trace.
const LASER_RANGE: f32 = 56755.0;

/// One bullet of a hitscan shot.
#[derive(Debug, Clone, Copy)]
pub(super) struct Bullet {
    /// The way it flew.
    pub(super) direction: Vec3,
    /// Where it went.
    pub(super) trace: ShotTrace,
    /// The damage it deals, a headshot's multiplier included.
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
        if let Some(weapon) = player.inventory.active() {
            if pulled.switched {
                events.deploys.push((id, weapon));
            }
            if pulled.reloading {
                events.reloads.push((id, weapon));
            }
            if let Some(noise) = pulled.noise {
                events.noises.push((id, weapon, noise));
            }
        }
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
                let launched = matches!(event, ToolEvent::Launched(_) | ToolEvent::Punted(_));
                let armed = if launched {
                    self.data.physics.breakables.barrels.launched_seconds
                } else {
                    0.0
                };
                self.breakables.arm(body, armed);
            }
            if let ToolEvent::Grabbed(body) = event {
                self.breakables.arm(body, 0.0);
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
                    self.fire_bullets(id, weapon, mode, bullets, &aim, cmd, pulled.penalty, events);
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
        penalty: f32,
        events: &mut TickEvents,
    ) {
        for bullet in self.bullets(id, mode, bullets, aim, cmd, penalty) {
            self.strike(
                id,
                weapon,
                Blow::Bullet,
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
    /// damage each deals (tripled for a headshot). The spread is drawn from
    /// the command's number, so a client predicting its own shot sees the
    /// same bullets the server fires; other players are where the shooter
    /// saw them. `penalty` is the shooter's accuracy penalty as they fired.
    pub(super) fn bullets(
        &self,
        id: PlayerId,
        mode: Mode,
        bullets: &HitscanConfig,
        aim: &Aim,
        cmd: &UserCmd,
        penalty: f32,
    ) -> Vec<Bullet> {
        let cone = bullets
            .accuracy
            .map_or(bullets.cone, |accuracy| accuracy.cone(penalty));
        let head_scale = self.data.player.head_scale;
        let targets = self.targets_seen(cmd.view_tick);
        let forward = view_direction(aim.pitch, aim.yaw);
        let mut rng = Rng::seeded(&[u64::from(cmd.number), id.index() as u64, mode as u64]);
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
            others: &[],
        };
        (0..bullets.pellets)
            .map(|_| {
                let direction = hitscan::spread(forward, cone, &mut rng);
                let end = aim.eye + direction * bullets.range;
                let trace = hitscan::trace(&scene, &targets, id, aim.eye, end, &Hull::POINT);
                let scale = if trace.head { head_scale } else { 1.0 };
                Bullet {
                    direction,
                    trace,
                    damage: bullets.damage * scale,
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
            Blow::Club,
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
            others: &[],
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
        blow: Blow,
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
            Struck::Prop(body) => {
                self.hit_prop(body, damage, blow, Some(id));
                self.physics.apply_impulse(body, impulse, point);
            }
            Struck::Brush(model) => self.brush_damage.push((Some(id), model as usize, damage)),
            Struck::World | Struck::Nothing => {}
        }
    }

    /// Launches a projectile from the eyes along the view. A shot made
    /// while crouching may be a different projectile (a grenade rolled
    /// instead of lobbed).
    fn launch(
        &mut self,
        id: PlayerId,
        weapon: WeaponId,
        mode: Mode,
        projectile: &ProjectileConfig,
        aim: &Aim,
    ) {
        let ducked = self.player(id).is_some_and(|player| player.state.ducked);
        let projectile = match &projectile.crouched {
            Some(crouched) if ducked => crouched.as_ref(),
            _ => projectile,
        };
        if let Some(body) = &projectile.body {
            self.throw_grenade(id, weapon, mode, projectile, body, aim);
            return;
        }
        let forward = view_direction(aim.pitch, aim.yaw);
        let (position, velocity) = match &projectile.missile {
            Some(missile) => {
                // Source's right-hand vector: forward × up.
                let right = forward
                    .cross(Vec3::Z)
                    .try_normalize()
                    .unwrap_or(Vec3::NEG_Y);
                let [ahead, aside, up] = missile.muzzle;
                (
                    aim.eye + forward * ahead + right * aside + Vec3::Z * up,
                    forward * missile.launch_forward + Vec3::Z * missile.launch_up,
                )
            }
            None => (
                aim.eye,
                forward * projectile.speed + Vec3::Z * projectile.lift,
            ),
        };
        self.projectiles.push(Projectile {
            id: self.next_projectile,
            weapon,
            mode,
            owner: id,
            position,
            velocity,
            age: 0.0,
            resting: false,
            heading: forward,
            lit: false,
            steer_clock: 0.0,
        });
        self.next_projectile = self.next_projectile.wrapping_add(1);
    }

    /// Where the laser of a missile's owner points: along their view from
    /// their eyes to the first thing it meets, if they still hold the
    /// launcher with its laser on.
    fn laser_dot(&self, projectile: &Projectile, targets: &[Target]) -> Option<Vec3> {
        let player = self.player(projectile.owner)?;
        let inventory = &player.inventory;
        if !player.alive() || inventory.active() != Some(projectile.weapon) || !inventory.laser() {
            return None;
        }
        let eye = self.eye_position(projectile.owner)?;
        let (pitch, yaw) = player.look;
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
            others: &[],
        };
        let end = eye + view_direction(pitch, yaw) * LASER_RANGE;
        let trace = hitscan::trace(&scene, targets, projectile.owner, eye, end, &Hull::POINT);
        (trace.struck != Struck::Nothing).then_some(trace.end)
    }

    /// Flies every projectile one tick; those that strike, burst or expire
    /// are resolved and removed.
    pub(super) fn advance_projectiles(&mut self, events: &mut TickEvents) {
        if self.projectiles.is_empty() {
            return;
        }
        let arsenal = Arc::clone(&self.arsenal);
        let targets = self.targets();
        let dots: Vec<Option<Vec3>> = self
            .projectiles
            .iter()
            .map(|projectile| self.laser_dot(projectile, &targets))
            .collect();
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
            others: &[],
        };
        let mut ended = Vec::new();
        let mut dots = dots.into_iter();
        self.projectiles.retain_mut(|projectile| {
            let dot = dots.next().flatten();
            let Some(Shot::Projectile(config)) = arsenal
                .fire(projectile.weapon, projectile.mode)
                .map(|fire| &fire.shot)
            else {
                return false;
            };
            match projectile.advance(
                config,
                self.data.physics.gravity,
                self.dt,
                &scene,
                &targets,
                dot,
            ) {
                Flight::Flying | Flight::Bounced => true,
                flight => {
                    ended.push((projectile.clone(), flight));
                    false
                }
            }
        });
        for (projectile, flight) in ended {
            if let Some(player) = self.player_mut(projectile.owner) {
                player
                    .inventory
                    .projectile_ended(&arsenal, projectile.weapon);
            }
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
                if config.hit_damage > 0.0 {
                    match struck {
                        Struck::Player(victim) => {
                            let damage = Damage {
                                amount: config.hit_damage,
                                attacker: Some(projectile.owner),
                                cause: Cause::Weapon(projectile.weapon),
                            };
                            self.hurt(victim, damage, events);
                        }
                        Struck::Brush(model) => self.brush_damage.push((
                            Some(projectile.owner),
                            model as usize,
                            config.hit_damage,
                        )),
                        Struck::Prop(body) => self.hit_prop(
                            body,
                            config.hit_damage,
                            Blow::Bullet,
                            Some(projectile.owner),
                        ),
                        Struck::World | Struck::Nothing => {}
                    }
                }
                point
            }
            Flight::Fused => projectile.position,
            Flight::Expired | Flight::Flying | Flight::Bounced => return,
        };
        if let Some(explosion) = &config.explosion {
            self.explode(
                Some(projectile.owner),
                BlastSource::Weapon(projectile.weapon),
                center,
                explosion,
                events,
            );
        }
    }

    /// A blast at `center` set off by `owner` with `weapon` (neither: the
    /// map's): damages and throws the players it reaches, shoves the props,
    /// damages the breakables.
    pub(super) fn explode(
        &mut self,
        owner: Option<PlayerId>,
        source: BlastSource,
        center: Vec3,
        explosion: &ExplosionConfig,
        events: &mut TickEvents,
    ) {
        let (weapon, cause) = match source {
            BlastSource::Weapon(weapon) => (Some(weapon), Cause::Weapon(weapon)),
            BlastSource::Prop => (None, Cause::Explosion),
            BlastSource::Map => (None, Cause::Hazard),
        };
        events.explosions.push(ExplosionEvent {
            position: center,
            radius: explosion.radius,
            weapon,
        });
        for reach in blast::props(center, explosion, &self.physics, &self.world) {
            if reach.dynamic {
                let impulse = reach.direction * explosion.force * reach.scale;
                self.physics
                    .apply_impulse(reach.body, impulse, reach.center);
                if let Some(owner) = owner {
                    self.thrown.push((reach.body, owner, self.tick));
                }
            }
            if reach.exposed {
                self.hit_prop(
                    reach.body,
                    explosion.damage * reach.scale,
                    Blow::Explosion,
                    owner,
                );
            }
        }
        for (model, scale) in blast::brushes(center, explosion, &self.world, self.logic.movers()) {
            self.brush_damage
                .push((owner, model, explosion.damage * scale));
        }
        let targets = self.targets();
        for reach in blast::players(center, explosion, &targets, &self.world) {
            if let Some(player) = self.player_mut(reach.player) {
                player.state.velocity += reach.direction * explosion.push_speed(reach.scale);
            }
            if explosion.damage <= 0.0 {
                continue;
            }
            let damage = Damage {
                amount: explosion.damage * reach.scale,
                attacker: owner,
                cause,
            };
            self.hurt(reach.player, damage, events);
        }
    }
}
