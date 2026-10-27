//! HL2DM's explosion, rebuilt from the game's own code.
//!
//! The client draws a blast with `C_BaseExplosionEffect` (`fx_explosion`),
//! not with a particle system: a handful of sprite puffs of the game's
//! `effects/fire_cloud` and `fire_embers` materials, launched hard and
//! braked at once. The numbers here (counts, lifetimes, sizes, speeds) and
//! the per-frame rules are read from `client.so`; where something is an
//! approximation it is said so.
//!
//! Everything is in Source units, as the engine has it, until a puff is
//! drawn. A puff's `size` is a half-size: the engine's quad runs from `-size`
//! to `+size`. The group sizes are fixed: a bigger blast does not draw a
//! bigger fireball, it only damages farther.
//!
//! What a puff does each frame (`CExplosionParticle`):
//! - its speed is multiplied by `exp(-18.42 dt)` (about a twentieth of a
//!   second to lose most of it), but never drops below 32 units/s, so a puff
//!   brakes hard and then drifts on in its direction;
//! - it grows linearly from its start size to its end size;
//! - its opacity and its brightness both follow `(1 - t)²`, `t` being how
//!   far through its life it is;
//! - its roll speed decays by `1 - 8 dt` but never below half a radian a
//!   second;
//! - it fades out within 128 units of the camera's plane, and is gone
//!   within 64.

use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_sim::collision::CollisionWorld;
use rl_sim::collision::contents::SOLID;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::camera::FlyCam;
use crate::world::MapScoped;

/// How fast a puff's speed decays, per second (`exp(-18.42 dt)`).
const DRAG: f32 = 18.420_7;
/// A puff never goes slower than this, units/s.
const MIN_SPEED: f32 = 32.0;
/// How far from a blast a wall is looked for, units. The engine traces as
/// far as the effect's radius; where that comes from was not traced, so this
/// is a stand-in.
const WALL_REACH: f32 = 128.0;
/// Puffs nearer the camera's plane than this are not drawn, units...
const NEAR_CLIP: f32 = 64.0;
/// ...and ones farther than this are drawn in full.
const NEAR_FADE_END: f32 = 128.0;
/// The slowest a puff's roll turns, radians/s.
const MIN_ROLL_SPEED: f32 = 0.5;
/// How fast a puff's roll slows, per second.
const ROLL_DRAG: f32 = 8.0;
/// Most puffs alive at once.
const MAX_PUFFS: usize = 400;

/// Which of the game's materials a puff is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sprite {
    /// `effects/fire_cloud1`.
    Cloud1,
    /// `effects/fire_cloud2`.
    Cloud2,
    /// `effects/fire_embers1`.
    Embers1,
    /// `effects/fire_embers2`.
    Embers2,
}

impl Sprite {
    /// Every sprite, to load.
    const ALL: [Self; 4] = [Self::Cloud1, Self::Cloud2, Self::Embers1, Self::Embers2];

    fn material(self) -> &'static str {
        match self {
            Self::Cloud1 => "effects/fire_cloud1",
            Self::Cloud2 => "effects/fire_cloud2",
            Self::Embers1 => "effects/fire_embers1",
            Self::Embers2 => "effects/fire_embers2",
        }
    }
}

/// One puff to make, as the engine's `CreateCore` makes it. Source units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Puff {
    /// What it is drawn with.
    pub sprite: Sprite,
    /// Where, relative to the blast.
    pub offset: Vec3,
    /// How it is launched, units/s.
    pub velocity: Vec3,
    /// Seconds it lives.
    pub life: f32,
    /// Half-size at birth and at death.
    pub size: (f32, f32),
    /// Brightness of its grey tint, `0..=1`.
    pub shade: f32,
    /// Starting roll, radians.
    pub roll: f32,
    /// Starting roll speed, radians/s.
    pub roll_speed: f32,
}

/// A number in `low..high`.
fn between(rng: &mut Rng, low: f32, high: f32) -> f32 {
    low + (high - low) * rng.unit()
}

