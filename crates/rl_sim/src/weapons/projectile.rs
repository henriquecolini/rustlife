//! Projectiles in flight: rockets, grenades, bolts, charge orbs.
//!
//! A projectile is a small box ([`ProjectileConfig::size`]) moved each tick
//! by its velocity and the share of gravity its data gives it, traced
//! against the map, its props and the players' boxes. What happens next is
//! data too:
//!
//! - touching a **player** always ends it (its `hit_damage` is dealt, then it
//!   bursts if it has an explosion). Its own thrower is never touched: it
//!   starts inside their box;
//! - touching the **map or a prop**, a bouncing projectile rebounds keeping
//!   `bounce` of its speed, and settles once it is slow on a floor; any
//!   other ends there;
//! - a **fuse** bursts it that long after launch, wherever it is;
//! - past its **lifetime** it is simply gone.

use glam::Vec3;
use rl_data::{MissileConfig, ProjectileConfig};
use serde::{Deserialize, Serialize};

use super::{Mode, WeaponId};
use crate::collision::contents::MASK_PROJECTILE;
use crate::collision::{CollisionQuery, Hull};
use crate::combat::Target;
use crate::combat::hitscan::Struck;
use crate::scene::Scene;
use crate::simulation::PlayerId;

/// A bouncing projectile slower than this after a bounce off a floor comes
/// to rest, units/s.
const REST_SPEED: f32 = 40.0;
/// Surfaces whose normal has at least this Z component are floors a
/// projectile can rest on.
const FLOOR_NORMAL_Z: f32 = 0.7;
/// Distance a projectile backs off a surface it hit, so the next trace
/// does not start inside it, units.
const SURFACE_GAP: f32 = 0.25;

/// A projectile in flight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projectile {
    /// Unique among a simulation's projectiles, for clients to follow it.
    pub id: u32,
    /// The weapon that fired it...
    pub weapon: WeaponId,
    /// ...and with which fire mode (the mode's shot is its data).
    pub mode: Mode,
    /// Who fired it.
    pub owner: PlayerId,
    /// Centre of its box, Source space.
    pub position: Vec3,
    /// Velocity, units/s.
    pub velocity: Vec3,
    /// Seconds since launch.
    pub age: f32,
    /// Lying still on a floor (bouncing projectiles only).
    pub resting: bool,
    /// The way a missile is pointed: the direction it flies once its motor
    /// is lit.
    pub heading: Vec3,
    /// A missile's motor is lit.
    pub lit: bool,
    /// Seconds since the missile last steered.
    pub steer_clock: f32,
    /// The fraction of the world's gravity it falls under, once a ricochet
    /// has changed it.
    pub gravity_scale: Option<f32>,
    /// Seconds it has left stuck in the map, if it has embedded.
    pub stuck: Option<f32>,
    /// Where and against what it last bounced, glanced off or embedded:
    /// `(point, surface normal, material)`, taken by the simulation for
    /// effects.
    pub impact: Option<(Vec3, Vec3, Option<u16>)>,
}

/// How a projectile's tick ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flight {
    /// Still flying (or resting).
    Flying,
    /// Bounced off something this tick.
    Bounced,
    /// It struck something and ends there: at `point`, on a surface facing
    /// along `normal` (zero for a player).
    Struck {
        /// What it struck.
        struck: Struck,
        /// Where.
        point: Vec3,
        /// The surface's normal.
        normal: Vec3,
    },
    /// Its fuse ran out: it bursts where it is.
    Fused,
    /// It outlived its lifetime and is gone.
    Expired,
}

