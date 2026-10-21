//! Particles the game draws itself: blasts, and the screen shake that goes
//! with them.
//!
//! An explosion is a handful of soft sprites made from textures generated
//! here, not the mounted game's: a white flash, a shockwave ring, a fireball
//! of overlapping puffs, smoke that rises and thins, and sparks that fly and
//! bounce on the map. Fire, sparks, flash and ring are added to what is
//! behind them (they glow); smoke is blended over it. Every particle is a
//! quad facing the camera with a size and a colour that change over its
//! life. [`crate::effects`] asks for a `Blast` and this module does the
//! rest; how it looks is `effects.ron`'s `explosion`.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use rl_assets::coords;
use rl_data::ExplosionEffectConfig;
use rl_sim::collision::contents::SOLID;
use rl_sim::physics::BodyId;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::camera::FlyCam;
use crate::net::Game;
use crate::player::Ticked;
use crate::world::MapScoped;

/// Most particles alive at once; blasts ask for no more than fit.
const MAX_PARTICLES: usize = 700;
/// Pixels across a particle texture.
const TEXTURE_SIZE: u32 = 64;
/// Sparks keep this much of their speed bouncing off a surface...
const SPARK_BOUNCE: f32 = 0.45;
/// ...and this much along it.
const SPARK_SLIDE: f32 = 0.7;
/// A spark this slow lies still, metres/s.
const SPARK_REST: f32 = 0.4;
/// How far off a surface a bounced spark is put, metres.
const SPARK_LIFT: f32 = 0.01;
/// Gravity, metres/s².
const GRAVITY: f32 = 9.8;
/// A blast shakes the view out to this many of its radii.
const SHAKE_REACH: f32 = 3.0;
/// How fast the shake dies away, per second.
const SHAKE_DECAY: f32 = 1.6;
/// The view's largest turn at full shake, radians.
const SHAKE_ANGLE: f32 = 0.05;

/// Draws particles.
pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Blast>()
            .init_resource::<Shake>()
            .init_resource::<Burning>()
            .add_systems(Startup, make_looks)
            .add_systems(
                Update,
                (note_fires, burn_props, spawn_blasts, move_particles)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(Update, calm_shake);
    }
}

/// Something blew up: where (Bevy space, metres) and how far its damage
/// reaches (Source units).
#[derive(Message, Debug, Clone, Copy)]
pub(crate) struct Blast {
    pub at: Vec3,
    pub radius: f32,
}

/// How hard the view is shaking, `0..=1`.
#[derive(Resource, Debug, Default)]
pub(crate) struct Shake {
    trauma: f32,
}

impl Shake {
    /// Shakes the view by `amount`, up to the most there is.
    pub(crate) fn add(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    /// The turn to give the view at `seconds`: rolls and sways, rougher the
    /// harder the shake (which is felt by its square, so small ones are
    /// slight).
    pub(crate) fn rotation(&self, seconds: f32) -> Quat {
        let strength = self.trauma * self.trauma * SHAKE_ANGLE;
        if strength <= f32::EPSILON {
            return Quat::IDENTITY;
        }
        Quat::from_euler(
            EulerRot::YXZ,
            (seconds * 37.0).sin() * strength,
            (seconds * 43.0 + 1.3).sin() * strength,
            (seconds * 29.0 + 2.1).sin() * strength * 0.6,
        )
    }
}

fn calm_shake(time: Res<Time>, mut shake: ResMut<Shake>) {
    if shake.trauma > 0.0 {
        shake.trauma = (shake.trauma - SHAKE_DECAY * time.delta_secs()).max(0.0);
    }
}

/// What a particle is, which says how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fire,
    Smoke,
    Spark,
    Ring,
    Flash,
}

impl Kind {
    /// Whether it is added to what is behind it rather than blended over.
    fn glows(self) -> bool {
        self != Self::Smoke
    }
}

/// A particle to make: all in metres and seconds, colours linear RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Spec {
    kind: Kind,
    /// Where, relative to the blast.
    offset: Vec3,
    velocity: Vec3,
    /// Downward acceleration; negative rises.
    gravity: f32,
    /// How fast it slows, per second.
    drag: f32,
    /// Seconds before it appears.
    delay: f32,
    life: f32,
    /// Size at birth and at death.
    size: (f32, f32),
    color: ([f32; 4], [f32; 4]),
    /// Turn, radians/s.
    spin: f32,
}