/// A vector whose components are each in `-reach..reach`.
fn random_vector(rng: &mut Rng, reach: f32) -> Vec3 {
    Vec3::new(
        between(rng, -reach, reach),
        between(rng, -reach, reach),
        between(rng, -reach, reach),
    )
}

/// How strongly a launch along `direction` counts, given the blast's
/// `normal`: `0.7 |direction · normal|`, or `1` if either is zero. The
/// engine scales speeds and sizes by it so that a blast throws its puffs
/// out along the open side rather than into the wall.
fn thrust(direction: Vec3, normal: Vec3) -> f32 {
    if direction == Vec3::ZERO || normal == Vec3::ZERO {
        1.0
    } else {
        0.7 * direction.dot(normal).abs()
    }
}

/// Two axes perpendicular to `normal` and to each other.
fn plane_axes(normal: Vec3) -> (Vec3, Vec3) {
    let helper = if normal.x.abs() < 0.9 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let right = normal.cross(helper).normalize_or_zero();
    (right, normal.cross(right).normalize_or_zero())
}

/// The puffs of one blast whose open side faces `normal` (a unit vector;
/// see [`wall_normal`]): four big fire clouds, eight smaller, sixteen
/// embers, thirty-two quick flashes and a ring of thirty-two flung flat.
pub(crate) fn engine_puffs(rng: &mut Rng, normal: Vec3) -> Vec<Puff> {
    let mut puffs = Vec::with_capacity(92);
    // A grey tint, `low..1` of full.
    let shade = |rng: &mut Rng, low: f32| between(rng, low, 1.0);

    // Four huge clouds that fill the middle: 72 growing to 144.
    for _ in 0..4 {
        let direction =
            (random_vector(rng, 0.7) + normal * between(rng, 1.0, 6.0)).normalize_or_zero();
        let speed = 2.0 * between(rng, 1.0, 750.0) * thrust(direction, normal);
        puffs.push(Puff {
            sprite: Sprite::Cloud2,
            offset: Vec3::ZERO,
            velocity: direction * speed,
            life: between(rng, 2.0, 3.0),
            size: (72.0, 144.0),
            shade: shade(rng, 0.5),
            roll: between(rng, 0.0, 360.0),
            roll_speed: between(rng, -2.0, 2.0),
        });
    }
    // Eight smaller, quicker clouds around the centre: 32-64 doubling.
    for _ in 0..8 {
        let direction =
            (random_vector(rng, 0.7) + normal * between(rng, 1.0, 6.0)).normalize_or_zero();
        let speed = 2.0 * between(rng, 1.0, 2000.0) * thrust(direction, normal);
        let start = between(rng, 32.0, 64.0).floor();
        puffs.push(Puff {
            sprite: Sprite::Cloud2,
            offset: random_vector(rng, 16.0),
            velocity: direction * speed,
            life: between(rng, 0.5, 1.0),
            size: (start, 2.0 * start),
            shade: shade(rng, 0.5),
            roll: between(rng, 0.0, 360.0),
            roll_speed: between(rng, -8.0, 8.0),
        });
    }
    // Sixteen embers thrown every way: constant size 4-32.
    for _ in 0..16 {
        let direction = random_vector(rng, 1.4).normalize_or_zero();
        let weight = thrust(direction, normal);
        let speed = between(rng, 1.0, 400.0) * weight * 8.0 * weight;
        let size = (between(rng, 8.0, 16.0).floor() * weight).clamp(4.0, 32.0);
        puffs.push(Puff {
            sprite: if rng.unit() < 0.5 {
                Sprite::Embers1
            } else {
                Sprite::Embers2
            },
            offset: Vec3::ZERO,
            velocity: direction * speed,
            life: between(rng, 2.0, 3.0),
            size: (size, size),
            shade: shade(rng, 0.75),
            roll: between(rng, 0.0, 360.0),
            roll_speed: between(rng, -8.0, 8.0),
        });
    }
    // Thirty-two brief flashes, 0.2-0.4 s, scattered over 96 units.
    for _ in 0..32 {
        let direction = random_vector(rng, 0.525).normalize_or_zero();
        let weight = thrust(direction, normal);
        let speed = between(rng, 400.0, 800.0) * weight * 8.0 * weight;
        let start = (between(rng, 32.0, 85.0).floor() * weight).max(1.0);
        puffs.push(Puff {
            sprite: Sprite::Cloud1,
            offset: random_vector(rng, 48.0),
            velocity: direction * speed,
            life: between(rng, 0.2, 0.4),
            size: (start, (1.5 * start).min(255.0)),
            shade: shade(rng, 0.5),
            roll: between(rng, 0.0, 360.0),
            roll_speed: between(rng, -16.0, 16.0),
        });
    }
    // A ring of thirty-two flung flat along the open side.
    let (right, up) = plane_axes(normal);
    for index in 0..32 {
        let angle = index as f32 * std::f32::consts::PI / 16.0;
        let direction = right * angle.cos() + up * angle.sin();
        let size = between(rng, 16.0, 32.0).floor();
        puffs.push(Puff {
            sprite: Sprite::Cloud2,
            offset: random_vector(rng, 4.0),
            velocity: direction * between(rng, 500.0, 2000.0),
            life: between(rng, 0.5, 1.5),
            size: (size, size),
            shade: shade(rng, 0.5),
            roll: between(rng, 0.0, 360.0),
            roll_speed: between(rng, -8.0, 8.0),
        });
    }
    puffs
}