impl Projectile {
    /// Advances the projectile by one tick of `dt` seconds under `gravity`
    /// (the world's, units/s²), through `scene` and the players' boxes. A
    /// missile steers towards `dot`, where its owner's laser is, if there
    /// is one.
    pub fn advance(
        &mut self,
        config: &ProjectileConfig,
        gravity: f32,
        dt: f32,
        scene: &Scene,
        targets: &[Target],
        dot: Option<Vec3>,
    ) -> Flight {
        self.age += dt;
        if let Some(missile) = &config.missile {
            self.fly_missile(missile, config.speed, dt, dot);
        }
        if config.fuse.is_some_and(|fuse| self.age >= fuse) {
            return Flight::Fused;
        }
        if self.age >= config.lifetime {
            return Flight::Expired;
        }
        if self.resting {
            if let Some(left) = self.stuck.as_mut() {
                *left -= dt;
                if *left <= 0.0 {
                    return Flight::Expired;
                }
            }
            return Flight::Flying;
        }
        if !self.lit {
            self.velocity.z -= gravity * self.gravity_scale.unwrap_or(config.gravity) * dt;
        }
        let start = self.position;
        let end = start + self.velocity * dt;
        let hull = Hull::new(Vec3::splat(-config.size), Vec3::splat(config.size));
        let map = scene.trace_hull(start, end, &hull, MASK_PROJECTILE);

        let player = targets
            .iter()
            .filter(|target| target.player != self.owner)
            .filter_map(|target| {
                let (fraction, _) = target.sweep(start, end, hull.extents())?;
                Some((fraction, target.player))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((fraction, player)) = player
            && (fraction < map.fraction || !map.hit())
        {
            self.position = start + (end - start) * fraction;
            return Flight::Struck {
                struck: Struck::Player(player),
                point: self.position,
                normal: Vec3::ZERO,
            };
        }

        if !map.hit() {
            self.position = end;
            return Flight::Flying;
        }
        self.position = map.end + map.normal * SURFACE_GAP;
        let struck = crate::combat::hitscan::struck_by(&map);
        if matches!(struck, Struck::World | Struck::Brush(_))
            && let Some(flight) = self.glance_or_embed(config, &map)
        {
            return flight;
        }
        let Some(bounce) = config.bounce else {
            return Flight::Struck {
                struck,
                point: self.position,
                normal: map.normal,
            };
        };
        let normal = map.normal.normalize_or(Vec3::Z);
        self.velocity = (self.velocity - 2.0 * self.velocity.dot(normal) * normal) * bounce;
        if normal.z >= FLOOR_NORMAL_Z && self.velocity.length() < REST_SPEED {
            self.velocity = Vec3::ZERO;
            self.resting = true;
        }
        Flight::Bounced
    }

    /// A hit on the map by a projectile that glances off or embeds: `None`
    /// if it does neither (the hit is then handled as any other).
    fn glance_or_embed(
        &mut self,
        config: &ProjectileConfig,
        map: &crate::collision::Trace,
    ) -> Option<Flight> {
        let normal = map.normal.normalize_or(Vec3::Z);
        let speed = self.velocity.length();
        let direction = self.velocity.normalize_or(Vec3::X);
        let impact = Some((map.end, normal, map.material));
        if let Some(glance) = &config.ricochet {
            let head_on = normal.dot(-direction);
            if head_on < glance.max_dot && speed > glance.min_speed {
                let reflected = direction + 2.0 * normal * head_on;
                self.velocity = reflected.normalize_or(direction) * speed * glance.speed_scale;
                self.gravity_scale = Some(glance.gravity);
                self.impact = impact;
                return Some(Flight::Bounced);
            }
        }
        let stay = config.stick?;
        self.velocity = Vec3::ZERO;
        self.heading = direction;
        self.resting = true;
        self.stuck = Some(stay);
        self.impact = impact;
        Some(Flight::Flying)
    }

    /// A missile's motor and steering: before the motor lights it only
    /// falls; once lit it flies along its heading at `speed`, which bends
    /// towards the laser dot by the missile's `steer` fraction every
    /// `steer_interval`.
    fn fly_missile(&mut self, missile: &MissileConfig, speed: f32, dt: f32, dot: Option<Vec3>) {
        if self.age < missile.ignite_delay {
            return;
        }
        self.lit = true;
        self.steer_clock += dt;
        while self.steer_clock >= missile.steer_interval {
            self.steer_clock -= missile.steer_interval;
            if let Some(dot) = dot
                && let Some(towards) = (dot - self.position).try_normalize()
            {
                let bent = self.heading * (1.0 - missile.steer) + towards * missile.steer;
                self.heading = bent.try_normalize().unwrap_or(self.heading);
            }
        }
        self.velocity = self.heading * speed;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::collision::{CollisionWorld, WorldBuilder};
    use crate::physics::PhysicsWorld;

    const DT: f32 = 1.0 / 66.0;

    fn config() -> ProjectileConfig {
        ProjectileConfig {
            model: "m.mdl".to_owned(),
            speed: 1000.0,
            lift: 0.0,
            gravity: 0.0,
            size: 2.0,
            bounce: None,
            fuse: None,
            hit_damage: 10.0,
            explosion: None,
            lifetime: 5.0,
            dissolves: false,
            ricochet: None,
            stick: None,
            glow: None,
            impact_force: 0.0,
            missile: None,
            crouched: None,
            body: None,
        }
    }

    /// A floor at z = 0 and a wall at x = 200.
    fn room() -> (Arc<CollisionWorld>, PhysicsWorld) {
        let world = Arc::new(
            WorldBuilder::new()
                .solid_box(
                    Vec3::new(-1000.0, -1000.0, -64.0),
                    Vec3::new(1000.0, 1000.0, 0.0),
                )
                .solid_box(
                    Vec3::new(200.0, -1000.0, 0.0),
                    Vec3::new(264.0, 1000.0, 500.0),
                )
                .build(),
        );
        let physics = PhysicsWorld::new(
            &world,
            &rl_data::GameData::builtin().expect("data").physics,
            DT,
        );
        (world, physics)
    }

    fn projectile(position: Vec3, velocity: Vec3) -> Projectile {
        Projectile {
            id: 0,
            weapon: WeaponId::from_index(0),
            mode: Mode::Primary,
            owner: PlayerId::from_index(0),
            position,
            velocity,
            age: 0.0,
            resting: false,
            heading: velocity.normalize_or_zero(),
            lit: false,
            steer_clock: 0.0,
            gravity_scale: None,
            stuck: None,
            impact: None,
        }
    }

    fn fly(
        projectile: &mut Projectile,
        config: &ProjectileConfig,
        scene: &Scene,
        targets: &[Target],
        ticks: usize,
    ) -> Vec<Flight> {
        (0..ticks)
            .map(|_| projectile.advance(config, 800.0, DT, scene, targets, None))
            .collect()
    }

    #[test]
    fn a_rocket_ends_at_the_wall() {
        let (world, physics) = room();
        let scene = Scene {
            world: &world,
            physics: &physics,
            ignore: None,
            others: &[],
        };
        let mut rocket = projectile(Vec3::new(0.0, 0.0, 50.0), Vec3::new(1000.0, 0.0, 0.0));
        let flights = fly(&mut rocket, &config(), &scene, &[], 30);
        let Some(Flight::Struck {
            struck,
            point,
            normal,
        }) = flights.into_iter().find(|f| *f != Flight::Flying)
        else {
            panic!("the rocket never hit");
        };
        assert_eq!(struck, Struck::World);
        assert_eq!(normal, Vec3::NEG_X);
        assert!((point.x - (200.0 - 2.0)).abs() < 1.0, "{point}");
    }

    #[test]
    fn players_are_struck_but_never_the_thrower() {
        let (world, physics) = room();
        let scene = Scene {
            world: &world,
            physics: &physics,
            ignore: None,
            others: &[],
        };
        let box_at = |index, x: f32| Target {
            player: PlayerId::from_index(index),
            mins: Vec3::new(x - 16.0, -16.0, 0.0),
            maxs: Vec3::new(x + 16.0, 16.0, 72.0),
        };
        let targets = [box_at(0, 0.0), box_at(1, 100.0)];
        let mut bolt = projectile(Vec3::new(0.0, 0.0, 50.0), Vec3::new(1000.0, 0.0, 0.0));
        let flights = fly(&mut bolt, &config(), &scene, &targets, 30);
        assert!(flights.iter().any(|flight| matches!(
            flight,
            Flight::Struck {
                struck: Struck::Player(player),
                ..
            } if *player == PlayerId::from_index(1)
        )));
    }

    #[test]
    fn grenades_bounce_settle_and_burst_on_their_fuse() {
        let (world, physics) = room();
        let scene = Scene {
            world: &world,
            physics: &physics,
            ignore: None,
            others: &[],
        };
        let grenade = ProjectileConfig {
            gravity: 1.0,
            bounce: Some(0.4),
            fuse: Some(2.0),
            ..config()
        };
        let mut frag = projectile(Vec3::new(0.0, 0.0, 40.0), Vec3::new(300.0, 0.0, 0.0));
        let flights = fly(&mut frag, &grenade, &scene, &[], 200);
        assert!(flights.contains(&Flight::Bounced));
        assert!(frag.resting, "came to rest");
        assert!(
            frag.position.z > 0.0 && frag.position.z < 4.0,
            "{}",
            frag.position
        );
        // The fuse runs out on the tick its age reaches 2 s (summed ticks
        // may land one tick either side of the exact quotient).
        let fused = flights
            .iter()
            .position(|f| *f == Flight::Fused)
            .expect("fused");
        assert!((131..=133).contains(&(fused + 1)), "{fused}");
    }

    #[test]
    fn projectiles_expire() {
        let (world, physics) = room();
        let scene = Scene {
            world: &world,
            physics: &physics,
            ignore: None,
            others: &[],
        };
        let short = ProjectileConfig {
            lifetime: 0.1,
            ..config()
        };
        let mut up = projectile(Vec3::new(0.0, 0.0, 40.0), Vec3::new(0.0, 0.0, 100.0));
        assert!(fly(&mut up, &short, &scene, &[], 10).contains(&Flight::Expired));
    }
}
