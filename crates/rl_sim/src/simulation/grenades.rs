//! Grenades: projectiles that are rigid bodies.
//!
//! A projectile whose weapon gives it a `body` is not flown as a box but
//! thrown into the physics world like any prop: it tumbles, bounces off
//! walls, floors, props and players, rolls to rest, and is shoved by
//! blasts, until its fuse runs out and it explodes. Like every body it is
//! simulated only on the server; clients are sent each live grenade's pose
//! ([`GrenadeView`]) with every snapshot and draw it where it is.

use glam::{Quat, Vec3};
use rl_data::{ProjectileBodyConfig, ProjectileConfig};
use serde::{Deserialize, Serialize};

use super::{BlastSource, PlayerId, Simulation, TickEvents};
use crate::collision::ConvexHull;
use crate::movement::view_direction;
use crate::physics::{BodyId, Motion, PropDesc};
use crate::rng::Rng;
use crate::tools::manipulator::Aim;
use crate::weapons::{Mode, WeaponId};

/// How far in front of the eyes a grenade leaves the hand, units.
const RELEASE_DISTANCE: f32 = 14.0;
/// Density of the metal a grenade is made of, kg/m³.
const DENSITY: f32 = 7000.0;
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
}

/// A grenade as clients draw it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GrenadeView {
    /// Which, so a client can follow it from snapshot to snapshot.
    pub id: u32,
    /// The weapon thrown (which names its model).
    pub weapon: WeaponId,
    /// The fire mode thrown with.
    pub mode: Mode,
    /// Where it is, Source space.
    pub position: Vec3,
    /// How it is turned.
    pub rotation: Quat,
    /// Seconds since it left the hand.
    pub age: f32,
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
        let forward = view_direction(aim.pitch, aim.yaw);
        let Some(thrower) = self.player(owner) else {
            return;
        };
        let (thrower_body, inherited) = (thrower.body, thrower.state.velocity);
        let mut rng = Rng::seeded(&[self.tick, u64::from(self.next_projectile)]);
        let radius = projectile.size.max(1.0);
        let desc = PropDesc {
            pieces: vec![ConvexHull::ball(radius)],
            origin: aim.eye + forward * RELEASE_DISTANCE,
            rotation: Quat::from_euler(
                glam::EulerRot::XYZ,
                rng.unit() * std::f32::consts::TAU,
                rng.unit() * std::f32::consts::TAU,
                0.0,
            ),
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
            surface: "grenade".to_owned(),
        };
        let handle = self.physics.add_prop(&desc);
        // It leaves the thrower's hand without bumping them.
        self.physics.set_held(handle, Some(thrower_body));
        self.physics.set_held(handle, None);
        let velocity = forward * projectile.speed + Vec3::Z * projectile.lift + inherited;
        let spin =
            Vec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5) * body.spin * 2.0;
        self.physics.set_velocity(handle, velocity, spin);
        self.grenades.push(Grenade {
            id: self.next_projectile,
            body: handle,
            weapon,
            mode,
            owner,
            age: 0.0,
        });
        self.next_projectile = self.next_projectile.wrapping_add(1);
        self.refresh_grenade_views();
    }

    /// Runs the fuses: a grenade whose time is up explodes where it lies
    /// and leaves the world.
    pub(super) fn advance_grenades(&mut self, events: &mut TickEvents) {
        if self.grenades.is_empty() {
            return;
        }
        let arsenal = std::sync::Arc::clone(&self.arsenal);
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
            let burst = config.fuse.is_some_and(|fuse| grenade.age >= fuse);
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
                    weapon: grenade.weapon,
                    mode: grenade.mode,
                    position: self.physics.center_of_mass(grenade.body)?,
                    rotation: pose.rotation,
                    age: grenade.age,
                })
            })
            .collect();
    }
}