/// Which way a blast at `origin` is open: the engine looks along the six
/// axes for walls (`wall` answers with how far along `from -> to`, `0..1`,
/// the first solid is, if any) and leans away from each in proportion to how
/// near it is. Straight up when nothing is near.
pub(crate) fn wall_normal(origin: Vec3, wall: impl Fn(Vec3, Vec3) -> Option<f32>) -> Vec3 {
    let mut sum = Vec3::ZERO;
    for axis in [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ] {
        if let Some(fraction) = wall(origin, origin + axis * WALL_REACH) {
            sum += axis * (fraction - 1.0);
        }
    }
    sum.try_normalize().unwrap_or(Vec3::Z)
}

/// A puff's speed after `dt` more seconds of the engine's braking:
/// decayed, but never slower than the drift speed.
pub(crate) fn braked(velocity: Vec3, dt: f32) -> Vec3 {
    let slowed = velocity * (-DRAG * dt).exp();
    if slowed.length_squared() < MIN_SPEED * MIN_SPEED {
        slowed.try_normalize().map_or(Vec3::ZERO, |d| d * MIN_SPEED)
    } else {
        slowed
    }
}

/// Opacity and brightness of a puff `t` of the way through its life.
pub(crate) fn fade(t: f32) -> f32 {
    (1.0 - t).clamp(0.0, 1.0).powi(2)
}

/// The roll and roll speed after `dt`: the speed decays but keeps at least
/// [`MIN_ROLL_SPEED`] in the direction it had.
pub(crate) fn rolled(roll: f32, speed: f32, dt: f32) -> (f32, f32) {
    let decayed = speed + speed * dt * -ROLL_DRAG;
    let kept = if decayed.abs() < MIN_ROLL_SPEED {
        MIN_ROLL_SPEED.copysign(if decayed > 0.0 { 1.0 } else { -1.0 })
    } else {
        decayed
    };
    (roll + speed * dt, kept)
}

/// How much of a puff `depth` units in front of the camera's plane shows:
/// none inside [`NEAR_CLIP`], all beyond [`NEAR_FADE_END`].
pub(crate) fn near_fade(depth: f32) -> f32 {
    ((depth - NEAR_CLIP) / (NEAR_FADE_END - NEAR_CLIP)).clamp(0.0, 1.0)
}

/// Draws explosions.
pub struct ExplosionPlugin;