/// A particle alive.
#[derive(Component, Debug)]
struct Particle {
    kind: Kind,
    velocity: Vec3,
    gravity: f32,
    drag: f32,
    /// Seconds lived; negative while it waits to appear.
    age: f32,
    life: f32,
    size: (f32, f32),
    color: ([f32; 4], [f32; 4]),
    roll: f32,
    spin: f32,
    resting: bool,
    material: Handle<StandardMaterial>,
}

/// The textures and quad every particle is made from.
#[derive(Resource)]
struct Looks {
    quad: Handle<Mesh>,
    puff: Handle<Image>,
    dot: Handle<Image>,
    ring: Handle<Image>,
}

impl Looks {
    fn texture(&self, kind: Kind) -> Handle<Image> {
        match kind {
            Kind::Fire | Kind::Smoke => self.puff.clone(),
            Kind::Spark | Kind::Flash => self.dot.clone(),
            Kind::Ring => self.ring.clone(),
        }
    }
}

fn make_looks(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
) {
    commands.insert_resource(Looks {
        quad: meshes.add(Rectangle::new(1.0, 1.0)),
        puff: images.add(texture(puff_alpha)),
        dot: images.add(texture(dot_alpha)),
        ring: images.add(texture(ring_alpha)),
    });
}

/// A white texture whose opacity at `(x, y)`, each `-1..=1` from the
/// centre, is `alpha`.
fn texture(alpha: fn(f32, f32) -> f32) -> Image {
    let size = TEXTURE_SIZE as usize;
    let mut pixels = Vec::with_capacity(size * size * 4);
    for row in 0..size {
        for column in 0..size {
            let x = (column as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let y = (row as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let opacity = (alpha(x, y).clamp(0.0, 1.0) * 255.0).round() as u8;
            pixels.extend_from_slice(&[255, 255, 255, opacity]);
        }
    }
    Image::new(
        Extent3d {
            width: TEXTURE_SIZE,
            height: TEXTURE_SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn smooth(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A repeatable pseudo-random value in `0..1` for a lattice point.
fn hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    (h & 0xffff) as f32 / 65535.0
}

/// Smooth value noise in `0..1`.
fn noise(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (smooth(0.0, 1.0, x - ix), smooth(0.0, 1.0, y - iy));
    let (ix, iy) = (ix as i32, iy as i32);
    let top = hash(ix, iy) * (1.0 - fx) + hash(ix + 1, iy) * fx;
    let bottom = hash(ix, iy + 1) * (1.0 - fx) + hash(ix + 1, iy + 1) * fx;
    top * (1.0 - fy) + bottom * fy
}

/// A round puff with a ragged edge.
fn puff_alpha(x: f32, y: f32) -> f32 {
    let distance = x.hypot(y);
    let ragged = 0.6 * noise(x * 3.0 + 7.0, y * 3.0) + 0.4 * noise(x * 7.0, y * 7.0 + 3.0);
    smooth(1.0, 0.15, distance + (ragged - 0.5) * 0.45)
}

/// A bright point with a soft halo.
fn dot_alpha(x: f32, y: f32) -> f32 {
    let distance = x.hypot(y);
    (-distance * distance * 7.0).exp() * smooth(1.0, 0.8, distance)
}

/// A thin soft ring.
fn ring_alpha(x: f32, y: f32) -> f32 {
    let distance = x.hypot(y);
    (-((distance - 0.78) / 0.09).powi(2)).exp() * smooth(1.0, 0.9, distance)
}

/// How opaque a particle `t` (`0..=1`) of the way through its life is: it
/// comes in fast and goes out slowly.
fn envelope(t: f32) -> f32 {
    let rise = smooth(0.0, 0.06, t);
    let fall = 1.0 - smooth(0.25, 1.0, t);
    rise * fall
}

fn lerp4(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
}

/// A random direction, leaning up by `lift` (`0` is any way at all).
fn direction(rng: &mut Rng, lift: f32) -> Vec3 {
    loop {
        let v = Vec3::new(
            rng.unit() * 2.0 - 1.0,
            rng.unit() * 2.0 - 1.0,
            rng.unit() * 2.0 - 1.0,
        );
        let length = v.length();
        if (0.05..=1.0).contains(&length) {
            let mut up = v / length;
            up.y = up.y.abs() * lift + up.y * (1.0 - lift);
            return up.normalize_or_zero();
        }
    }
}

/// Between `low` and `high`.
fn between(rng: &mut Rng, low: f32, high: f32) -> f32 {
    low + (high - low) * rng.unit()
}

/// The particles of a blast of `radius` metres.
fn blast_particles(config: &ExplosionEffectConfig, radius: f32, rng: &mut Rng) -> Vec<Spec> {
    let fireball = radius * config.size_per_radius;
    let rise = Vec3::Y * fireball * config.rise;
    let mut specs = Vec::new();
    specs.push(Spec {
        kind: Kind::Flash,
        offset: rise,
        velocity: Vec3::ZERO,
        gravity: 0.0,
        drag: 0.0,
        delay: 0.0,
        life: 0.14,
        size: (fireball * 1.6, fireball * 2.6),
        color: ([1.6, 1.3, 0.8, 1.0], [1.0, 0.5, 0.15, 0.0]),
        spin: 0.0,
    });
    specs.push(Spec {
        kind: Kind::Ring,
        offset: rise * 0.4,
        velocity: Vec3::ZERO,
        gravity: 0.0,
        drag: 0.0,
        delay: 0.0,
        life: 0.4,
        size: (fireball * 0.4, fireball * 3.4),
        color: ([1.2, 0.8, 0.4, 0.9], [0.8, 0.4, 0.15, 0.0]),
        spin: 0.0,
    });
    for _ in 0..config.fireballs {
        let heading = direction(rng, 0.5);
        specs.push(Spec {
            kind: Kind::Fire,
            offset: rise + heading * fireball * 0.25 * rng.unit(),
            velocity: heading * radius * between(rng, 0.4, 1.6),
            gravity: -1.2,
            drag: 2.6,
            delay: 0.0,
            life: config.duration * between(rng, 0.6, 1.1),
            size: (
                fireball * between(rng, 0.35, 0.6),
                fireball * between(rng, 0.9, 1.4),
            ),
            color: ([1.5, 0.85, 0.3, 0.9], [0.5, 0.07, 0.02, 0.0]),
            spin: between(rng, -1.5, 1.5),
        });
    }
    for _ in 0..config.smoke {
        let heading = direction(rng, 0.8);
        let shade = between(rng, 0.08, 0.2);
        specs.push(Spec {
            kind: Kind::Smoke,
            offset: rise * 0.5 + heading * fireball * 0.3 * rng.unit(),
            velocity: heading * radius * between(rng, 0.15, 0.5) + Vec3::Y * radius * 0.25,
            gravity: -0.35,
            drag: 0.9,
            delay: between(rng, 0.05, 0.3),
            life: config.smoke_duration * between(rng, 0.7, 1.1),
            size: (
                fireball * between(rng, 0.4, 0.6),
                fireball * between(rng, 1.2, 1.9),
            ),
            color: (
                [shade, shade * 0.95, shade * 0.9, 0.6],
                [shade * 1.6, shade * 1.6, shade * 1.6, 0.0],
            ),
            spin: between(rng, -0.6, 0.6),
        });
    }
    for _ in 0..config.sparks {
        let heading = direction(rng, 0.6);
        specs.push(Spec {
            kind: Kind::Spark,
            offset: rise * 0.5,
            velocity: heading * radius * between(rng, 1.5, 4.5),
            gravity: GRAVITY,
            drag: 0.3,
            delay: 0.0,
            life: between(rng, 0.5, 1.4),
            size: (0.07, 0.03),
            color: ([1.6, 1.1, 0.45, 1.0], [1.0, 0.25, 0.05, 0.0]),
            spin: 0.0,
        });
    }
    specs
}

/// Turns asked-for blasts into particles and shakes the view.
fn spawn_blasts(
    mut commands: Commands,
    mut blasts: MessageReader<Blast>,
    game: Game,
    looks: Option<Res<Looks>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    live: Query<(), With<Particle>>,
    mut shake: ResMut<Shake>,
    mut rng: Local<Option<Rng>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Some(sim), Some(looks)) = (game.sim(), looks) else {
        blasts.clear();
        return;
    };
    let config = &sim.data().effects.explosion;
    let rng = rng.get_or_insert_with(|| Rng::seeded(&[0x0062_6c61_7374]));
    let mut room = MAX_PARTICLES.saturating_sub(live.iter().count());
    for blast in blasts.read() {
        if let Ok(camera) = camera.single() {
            let reach = blast.radius * SHAKE_REACH * coords::METERS_PER_UNIT;
            let nearness = 1.0 - camera.translation().distance(blast.at) / reach.max(f32::EPSILON);
            shake.add(config.shake * nearness.clamp(0.0, 1.0));
        }
        let radius = blast.radius * coords::METERS_PER_UNIT;
        for spec in blast_particles(config, radius, rng) {
            if room == 0 {
                break;
            }
            room -= 1;
            spawn_particle(&mut commands, &looks, &mut materials, rng, blast.at, &spec);
        }
    }
}

/// Makes the particle `spec` at `at`.
fn spawn_particle(
    commands: &mut Commands,
    looks: &Looks,
    materials: &mut Assets<StandardMaterial>,
    rng: &mut Rng,
    at: Vec3,
    spec: &Spec,
) {
    let material = materials.add(StandardMaterial {
        base_color: Color::NONE,
        base_color_texture: Some(looks.texture(spec.kind)),
        unlit: true,
        double_sided: true,
        cull_mode: None,
        alpha_mode: if spec.kind.glows() {
            AlphaMode::Add
        } else {
            AlphaMode::Blend
        },
        ..default()
    });
    commands.spawn((
        Particle {
            kind: spec.kind,
            velocity: spec.velocity,
            gravity: spec.gravity,
            drag: spec.drag,
            age: -spec.delay,
            life: spec.life,
            size: spec.size,
            color: spec.color,
            roll: rng.unit() * std::f32::consts::TAU,
            spin: spec.spin,
            resting: false,
            material: material.clone(),
        },
        Mesh3d(looks.quad.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(at + spec.offset).with_scale(Vec3::ZERO),
        MapScoped,
    ));
}

/// The props that are on fire, until when (seconds since startup).
#[derive(Resource, Debug, Default)]
struct Burning(HashMap<BodyId, f32>);

/// How often a burning prop gives off a puff of flame, seconds.
const FLAME_INTERVAL: f32 = 0.045;
/// How often it gives off smoke, in flame puffs.
const SMOKE_EVERY: u32 = 3;

/// A tongue of flame rising off a burning prop, with a little flicker.
fn flame_particles(rng: &mut Rng, puffs: u32) -> Vec<Spec> {
    let mut specs = Vec::new();
    for puff in 0..puffs {
        let sway = Vec3::new(rng.unit() - 0.5, 0.0, rng.unit() - 0.5) * 0.25;
        specs.push(Spec {
            kind: Kind::Fire,
            offset: Vec3::Y * 0.35 + sway * 0.6,
            velocity: Vec3::Y * between(rng, 0.8, 1.6) + sway,
            gravity: -1.5,
            drag: 0.6,
            delay: 0.0,
            life: between(rng, 0.45, 0.8),
            size: (between(rng, 0.2, 0.3), between(rng, 0.05, 0.12)),
            color: ([1.4, 0.75, 0.25, 0.9], [0.6, 0.1, 0.02, 0.0]),
            spin: between(rng, -2.0, 2.0),
        });
        if puff % SMOKE_EVERY == 0 {
            specs.push(Spec {
                kind: Kind::Smoke,
                offset: Vec3::Y * 0.7 + sway,
                velocity: Vec3::Y * between(rng, 0.6, 1.2) + sway,
                gravity: -0.3,
                drag: 0.5,
                delay: 0.0,
                life: between(rng, 1.6, 2.6),
                size: (0.25, between(rng, 0.7, 1.1)),
                color: ([0.1, 0.1, 0.1, 0.45], [0.2, 0.2, 0.2, 0.0]),
                spin: between(rng, -0.5, 0.5),
            });
        }
    }
    specs
}

/// Notes the props set on fire; stops noting them once they break.
fn note_fires(
    mut ticks: MessageReader<Ticked>,
    time: Res<Time>,
    game: Game,
    mut burning: ResMut<Burning>,
) {
    let now = time.elapsed_secs();
    for Ticked(events) in ticks.read() {
        for ignition in &events.prop_ignitions {
            burning.0.insert(ignition.body, now + ignition.fuse + 0.5);
        }
    }
    if let Some(sim) = game.sim() {
        burning
            .0
            .retain(|&body, until| *until > now && !sim.physics().is_broken(body));
    }
}

/// Lets flames off the props that are on fire.
fn burn_props(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    burning: Res<Burning>,
    looks: Option<Res<Looks>>,
    live: Query<(), With<Particle>>,
    mut since: Local<f32>,
    mut rng: Local<Option<Rng>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Some(sim), Some(looks)) = (game.sim(), looks) else {
        return;
    };
    *since += time.delta_secs();
    let puffs = (*since / FLAME_INTERVAL) as u32;
    if puffs == 0 || burning.0.is_empty() {
        return;
    }
    *since -= puffs as f32 * FLAME_INTERVAL;
    let rng = rng.get_or_insert_with(|| Rng::seeded(&[0x0066_6c61_6d65]));
    let mut room = MAX_PARTICLES.saturating_sub(live.iter().count());
    for &body in burning.0.keys() {
        let Some(center) = sim.physics().center_of_mass(body) else {
            continue;
        };
        let at = coords::position(center.to_array());
        for spec in flame_particles(rng, puffs.min(3)) {
            if room == 0 {
                return;
            }
            room -= 1;
            spawn_particle(&mut commands, &looks, &mut materials, rng, at, &spec);
        }
    }
}

/// Moves, bounces, sizes and colours every particle; removes the spent.
fn move_particles(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut particles: Query<(Entity, &mut Particle, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    let facing = camera
        .single()
        .map_or(Quat::IDENTITY, GlobalTransform::rotation);
    let world = game.sim().map(rl_sim::simulation::Simulation::world);
    for (entity, mut particle, mut transform) in &mut particles {
        particle.age += dt;
        if particle.age >= particle.life {
            commands.entity(entity).despawn();
            continue;
        }
        if particle.age < 0.0 {
            continue;
        }
        if !particle.resting {
            let slow = (-particle.drag * dt).exp();
            particle.velocity *= slow;
            particle.velocity.y -= particle.gravity * dt;
            let from = transform.translation;
            let mut to = from + particle.velocity * dt;
            if particle.kind == Kind::Spark
                && let Some(world) = world
            {
                to = bounce(&mut particle, world, from, to);
            }
            transform.translation = to;
        }
        let t = particle.age / particle.life;
        let size = particle.size.0 + (particle.size.1 - particle.size.0) * t;
        transform.scale = Vec3::splat(size);
        transform.rotation =
            facing * Quat::from_rotation_z(particle.roll + particle.spin * particle.age);
        let mut color = lerp4(particle.color.0, particle.color.1, t);
        color[3] *= envelope(t);
        if let Some(mut material) = materials.get_mut(&particle.material) {
            material.base_color = Color::linear_rgba(color[0], color[1], color[2], color[3]);
        }
    }
}

/// Moves a spark from `from` to `to`, turning it back off the map if it
/// hits: returns where it ends up.
fn bounce(
    particle: &mut Particle,
    world: &rl_sim::collision::CollisionWorld,
    from: Vec3,
    to: Vec3,
) -> Vec3 {
    let source = |point: Vec3| Vec3::from(coords::to_source_position(point));
    let trace = world.trace_ray(source(from), source(to), SOLID);
    if !trace.hit() || trace.start_solid {
        return to;
    }
    let normal = coords::direction(trace.normal.to_array());
    let along = particle.velocity.dot(normal);
    let sliding = particle.velocity - normal * along;
    particle.velocity = sliding * SPARK_SLIDE - normal * along * SPARK_BOUNCE;
    if particle.velocity.length() < SPARK_REST {
        particle.velocity = Vec3::ZERO;
        particle.gravity = 0.0;
        particle.resting = true;
    }
    coords::position(trace.end.to_array()) + normal * SPARK_LIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ExplosionEffectConfig {
        ExplosionEffectConfig {
            size_per_radius: 0.8,
            rise: 0.35,
            fireballs: 9,
            smoke: 12,
            sparks: 28,
            duration: 0.8,
            smoke_duration: 3.5,
            shake: 0.7,
        }
    }

    #[test]
    fn a_blast_makes_what_its_config_asks_for() {
        let specs = blast_particles(&config(), 6.5, &mut Rng::seeded(&[1]));
        let count = |kind| specs.iter().filter(|spec| spec.kind == kind).count();
        assert_eq!(count(Kind::Flash), 1);
        assert_eq!(count(Kind::Ring), 1);
        assert_eq!(count(Kind::Fire), 9);
        assert_eq!(count(Kind::Smoke), 12);
        assert_eq!(count(Kind::Spark), 28);
        assert!(
            specs
                .iter()
                .all(|spec| spec.life > 0.0 && spec.delay >= 0.0)
        );
    }

    #[test]
    fn fire_glows_and_smoke_does_not_and_smoke_outlasts_fire() {
        assert!(Kind::Fire.glows() && Kind::Spark.glows() && Kind::Ring.glows());
        assert!(!Kind::Smoke.glows());
        let specs = blast_particles(&config(), 6.5, &mut Rng::seeded(&[2]));
        let longest = |kind| {
            specs
                .iter()
                .filter(|spec| spec.kind == kind)
                .map(|spec| spec.life)
                .fold(0.0, f32::max)
        };
        assert!(longest(Kind::Smoke) > longest(Kind::Fire));
    }

    #[test]
    fn particles_fade_in_quickly_and_out_by_their_end() {
        assert_eq!(envelope(0.0), 0.0);
        assert!(envelope(0.1) > 0.9);
        assert!(envelope(0.5) < envelope(0.3));
        assert_eq!(envelope(1.0), 0.0);
    }

    #[test]
    fn textures_are_opaque_in_the_middle_and_clear_at_the_rim() {
        assert!(puff_alpha(0.0, 0.0) > 0.9 && puff_alpha(1.0, 1.0) == 0.0);
        assert!(dot_alpha(0.0, 0.0) > 0.99 && dot_alpha(1.0, 0.0) < 0.01);
        assert!(ring_alpha(0.78, 0.0) > 0.95 && ring_alpha(0.0, 0.0) < 0.01);
        let image = texture(puff_alpha);
        assert_eq!(image.data.as_ref().map(Vec::len), Some(64 * 64 * 4));
    }

    #[test]
    fn shake_is_nothing_at_rest_and_felt_by_its_square() {
        let mut shake = Shake::default();
        assert_eq!(shake.rotation(1.0), Quat::IDENTITY);
        shake.add(0.5);
        let half = shake.rotation(1.0).angle_between(Quat::IDENTITY);
        shake.add(0.5);
        let full = shake.rotation(1.0).angle_between(Quat::IDENTITY);
        assert!(half > 0.0 && full > half * 2.0);
        assert!(full < 0.2, "never more than a few degrees");
        shake.add(5.0);
        assert_eq!(shake.trauma, 1.0);
    }

    #[test]
    fn flames_rise_and_smoke_every_few_puffs() {
        let specs = flame_particles(&mut Rng::seeded(&[4]), 3);
        let fire = specs.iter().filter(|spec| spec.kind == Kind::Fire).count();
        let smoke = specs.iter().filter(|spec| spec.kind == Kind::Smoke).count();
        assert_eq!((fire, smoke), (3, 1));
        assert!(
            specs
                .iter()
                .all(|spec| spec.velocity.y > 0.0 && spec.gravity < 0.0)
        );
    }

    #[test]
    fn random_directions_are_unit_and_can_lean_up() {
        let mut rng = Rng::seeded(&[3]);
        for _ in 0..50 {
            let heading = direction(&mut rng, 1.0);
            assert!((heading.length() - 1.0).abs() < 1e-4);
            assert!(heading.y >= 0.0);
        }
    }
}
