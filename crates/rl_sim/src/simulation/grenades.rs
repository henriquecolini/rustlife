//! Grenades: projectiles that are rigid bodies.
//!
//! A projectile whose weapon gives it a `body` is not flown as a box but
//! thrown into the physics world like any prop: it tumbles, bounces off
//! walls, floors, props and players, rolls to rest, and is shoved by
//! blasts, until its fuse runs out and it explodes. Its collision shape is a
//! cylinder about its centre of mass, as the game's grenade is. A grenade
//! is thrown overhand, or rolled along the floor on its side. Like every
//! body it is simulated only on the server; clients are sent each live
//! grenade's pose ([`GrenadeView`]) with every snapshot and draw it where
//! it is.

use glam::{Quat, Vec3};
use rl_data::{ProjectileBodyConfig, ProjectileConfig};
use serde::{Deserialize, Serialize};

use super::{BlastSource, PlayerId, Simulation, TickEvents};
use crate::collision::{ConvexHull, Hull};
use crate::combat::hitscan::{self, Struck};
use crate::movement::view_direction;
use crate::physics::{BodyId, Motion, PropDesc};
use crate::rng::Rng;
use crate::scene::Scene;
use crate::tools::manipulator::Aim;
use crate::weapons::{Mode, WeaponId};

/// How far in front of the eyes a grenade leaves the hand, units.
const RELEASE_DISTANCE: f32 = 14.0;
/// How far ahead of a player a rolled grenade is set down, units: just
/// beyond their box.
const ROLL_DISTANCE: f32 = 24.0;
/// Flat faces of a grenade's cylinder.
const CYLINDER_SIDES: u16 = 12;
/// Density of the metal a grenade is made of, kg/m³.
const DENSITY: f32 = 7000.0;
/// How far from the eyes a tripmine can be set on a wall, units.
const ATTACH_REACH: f32 = 128.0;
/// A tripmine's beam starts beyond the mine's own body, units.
const BEAM_START: f32 = 4.0;
/// A grenade this far below the world is lost.
const VOID_Z: f32 = -16384.0;

/// A grenade in flight, on the server.
#[derive(Debug, Clone)]
pub(super) struct Grenade {
    id: u32,
    body: BodyId,
    weapon: WeaponId,
    mode: Mode,
    owner: PlayerId,
    /// Seconds since it left the hand.
    age: f32,
    /// Where a tripmine's beam ends, if it has one and is armed.
    beam_end: Option<Vec3>,
}

/// A grenade as clients draw it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GrenadeView {
    /// Which, so a client can follow it from snapshot to snapshot.
    pub id: u32,
    /// Its physics body, which the impacts and scrapes it makes name.
    pub body: BodyId,
    /// The weapon thrown (which names its model).
    pub weapon: WeaponId,
    /// The fire mode thrown with.
    pub mode: Mode,
    /// Where its centre is, Source space.
    pub position: Vec3,
    /// How it is turned.
    pub rotation: Quat,
    /// Seconds since it left the hand.
    pub age: f32,
    /// Where an armed tripmine's beam ends (it starts at `position`).
    pub beam_end: Option<Vec3>,
}