impl Plugin for ExplosionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Explosion>()
            .init_resource::<Sprites>()
            .add_systems(Startup, load_sprites)
            .add_systems(
                Update,
                (spawn_puffs, move_puffs)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// A blast to draw: where (Bevy space, metres) and which way it is open
/// (Source space).
#[derive(Message, Debug, Clone, Copy)]
pub(crate) struct Explosion {
    pub at: Vec3,
    pub normal: Vec3,
}

/// Where a blast at `at` (Bevy space) is open, looking at `world`.
pub(crate) fn open_side(world: &CollisionWorld, at: Vec3) -> Vec3 {
    let origin = Vec3::from(coords::to_source_position(at));
    wall_normal(origin, |from, to| {
        let trace = world.trace_ray(from, to, SOLID);
        (trace.hit() && !trace.start_solid).then_some(trace.fraction)
    })
}

/// The game's sprites for the puffs.
#[derive(Resource, Default)]
struct Sprites(Vec<(Sprite, Handle<SpriteAsset>)>);

fn load_sprites(mut sprites: ResMut<Sprites>, assets: Res<AssetServer>) {
    sprites.0 = Sprite::ALL
        .iter()
        .map(|&sprite| {
            let path = format!("{SOURCE}://materials/{}.vmt", sprite.material());
            (sprite, assets.load(path))
        })
        .collect();
}

/// A puff alive.
#[derive(Component, Debug)]
struct Alive {
    /// Velocity, Source units/s.
    velocity: Vec3,
    age: f32,
    life: f32,
    size: (f32, f32),
    shade: f32,
    roll: f32,
    roll_speed: f32,
    material: Handle<StandardMaterial>,
}

/// Makes the puffs of each asked-for blast.
fn spawn_puffs(
    mut commands: Commands,
    mut blasts: MessageReader<Explosion>,
    sprites: Res<Sprites>,
    assets: Res<Assets<SpriteAsset>>,
    live: Query<(), With<Alive>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut quad: Local<Option<Handle<Mesh>>>,
    mut rng: Local<Option<Rng>>,
) {
    let quad = quad
        .get_or_insert_with(|| meshes.add(Rectangle::new(1.0, 1.0)))
        .clone();
    let rng = rng.get_or_insert_with(|| Rng::seeded(&[0x0065_7870_6c6f]));
    let mut room = MAX_PUFFS.saturating_sub(live.iter().count());
    for blast in blasts.read() {
        for puff in engine_puffs(rng, blast.normal) {
            let Some(sprite) = sprites
                .0
                .iter()
                .find(|(which, _)| *which == puff.sprite)
                .and_then(|(_, handle)| assets.get(handle))
            else {
                continue;
            };
            if room == 0 {
                return;
            }
            room -= 1;
            let material = materials.add(StandardMaterial {
                base_color: Color::NONE,
                base_color_texture: Some(sprite.image.clone()),
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Add,
                ..default()
            });
            let offset = coords::direction(puff.offset.to_array()) * coords::METERS_PER_UNIT;
            commands.spawn((
                Alive {
                    velocity: puff.velocity,
                    age: 0.0,
                    life: puff.life,
                    size: puff.size,
                    shade: puff.shade,
                    roll: puff.roll,
                    roll_speed: puff.roll_speed,
                    material: material.clone(),
                },
                Mesh3d(quad.clone()),
                MeshMaterial3d(material),
                Transform::from_translation(blast.at + offset).with_scale(Vec3::ZERO),
                MapScoped,
            ));
        }
    }
}

/// Moves, grows, turns and fades every puff; removes the spent.
fn move_puffs(
    mut commands: Commands,
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut puffs: Query<(Entity, &mut Alive, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    let Ok(camera) = camera.single() else {
        return;
    };
    let (_, facing, eye) = camera.to_scale_rotation_translation();
    let forward = camera.forward();
    for (entity, mut puff, mut transform) in &mut puffs {
        puff.age += dt;
        if puff.age >= puff.life {
            commands.entity(entity).try_despawn();
            continue;
        }
        puff.velocity = braked(puff.velocity, dt);
        let step = coords::direction(puff.velocity.to_array()) * coords::METERS_PER_UNIT * dt;
        transform.translation += step;
        let (roll, roll_speed) = rolled(puff.roll, puff.roll_speed, dt);
        puff.roll = roll;
        puff.roll_speed = roll_speed;
        let t = puff.age / puff.life;
        let half = puff.size.0 + (puff.size.1 - puff.size.0) * t;
        transform.scale = Vec3::splat(2.0 * half * coords::METERS_PER_UNIT);
        transform.rotation = facing * Quat::from_rotation_z(puff.roll);
        let depth = (transform.translation - eye).dot(*forward) / coords::METERS_PER_UNIT;
        let brightness = fade(t);
        let alpha = brightness * near_fade(depth);
        if let Some(mut material) = materials.get_mut(&puff.material) {
            let tint = puff.shade * brightness;
            material.base_color = Color::linear_rgba(tint, tint, tint, alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn puffs(normal: Vec3) -> Vec<Puff> {
        engine_puffs(&mut Rng::seeded(&[9]), normal)
    }

    fn of(puffs: &[Puff], sprite: Sprite) -> Vec<Puff> {
        puffs
            .iter()
            .filter(|p| p.sprite == sprite)
            .copied()
            .collect()
    }

    #[test]
    fn a_blast_is_ninety_two_puffs_in_five_groups() {
        let all = puffs(Vec3::Z);
        assert_eq!(all.len(), 92);
        assert_eq!(of(&all, Sprite::Cloud1).len(), 32, "flashes");
        assert_eq!(
            of(&all, Sprite::Cloud2).len(),
            4 + 8 + 32,
            "clouds and ring"
        );
        let embers = of(&all, Sprite::Embers1).len() + of(&all, Sprite::Embers2).len();
        assert_eq!(embers, 16);
    }

    #[test]
    fn the_big_clouds_are_72_growing_to_144_and_live_two_to_three_seconds() {
        let big: Vec<Puff> = puffs(Vec3::Z)
            .into_iter()
            .filter(|p| p.size == (72.0, 144.0))
            .collect();
        assert_eq!(big.len(), 4);
        for p in big {
            assert!((2.0..=3.0).contains(&p.life), "{}", p.life);
            assert_eq!(p.offset, Vec3::ZERO);
        }
    }

    #[test]
    fn every_puff_stays_within_the_engines_ranges() {
        for seed in 0..20 {
            for p in engine_puffs(&mut Rng::seeded(&[seed]), Vec3::Z) {
                assert!(p.life >= 0.2 && p.life <= 3.0, "{p:?}");
                assert!(p.size.0 >= 1.0 && p.size.1 >= p.size.0 && p.size.1 <= 256.0);
                assert!((0.5..=1.0).contains(&p.shade));
                assert!(p.velocity.length() <= 4000.0, "{p:?}");
                assert!(p.velocity.is_finite() && p.offset.is_finite());
                assert!(p.offset.abs().max_element() <= 48.0);
            }
        }
    }

    #[test]
    fn the_ring_is_flat_along_the_open_side_and_goes_all_the_way_round() {
        let normal = Vec3::Z;
        let ring: Vec<Puff> = puffs(normal)
            .into_iter()
            .filter(|p| p.sprite == Sprite::Cloud2 && (p.size.0 - p.size.1).abs() < f32::EPSILON)
            .collect();
        assert_eq!(ring.len(), 32);
        for p in &ring {
            assert!(p.velocity.z.abs() < 1e-2, "{:?}", p.velocity);
            assert!((500.0..=2000.0).contains(&p.velocity.length()));
        }
        let mean: Vec3 = ring.iter().map(|p| p.velocity.normalize()).sum::<Vec3>() / 32.0;
        assert!(mean.length() < 0.1, "balanced all round: {mean}");
    }

    #[test]
    fn clouds_are_thrown_toward_the_open_side() {
        let normal = Vec3::Z;
        let mut up = 0.0;
        let mut down = 0.0;
        for seed in 0..50 {
            for p in engine_puffs(&mut Rng::seeded(&[seed]), normal) {
                if p.size == (72.0, 144.0) {
                    up += p.velocity.z.max(0.0);
                    down += (-p.velocity.z).max(0.0);
                }
            }
        }
        assert!(up > 5.0 * down, "up {up}, down {down}");
    }

    #[test]
    fn thrust_counts_alignment_with_the_normal() {
        assert_eq!(thrust(Vec3::Z, Vec3::Z), 0.7);
        assert_eq!(thrust(Vec3::X, Vec3::Z), 0.0);
        assert_eq!(thrust(Vec3::ZERO, Vec3::Z), 1.0);
        assert_eq!(thrust(Vec3::X, Vec3::ZERO), 1.0);
    }

    #[test]
    fn open_ground_faces_up_and_a_wall_pushes_the_normal_away() {
        assert_eq!(wall_normal(Vec3::ZERO, |_, _| None), Vec3::Z);
        // A wall just along +X.
        let wall = |_: Vec3, to: Vec3| (to.x > 0.0 && to.y == 0.0 && to.z == 0.0).then_some(0.25);
        let normal = wall_normal(Vec3::ZERO, wall);
        assert!(normal.x < -0.99, "{normal}");
        // Floor and a wall: leans away from both.
        let both = |_: Vec3, to: Vec3| {
            let along_x = to.x > 0.0 && to.y == 0.0 && to.z == 0.0;
            let down = to.z < 0.0 && to.x == 0.0;
            (along_x || down).then_some(0.5)
        };
        let normal = wall_normal(Vec3::ZERO, both);
        assert!(normal.x < 0.0 && normal.z > 0.0);
        assert!((normal.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn puffs_brake_hard_then_drift_at_the_minimum_speed() {
        let launched = Vec3::new(1000.0, 0.0, 0.0);
        let after = braked(launched, 0.1);
        assert!(after.length() < 200.0, "{after}");
        let mut v = launched;
        for _ in 0..120 {
            v = braked(v, 1.0 / 60.0);
        }
        assert!((v.length() - MIN_SPEED).abs() < 1e-3, "{v}");
        assert!(v.x > 0.0, "keeps its heading");
        assert_eq!(braked(Vec3::ZERO, 0.1), Vec3::ZERO);
    }

    #[test]
    fn a_puff_fades_by_the_square_of_what_is_left() {
        assert_eq!(fade(0.0), 1.0);
        assert!((fade(0.5) - 0.25).abs() < 1e-6);
        assert_eq!(fade(1.0), 0.0);
        assert_eq!(fade(1.5), 0.0);
    }

    #[test]
    fn rolls_slow_down_but_never_stop() {
        let (mut roll, mut speed) = (0.0, 8.0);
        for _ in 0..600 {
            (roll, speed) = rolled(roll, speed, 1.0 / 60.0);
        }
        assert!((speed - MIN_ROLL_SPEED).abs() < 1e-4, "{speed}");
        assert!(roll > 0.0);
        let (_, backwards) = rolled(0.0, -0.1, 0.01);
        assert_eq!(backwards, -MIN_ROLL_SPEED, "keeps its direction");
    }

    #[test]
    fn puffs_vanish_near_the_camera() {
        assert_eq!(near_fade(0.0), 0.0);
        assert_eq!(near_fade(64.0), 0.0);
        assert!((near_fade(96.0) - 0.5).abs() < 1e-6);
        assert_eq!(near_fade(128.0), 1.0);
        assert_eq!(near_fade(5000.0), 1.0);
        assert_eq!(near_fade(-10.0), 0.0, "behind the camera");
    }

    #[test]
    fn the_same_seed_makes_the_same_blast() {
        assert_eq!(puffs(Vec3::Z), puffs(Vec3::Z));
    }
}