impl Simulation {
    /// Throws a grenade from the eyes along the view.
    pub(super) fn throw_grenade(
        &mut self,
        owner: PlayerId,
        weapon: WeaponId,
        mode: Mode,
        projectile: &ProjectileConfig,
        body: &ProjectileBodyConfig,
        aim: &Aim,
    ) {
        if body.fixed {
            self.place_mine(owner, weapon, mode, projectile, body, aim);
            return;
        }
        let forward = view_direction(aim.pitch, aim.yaw);
        let Some(thrower) = self.player(owner) else {
            return;
        };
        let (thrower_body, inherited, feet) =
            (thrower.body, thrower.state.velocity, thrower.state.origin);
        let mut rng = Rng::seeded(&[self.tick, u64::from(self.next_projectile)]);
        let (origin, direction, rotation, spin) = if body.rolled {
            let along = forward.with_z(0.0).try_normalize().unwrap_or(Vec3::X);
            let origin = (feet + along * ROLL_DISTANCE).with_z(feet.z + body.radius + 0.5);
            // On its side, turning forward as it runs along the floor.
            let across = Vec3::Z.cross(along);
            let rotation = Quat::from_rotation_arc(Vec3::Z, across);
            (
                origin,
                along,
                rotation,
                across * (projectile.speed / body.radius),
            )
        } else {
            let tumble = Vec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5);
            let rotation = Quat::from_euler(
                glam::EulerRot::XYZ,
                rng.unit() * std::f32::consts::TAU,
                rng.unit() * std::f32::consts::TAU,
                0.0,
            );
            (
                aim.eye + forward * RELEASE_DISTANCE,
                forward,
                rotation,
                tumble * body.spin * 2.0,
            )
        };
        let desc = PropDesc {
            pieces: vec![ConvexHull::cylinder(
                body.radius,
                body.height,
                CYLINDER_SIDES,
            )],
            origin,
            rotation,
            motion: Motion::Dynamic,
            mass: body.mass,
            material: rl_data::MaterialConfig {
                friction: body.friction,
                restitution: projectile.bounce.unwrap_or(0.4),
                density: DENSITY,
            },
            linear_damping: body.damping,
            angular_damping: body.damping,
            debris: true,
            start_asleep: false,
            grabbable: false,
            free_on_grab: false,
            breakable: None,
            surface: body.surface.to_ascii_lowercase(),
        };
        let handle = self.physics.add_prop(&desc);
        // It leaves the thrower's hand without bumping them.
        self.physics.set_held(handle, Some(thrower_body));
        self.physics.set_held(handle, None);
        let velocity = direction * projectile.speed + Vec3::Z * projectile.lift + inherited;
        self.physics.set_velocity(handle, velocity, spin);
        self.grenades.push(Grenade {
            id: self.next_projectile,
            body: handle,
            weapon,
            mode,
            owner,
            age: 0.0,
            beam_end: None,
        });
        self.next_projectile = self.next_projectile.wrapping_add(1);
        self.refresh_grenade_views();
    }

    /// Sets a mine (a fixed body with a beam) on the wall the eyes look at,
    /// if one is within reach; otherwise the shot is not made and its ammo
    /// is given back.
    fn place_mine(
        &mut self,
        owner: PlayerId,
        weapon: WeaponId,
        mode: Mode,
        projectile: &ProjectileConfig,
        body: &ProjectileBodyConfig,
        aim: &Aim,
    ) {
        let forward = view_direction(aim.pitch, aim.yaw);
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
            others: &[],
        };
        let reach = hitscan::trace(
            &scene,
            &[],
            owner,
            aim.eye,
            aim.eye + forward * ATTACH_REACH,
            &Hull::POINT,
        );
        if !matches!(reach.struck, Struck::World | Struck::Brush(_)) {
            self.refund(owner, weapon, mode);
            return;
        }
        let normal = reach.normal.try_normalize().unwrap_or(Vec3::Z);
        let desc = PropDesc {
            pieces: vec![ConvexHull::cylinder(
                body.radius,
                body.height,
                CYLINDER_SIDES,
            )],
            origin: reach.end + normal * body.height * 0.5,
            rotation: Quat::from_rotation_arc(Vec3::Z, normal),
            motion: Motion::Fixed,
            mass: body.mass,
            material: rl_data::MaterialConfig {
                friction: body.friction,
                restitution: projectile.bounce.unwrap_or(0.4),
                density: DENSITY,
            },
            linear_damping: 0.0,
            angular_damping: 0.0,
            debris: true,
            start_asleep: true,
            grabbable: false,
            free_on_grab: false,
            breakable: None,
            surface: body.surface.to_ascii_lowercase(),
        };
        let handle = self.physics.add_prop(&desc);
        self.grenades.push(Grenade {
            id: self.next_projectile,
            body: handle,
            weapon,
            mode,
            owner,
            age: 0.0,
            beam_end: None,
        });
        self.next_projectile = self.next_projectile.wrapping_add(1);
        self.refresh_grenade_views();
    }

    /// Gives back the ammo of a shot that was not made.
    fn refund(&mut self, owner: PlayerId, weapon: WeaponId, mode: Mode) {
        let arsenal = std::sync::Arc::clone(&self.arsenal);
        let Some(rl_data::FireAmmo::Reserve { ammo, count }) =
            arsenal.fire(weapon, mode).map(|fire| fire.ammo.clone())
        else {
            return;
        };
        if let (Some(ammo), Some(player)) = (arsenal.find_ammo(&ammo), self.player_mut(owner)) {
            player.inventory.give_ammo(&arsenal, ammo, count);
        }
    }

    /// A SLAM's alternate button: detonates the player's satchels if any are
    /// out, otherwise switches the SLAM between its modes.
    pub(super) fn slam_alt(&mut self, owner: PlayerId) {
        let arsenal = std::sync::Arc::clone(&self.arsenal);
        let remote = |grenade: &Grenade| {
            grenade.owner == owner
                && matches!(
                    arsenal.fire(grenade.weapon, grenade.mode).map(|fire| &fire.shot),
                    Some(rl_data::Shot::Projectile(config))
                        if config.body.as_ref().is_some_and(|body| body.remote)
                )
        };
        if self.grenades.iter().any(remote) {
            self.detonations.push(owner);
        } else if let Some(player) = self.player_mut(owner) {
            player.inventory.toggle_slam();
        }
    }

    /// Runs the fuses: a grenade whose time is up explodes where it lies
    /// and leaves the world.
    pub(super) fn advance_grenades(&mut self, events: &mut TickEvents) {
        if self.grenades.is_empty() {
            return;
        }
        let arsenal = std::sync::Arc::clone(&self.arsenal);
        let targets = self.targets();
        let scene = Scene {
            world: &self.world,
            physics: &self.physics,
            ignore: None,
            others: &[],
        };
        let detonations = std::mem::take(&mut self.detonations);
        let mut gone = Vec::new();
        for grenade in &mut self.grenades {
            grenade.age += self.dt;
            let Some(rl_data::Shot::Projectile(config)) = arsenal
                .fire(grenade.weapon, grenade.mode)
                .map(|fire| &fire.shot)
            else {
                gone.push(grenade.id);
                continue;
            };
            let position = self
                .physics
                .center_of_mass(grenade.body)
                .unwrap_or(Vec3::ZERO);
            let mut burst = config.fuse.is_some_and(|fuse| grenade.age >= fuse);
            if let Some(body) = &config.body {
                burst |= body.remote && detonations.contains(&grenade.owner);
                if let Some(beam) = &body.beam
                    && grenade.age >= beam.arm_time
                    && let Some(pose) = self.physics.pose(grenade.body)
                {
                    let axis = pose.rotation * Vec3::Z;
                    // No player's box is the owner's own here: the beam
                    // catches everyone.
                    let nobody = PlayerId::from_index(usize::from(u16::MAX));
                    let trace = hitscan::trace(
                        &scene,
                        &targets,
                        nobody,
                        position + axis * BEAM_START,
                        position + axis * beam.length,
                        &Hull::POINT,
                    );
                    grenade.beam_end = Some(trace.end);
                    burst |= matches!(trace.struck, Struck::Player(_) | Struck::Prop(_));
                }
            }
            if burst || grenade.age >= config.lifetime || position.z < VOID_Z {
                if burst && let Some(explosion) = &config.explosion {
                    let (owner, weapon) = (grenade.owner, grenade.weapon);
                    self.pending_blasts
                        .push((owner, weapon, position, *explosion));
                }
                gone.push(grenade.id);
            }
        }
        let removed: Vec<Grenade> = self
            .grenades
            .iter()
            .filter(|grenade| gone.contains(&grenade.id))
            .cloned()
            .collect();
        self.grenades.retain(|grenade| !gone.contains(&grenade.id));
        for grenade in removed {
            self.physics.break_prop(grenade.body);
        }
        for (owner, weapon, position, explosion) in std::mem::take(&mut self.pending_blasts) {
            self.explode(
                Some(owner),
                BlastSource::Weapon(weapon),
                position,
                &explosion,
                events,
            );
        }
        self.refresh_grenade_views();
    }

    /// The surface property of a body: a grenade's from its weapon's data
    /// (clients have no body for it, only its [`GrenadeView`]), a prop's
    /// from the physics world. Empty if unknown.
    #[must_use]
    pub fn surface_of(&self, body: BodyId) -> &str {
        let grenade = self.grenade_views.iter().find(|view| view.body == body);
        let surface = grenade.and_then(|grenade| {
            match self.arsenal.fire(grenade.weapon, grenade.mode)?.shot {
                rl_data::Shot::Projectile(ref projectile) => {
                    projectile.body.as_ref().map(|body| body.surface.as_str())
                }
                _ => None,
            }
        });
        surface.unwrap_or_else(|| self.physics.surface(body))
    }

    /// Takes every grenade out of the world (a new match begins).
    pub(super) fn clear_grenades(&mut self) {
        for grenade in std::mem::take(&mut self.grenades) {
            self.physics.break_prop(grenade.body);
        }
        self.grenade_views.clear();
    }

    /// Records where every live grenade is, for snapshots.
    pub(super) fn refresh_grenade_views(&mut self) {
        self.grenade_views = self
            .grenades
            .iter()
            .filter_map(|grenade| {
                let pose = self.physics.pose(grenade.body)?;
                Some(GrenadeView {
                    id: grenade.id,
                    body: grenade.body,
                    weapon: grenade.weapon,
                    mode: grenade.mode,
                    position: self.physics.center_of_mass(grenade.body)?,
                    rotation: pose.rotation,
                    age: grenade.age,
                    beam_end: grenade.beam_end,
                })
            })
            .collect();
    }
}
