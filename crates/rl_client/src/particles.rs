//! Particles the game draws itself: blasts, fire, and the shake and flash
//! that go with them.
//!
//! An explosion is built from soft sprites made from textures generated
//! here, not the mounted game's. In the first instants it is a white-hot
//! flash and a shockwave ring; a fireball of puffs bursts out at several
//! times the speed of sound in a game, slows as it climbs and cools from
//! white through orange to soot; black smoke boils up behind it, dust rolls
//! out along the floor, and sparks and embers fly, the sparks as streaks
//! along their motion, bouncing on the map. A prop on fire gives off
//! flames and smoke that grow as its fuse burns down.
//!
//! Fire, sparks, embers, flash and ring are added to what is behind them;
//! smoke and dust are blended over it. Every particle is a quad facing the
//! camera whose size and colour change over its life (colours are HDR-ish:
//! values above one burn to white). [`crate::effects`] asks for a `Blast`
//! and this module does the rest; how big and how many is `effects.ron`'s
//! `explosion`.

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
const MAX_PARTICLES: usize = 900;
/// Pixels across a particle texture.
const TEXTURE_SIZE: u32 = 128;
/// Sparks keep this much of their speed bouncing off a surface...
const SPARK_BOUNCE: f32 = 0.4;
/// ...and this much along it.
const SPARK_SLIDE: f32 = 0.65;
/// A spark this slow lies still, metres/s.
const SPARK_REST: f32 = 0.5;
/// How far off a surface a bounced spark is put, metres.
const SPARK_LIFT: f32 = 0.01;
/// A spark streak is this many seconds of its motion long.
const STREAK_SECONDS: f32 = 0.035;
/// The longest a spark streak gets, metres.
const STREAK_MAX: f32 = 1.2;
/// Gravity, metres/s².
const GRAVITY: f32 = 9.8;
/// A blast shakes the view out to this many of its radii.
const SHAKE_REACH: f32 = 3.0;
/// How fast the shake dies away, per second.
const SHAKE_DECAY: f32 = 1.2;
/// The view's largest turn at full shake, radians.
const SHAKE_ANGLE: f32 = 0.09;
/// A blast whiteens the screen out to this many of its radii.
const FLASH_REACH: f32 = 1.4;
/// How fast the white fades, per second.
const FLASH_DECAY: f32 = 3.2;
/// How far below a blast the floor is looked for, in blast radii, for the
/// dust it kicks up.
const DUST_REACH: f32 = 0.8;
/// Where along its life a particle's gradient passes its middle colour.
const MIDDLE: f32 = 0.35;

/// Draws particles.
pub struct ParticlesPlugin;

impl Plugin for ParticlesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Blast>()
            .init_resource::<Shake>()
            .init_resource::<ScreenFlash>()
            .init_resource::<Burning>()
            .add_systems(Startup, (make_looks, make_flash_overlay))
            .add_systems(
                Update,
                (note_fires, burn_props, spawn_blasts, move_particles)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(Update, (calm_shake, show_flash));
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

/// How white the whole screen is from a blast right beside the player,
/// `0..=1`.
#[derive(Resource, Debug, Default)]
struct ScreenFlash(f32);

/// The white sheet drawn over the view.
#[derive(Component)]
struct FlashOverlay;

fn make_flash_overlay(mut commands: Commands) {
    commands.spawn((
        FlashOverlay,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        BackgroundColor(Color::NONE),
        GlobalZIndex(4),
        Pickable::IGNORE,
    ));
}

fn show_flash(
    time: Res<Time>,
    mut flash: ResMut<ScreenFlash>,
    mut overlay: Query<&mut BackgroundColor, With<FlashOverlay>>,
) {
    flash.0 = (flash.0 - FLASH_DECAY * time.delta_secs()).max(0.0);
    if let Ok(mut color) = overlay.single_mut() {
        let wanted = Color::srgba(1.0, 0.93, 0.8, flash.0 * 0.85);
        if color.0 != wanted {
            color.0 = wanted;
        }
    }
}

/// What a particle is, which says how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fire,
    Smoke,
    Dust,
    /// A streak along its motion.
    Spark,
    /// A glowing speck that drifts.
    Ember,
    Ring,
    Flash,
}

impl Kind {
    /// Whether it is added to what is behind it rather than blended over.
    fn glows(self) -> bool {
        !matches!(self, Self::Smoke | Self::Dust)
    }
}

/// A gradient over a particle's life: its colour at birth, at
/// [`MIDDLE`] and at its end (linear RGBA, values above one burn white).
type Gradient = [[f32; 4]; 3];

/// A particle to make: all in metres and seconds.
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
    /// Size at birth and at death (a streak's width).
    size: (f32, f32),
    color: Gradient,
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
    color: Gradient,
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
    streak: Handle<Image>,
    ring: Handle<Image>,
}

impl Looks {
    fn texture(&self, kind: Kind) -> Handle<Image> {
        match kind {
            Kind::Fire | Kind::Smoke | Kind::Dust => self.puff.clone(),
            Kind::Ember | Kind::Flash => self.dot.clone(),
            Kind::Spark => self.streak.clone(),
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
        puff: images.add(texture(puff_shade)),
        dot: images.add(texture(dot_shade)),
        streak: images.add(texture(streak_shade)),
        ring: images.add(texture(ring_shade)),
    });
}

/// A texture whose brightness and opacity at `(x, y)`, each `-1..=1` from
/// the centre, is `shade`'s `(brightness, opacity)`.
fn texture(shade: fn(f32, f32) -> (f32, f32)) -> Image {
    let size = TEXTURE_SIZE as usize;
    let mut pixels = Vec::with_capacity(size * size * 4);
    for row in 0..size {
        for column in 0..size {
            let x = (column as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let y = (row as f32 + 0.5) / size as f32 * 2.0 - 1.0;
            let (brightness, opacity) = shade(x, y);
            let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            let b = byte(brightness);
            pixels.extend_from_slice(&[b, b, b, byte(opacity)]);
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

/// Four octaves of noise in about `0..1`: cloudy detail at every scale.
fn fbm(x: f32, y: f32) -> f32 {
    let mut sum = 0.0;
    let mut amplitude = 0.5;
    let mut frequency = 1.0;
    for octave in 0..4 {
        sum += amplitude * noise(x * frequency + octave as f32 * 17.3, y * frequency);
        amplitude *= 0.5;
        frequency *= 2.1;
    }
    sum / 0.9375
}

/// A billowing puff: a ragged round cloud, lit from above-left, with
/// swirls of darker and brighter inside it.
fn puff_shade(x: f32, y: f32) -> (f32, f32) {
    let distance = x.hypot(y);
    let warp = fbm(x * 2.2 + 3.0, y * 2.2 + 9.0);
    let edge = distance + (warp - 0.5) * 0.9;
    let opacity = smooth(1.0, 0.2, edge) * (0.75 + 0.25 * fbm(x * 4.0, y * 4.0 + 5.0));
    let swirl = fbm(x * 5.0 + warp * 3.0, y * 5.0 - warp * 3.0);
    let light = 0.5 + 0.5 * ((-x - y) * 0.35 + 0.5).clamp(0.0, 1.0);
    ((0.3 + 0.7 * swirl) * light, opacity)
}

/// A bright point with a soft halo.
fn dot_shade(x: f32, y: f32) -> (f32, f32) {
    let distance = x.hypot(y);
    (
        1.0,
        (-distance * distance * 7.0).exp() * smooth(1.0, 0.8, distance),
    )
}

/// A thin streak along the vertical, brightest in the middle and fading to
/// a point at each end.
fn streak_shade(x: f32, y: f32) -> (f32, f32) {
    let across = (-(x / 0.16).powi(2)).exp();
    let along = smooth(1.0, 0.55, y.abs());
    (1.0, across * along)
}

/// A thin soft ring.
fn ring_shade(x: f32, y: f32) -> (f32, f32) {
    let distance = x.hypot(y);
    (
        1.0,
        (-((distance - 0.8) / 0.08).powi(2)).exp() * smooth(1.0, 0.9, distance),
    )
}

/// How opaque a particle `t` (`0..=1`) of the way through its life is: it
/// comes in at once and is gone by its end.
fn envelope(t: f32) -> f32 {
    smooth(0.0, 0.04, t) * (1.0 - smooth(0.7, 1.0, t))
}

fn lerp4(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
}

/// The colour of a gradient `t` (`0..=1`) of the way through a life.
fn gradient(colors: &Gradient, t: f32) -> [f32; 4] {
    if t < MIDDLE {
        lerp4(colors[0], colors[1], t / MIDDLE)
    } else {
        lerp4(colors[1], colors[2], (t - MIDDLE) / (1.0 - MIDDLE))
    }
}

/// Fire: white-hot, then orange, then dark red and gone.
const FIRE: Gradient = [
    [1.9, 1.05, 0.3, 0.85],
    [1.3, 0.33, 0.04, 0.7],
    [0.3, 0.03, 0.0, 0.0],
];

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

/// A particle that does nothing but sit where it is, for the base of
/// [`Spec`] literals below.
fn base(kind: Kind) -> Spec {
    Spec {
        kind,
        offset: Vec3::ZERO,
        velocity: Vec3::ZERO,
        gravity: 0.0,
        drag: 0.0,
        delay: 0.0,
        life: 1.0,
        size: (1.0, 1.0),
        color: [[0.0; 4]; 3],
        spin: 0.0,
    }
}

/// The particles of a blast of `radius` metres; `floor` is how far below it
/// the floor is, if there is one close enough to kick up dust.
fn blast_particles(
    config: &ExplosionEffectConfig,
    radius: f32,
    floor: Option<f32>,
    rng: &mut Rng,
) -> Vec<Spec> {
    let fireball = radius * config.size_per_radius;
    // Fireballs rise, and one centred on a blast at the floor would be cut
    // in half by it, so it is lifted: but only as far as the floor is near.
    // A blast on a wall or in mid-air is centred where it is.
    let near_floor = floor.map_or(0.0, |drop| 1.0 - smooth(0.0, fireball * 0.8, drop));
    let rise = Vec3::Y * fireball * config.rise * near_floor;
    let mut specs = vec![
        // The white-hot instant.
        Spec {
            offset: rise,
            life: 0.12,
            size: (fireball * 1.1, fireball * 2.4),
            color: [
                [3.0, 2.6, 1.8, 1.0],
                [2.0, 1.2, 0.5, 0.8],
                [1.0, 0.4, 0.1, 0.0],
            ],
            ..base(Kind::Flash)
        },
        // The shockwave.
        Spec {
            offset: rise * 0.4,
            life: 0.32,
            size: (fireball * 0.3, fireball * 4.2),
            color: [
                [1.6, 1.2, 0.8, 0.8],
                [1.0, 0.7, 0.4, 0.4],
                [0.6, 0.4, 0.2, 0.0],
            ],
            ..base(Kind::Ring)
        },
    ];
    let shape = Shape {
        radius,
        fireball,
        rise,
    };
    specs.extend(fireball_specs(config, &shape, rng));
    specs.extend(smoke_specs(config, &shape, rng));
    if let Some(drop) = floor {
        specs.extend(dust_specs(config, &shape, drop, rng));
    }
    specs.extend(spark_specs(config, &shape, rng));
    specs
}

/// How big a blast is, for working out its parts.
#[derive(Clone, Copy)]
struct Shape {
    /// Its radius, metres.
    radius: f32,
    /// The fireball's size, metres.
    fireball: f32,
    /// How far it is lifted off where it happened.
    rise: Vec3,
}

/// The fireball: puffs flung out hard that brake quickly and climb.
fn fireball_specs(config: &ExplosionEffectConfig, shape: &Shape, rng: &mut Rng) -> Vec<Spec> {
    let Shape {
        radius,
        fireball,
        rise,
    } = *shape;
    let mut specs = Vec::new();
    for index in 0..config.fireballs {
        let heading = direction(rng, 0.15);
        let late = index >= config.fireballs / 2;
        specs.push(Spec {
            offset: rise + heading * fireball * 0.12 * rng.unit(),
            velocity: heading * radius * between(rng, 2.0, 5.5)
                + if late {
                    Vec3::Y * radius * 1.2
                } else {
                    Vec3::ZERO
                },
            gravity: -3.0,
            drag: 4.5,
            delay: if late { between(rng, 0.03, 0.12) } else { 0.0 },
            life: config.duration * between(rng, 0.5, 1.0),
            size: (
                fireball * between(rng, 0.3, 0.5),
                fireball * between(rng, 0.8, 1.35),
            ),
            color: FIRE,
            spin: between(rng, -2.0, 2.0),
            ..base(Kind::Fire)
        });
    }
    specs
}

/// Smoke boiling up behind it: hot brown at first, then black soot, then
/// grey and gone.
fn smoke_specs(config: &ExplosionEffectConfig, shape: &Shape, rng: &mut Rng) -> Vec<Spec> {
    let Shape {
        radius,
        fireball,
        rise,
    } = *shape;
    let mut specs = Vec::new();
    for _ in 0..config.smoke {
        let heading = direction(rng, 0.7);
        let soot = between(rng, 0.03, 0.1);
        specs.push(Spec {
            offset: rise * 0.6 + heading * fireball * 0.25 * rng.unit(),
            velocity: heading * radius * between(rng, 0.6, 2.2)
                + Vec3::Y * radius * between(rng, 0.2, 0.7),
            gravity: -0.5,
            drag: 1.7,
            delay: between(rng, 0.06, 0.4),
            life: config.smoke_duration * between(rng, 0.6, 1.0),
            size: (
                fireball * between(rng, 0.45, 0.7),
                fireball * between(rng, 1.5, 2.4),
            ),
            color: [
                [0.5, 0.3, 0.15, 0.7],
                [soot, soot * 0.95, soot * 0.9, 0.8],
                [soot * 3.0, soot * 3.0, soot * 3.0, 0.0],
            ],
            spin: between(rng, -0.7, 0.7),
            ..base(Kind::Smoke)
        });
    }
    specs
}

/// Dust rolling out along a floor `drop` metres below the blast.
fn dust_specs(
    config: &ExplosionEffectConfig,
    shape: &Shape,
    drop: f32,
    rng: &mut Rng,
) -> Vec<Spec> {
    let Shape {
        radius, fireball, ..
    } = *shape;
    let mut specs = Vec::new();
    for _ in 0..(config.smoke / 2).max(6) {
        let around = between(rng, 0.0, std::f32::consts::TAU);
        let outward = Vec3::new(around.cos(), 0.0, around.sin());
        let tan = between(rng, 0.25, 0.42);
        specs.push(Spec {
            offset: Vec3::Y * (0.15 - drop),
            velocity: outward * radius * between(rng, 2.0, 4.0) + Vec3::Y * 0.6,
            gravity: -0.2,
            drag: 3.2,
            delay: between(rng, 0.0, 0.08),
            life: between(rng, 1.1, 1.9),
            size: (fireball * 0.25, fireball * between(rng, 0.9, 1.4)),
            color: [
                [tan, tan * 0.85, tan * 0.65, 0.65],
                [tan * 0.8, tan * 0.7, tan * 0.55, 0.45],
                [tan * 0.8, tan * 0.7, tan * 0.55, 0.0],
            ],
            spin: between(rng, -0.8, 0.8),
            ..base(Kind::Dust)
        });
    }
    specs
}

/// Sparks flung out as streaks, and embers that drift up and settle.
fn spark_specs(config: &ExplosionEffectConfig, shape: &Shape, rng: &mut Rng) -> Vec<Spec> {
    let Shape { radius, rise, .. } = *shape;
    let mut specs = Vec::new();
    for _ in 0..config.sparks {
        let heading = direction(rng, 0.55);
        specs.push(Spec {
            offset: rise * 0.5,
            velocity: heading * radius * between(rng, 3.0, 9.0),
            gravity: GRAVITY,
            drag: 0.2,
            life: between(rng, 0.6, 1.7),
            size: (0.05, 0.03),
            color: [
                [3.0, 2.2, 1.0, 1.0],
                [2.0, 0.9, 0.25, 1.0],
                [0.8, 0.15, 0.03, 0.0],
            ],
            ..base(Kind::Spark)
        });
    }
    // Embers drifting up and settling.
    for _ in 0..(config.sparks / 2) {
        let heading = direction(rng, 0.8);
        specs.push(Spec {
            offset: rise * 0.7,
            velocity: heading * radius * between(rng, 0.8, 2.4),
            gravity: -0.4,
            drag: 1.4,
            delay: between(rng, 0.0, 0.2),
            life: between(rng, 1.5, 3.2),
            size: (0.07, 0.025),
            color: [
                [2.4, 1.3, 0.4, 1.0],
                [1.4, 0.4, 0.08, 0.8],
                [0.4, 0.06, 0.0, 0.0],
            ],
            ..base(Kind::Ember)
        });
    }
    specs
}

/// Fire and smoke off a burning prop, `intensity` from `0` to about `1.5`
/// (it grows as the fuse burns down). Tongues of flame rise in a column
/// and flick about; thick black smoke follows; now and then an ember flies
/// off.
fn flame_particles(rng: &mut Rng, puffs: u32, intensity: f32) -> Vec<Spec> {
    let mut specs = Vec::new();
    for puff in 0..puffs {
        let (x, z) = rng.in_disc();
        let spread = Vec3::new(x, 0.0, z) * 0.2 * intensity;
        let sway = Vec3::new(rng.unit() - 0.5, 0.0, rng.unit() - 0.5) * 0.9;
        let rising = between(rng, 2.2, 3.6) * intensity;
        specs.push(Spec {
            offset: Vec3::Y * 0.45 + spread,
            velocity: Vec3::Y * rising + sway,
            gravity: -2.0,
            drag: 0.8,
            life: between(rng, 0.45, 0.85),
            size: (between(rng, 0.3, 0.45) * intensity, 0.06),
            color: FIRE,
            spin: between(rng, -2.5, 2.5),
            ..base(Kind::Fire)
        });
        // A small white-hot core at the base.
        specs.push(Spec {
            offset: Vec3::Y * 0.4 + spread * 0.5,
            velocity: Vec3::Y * rising * 0.5,
            gravity: -1.0,
            drag: 1.0,
            life: between(rng, 0.2, 0.35),
            size: (0.22 * intensity, 0.05),
            color: [
                [3.0, 2.6, 1.6, 0.9],
                [2.0, 1.2, 0.4, 0.6],
                [1.0, 0.4, 0.1, 0.0],
            ],
            ..base(Kind::Flash)
        });
        if puff % 2 == 0 {
            let soot = between(rng, 0.03, 0.09);
            specs.push(Spec {
                offset: Vec3::Y * 0.9 + spread,
                velocity: Vec3::Y * between(rng, 1.6, 2.8) * intensity + sway * 0.6,
                gravity: -0.5,
                drag: 0.45,
                life: between(rng, 2.4, 4.0),
                size: (0.3 * intensity, between(rng, 1.2, 1.9)),
                color: [
                    [0.35, 0.2, 0.1, 0.55],
                    [soot, soot, soot, 0.75],
                    [soot * 3.0, soot * 3.0, soot * 3.0, 0.0],
                ],
                spin: between(rng, -0.5, 0.5),
                ..base(Kind::Smoke)
            });
        }
        if rng.unit() < 0.18 * intensity {
            specs.push(Spec {
                offset: Vec3::Y * 0.6 + spread,
                velocity: Vec3::new(sway.x, 1.2, sway.z) * between(rng, 1.0, 2.5),
                gravity: 0.5,
                drag: 0.4,
                life: between(rng, 0.8, 1.6),
                size: (0.05, 0.02),
                color: [
                    [2.4, 1.3, 0.4, 1.0],
                    [1.4, 0.4, 0.08, 0.8],
                    [0.4, 0.06, 0.0, 0.0],
                ],
                ..base(Kind::Ember)
            });
        }
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
    mut flash: ResMut<ScreenFlash>,
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
            let distance = camera.translation().distance(blast.at);
            let units = blast.radius * coords::METERS_PER_UNIT;
            let nearness =
                |reach: f32| (1.0 - distance / (units * reach).max(f32::EPSILON)).clamp(0.0, 1.0);
            shake.add(config.shake * nearness(SHAKE_REACH));
            flash.0 = flash.0.max(nearness(FLASH_REACH).powi(2));
        }
        let radius = blast.radius * coords::METERS_PER_UNIT;
        let floor = floor_below(sim.world(), blast.at, radius * DUST_REACH);
        for spec in blast_particles(config, radius, floor, rng) {
            if room == 0 {
                break;
            }
            room -= 1;
            spawn_particle(&mut commands, &looks, &mut materials, rng, blast.at, &spec);
        }
    }
}

/// How far below `at` (Bevy space) the map is, if within `reach` metres.
fn floor_below(world: &rl_sim::collision::CollisionWorld, at: Vec3, reach: f32) -> Option<f32> {
    let source = Vec3::from(coords::to_source_position(at));
    let down = Vec3::Z * reach / coords::METERS_PER_UNIT;
    let trace = world.trace_ray(source, source - down, SOLID);
    (trace.hit() && !trace.start_solid)
        .then_some((source.z - trace.end.z) * coords::METERS_PER_UNIT)
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

/// The props that are on fire: when each caught and when its fuse is spent
/// (seconds since startup).
#[derive(Resource, Debug, Default)]
struct Burning(HashMap<BodyId, (f32, f32)>);

/// How often a burning prop gives off a puff of flame, seconds.
const FLAME_INTERVAL: f32 = 0.03;

/// How fierce a fire is `progress` (`0..=1`) of the way to its fuse's end.
fn fierceness(progress: f32) -> f32 {
    0.7 + 0.8 * progress.clamp(0.0, 1.0)
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
            burning
                .0
                .insert(ignition.body, (now, now + ignition.fuse + 0.5));
        }
    }
    if let Some(sim) = game.sim() {
        burning
            .0
            .retain(|&body, &mut (_, until)| until > now && !sim.physics().is_broken(body));
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
    let now = time.elapsed_secs();
    let mut room = MAX_PARTICLES.saturating_sub(live.iter().count());
    for (&body, &(since_lit, until)) in &burning.0 {
        let Some(center) = sim.physics().center_of_mass(body) else {
            continue;
        };
        let at = coords::position(center.to_array());
        let progress = (now - since_lit) / (until - since_lit).max(f32::EPSILON);
        for spec in flame_particles(rng, puffs.min(3), fierceness(progress)) {
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
            commands.entity(entity).try_despawn();
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
        if particle.kind == Kind::Spark {
            // A streak along where it is going, as the camera sees it.
            let seen = facing.inverse() * particle.velocity;
            let angle = seen.y.atan2(seen.x) - std::f32::consts::FRAC_PI_2;
            let length = (particle.velocity.length() * STREAK_SECONDS).clamp(size, STREAK_MAX);
            transform.scale = Vec3::new(size, length, 1.0);
            transform.rotation = facing * Quat::from_rotation_z(angle);
        } else {
            transform.scale = Vec3::splat(size);
            transform.rotation =
                facing * Quat::from_rotation_z(particle.roll + particle.spin * particle.age);
        }
        let mut color = gradient(&particle.color, t);
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

    fn count(specs: &[Spec], kind: Kind) -> usize {
        specs.iter().filter(|spec| spec.kind == kind).count()
    }

    #[test]
    fn a_blast_makes_what_its_config_asks_for() {
        let specs = blast_particles(&config(), 6.5, None, &mut Rng::seeded(&[1]));
        assert_eq!(count(&specs, Kind::Flash), 1);
        assert_eq!(count(&specs, Kind::Ring), 1);
        assert_eq!(count(&specs, Kind::Fire), 9);
        assert_eq!(count(&specs, Kind::Smoke), 12);
        assert_eq!(count(&specs, Kind::Spark), 28);
        assert_eq!(count(&specs, Kind::Ember), 14);
        assert_eq!(count(&specs, Kind::Dust), 0, "no floor, no dust");
        assert!(
            specs
                .iter()
                .all(|spec| spec.life > 0.0 && spec.delay >= 0.0)
        );
    }

    #[test]
    fn only_a_blast_near_the_floor_is_lifted_off_it() {
        let flash_height = |floor| {
            blast_particles(&config(), 6.5, floor, &mut Rng::seeded(&[1]))
                .iter()
                .find(|spec| spec.kind == Kind::Flash)
                .map(|spec| spec.offset.y)
                .expect("a flash")
        };
        assert!(flash_height(Some(0.0)) > 1.0, "on the floor");
        assert_eq!(flash_height(None), 0.0, "on a wall, centred");
        assert!(flash_height(Some(2.0)) < flash_height(Some(0.2)));
    }

    #[test]
    fn a_floor_close_below_kicks_up_dust_along_it() {
        let specs = blast_particles(&config(), 6.5, Some(1.0), &mut Rng::seeded(&[1]));
        let dust: Vec<_> = specs
            .iter()
            .filter(|spec| spec.kind == Kind::Dust)
            .collect();
        assert!(dust.len() >= 6);
        for spec in dust {
            assert!(spec.offset.y < 0.0, "at the floor, below the blast");
            assert!(spec.velocity.y.abs() < spec.velocity.xz().length());
        }
    }

    #[test]
    fn the_fireball_bursts_out_fast_and_is_gone_before_the_smoke() {
        let specs = blast_particles(&config(), 6.5, None, &mut Rng::seeded(&[2]));
        let longest = |kind| {
            specs
                .iter()
                .filter(|spec| spec.kind == kind)
                .map(|spec| spec.life)
                .fold(0.0, f32::max)
        };
        assert!(longest(Kind::Smoke) > 2.0 * longest(Kind::Fire));
        // Fast: a fireball puff covers a radius's worth in its first moments.
        let fastest = specs
            .iter()
            .filter(|spec| spec.kind == Kind::Fire)
            .map(|spec| spec.velocity.length())
            .fold(0.0, f32::max);
        assert!(fastest > 6.5 * 2.0, "{fastest}");
        assert!(
            specs
                .iter()
                .filter(|spec| spec.kind == Kind::Fire)
                .all(|spec| spec.life < 0.85),
            "snappy"
        );
    }

    #[test]
    fn glowing_kinds_add_and_smoke_and_dust_blend() {
        for kind in [
            Kind::Fire,
            Kind::Spark,
            Kind::Ember,
            Kind::Ring,
            Kind::Flash,
        ] {
            assert!(kind.glows(), "{kind:?}");
        }
        assert!(!Kind::Smoke.glows() && !Kind::Dust.glows());
    }

    #[test]
    fn gradients_pass_through_their_middle_colour() {
        let colors: Gradient = [[0.0; 4], [1.0; 4], [0.0; 4]];
        assert_eq!(gradient(&colors, 0.0), [0.0; 4]);
        assert_eq!(gradient(&colors, MIDDLE), [1.0; 4]);
        assert_eq!(gradient(&colors, 1.0), [0.0; 4]);
        assert!(gradient(&colors, 0.2)[0] > 0.0);
    }

    #[test]
    fn particles_come_in_at_once_and_are_gone_by_their_end() {
        assert_eq!(envelope(0.0), 0.0);
        assert!(envelope(0.1) > 0.95);
        assert!(envelope(0.9) < envelope(0.7));
        assert_eq!(envelope(1.0), 0.0);
    }

    #[test]
    fn textures_are_opaque_in_the_middle_and_clear_at_the_rim() {
        assert!(puff_shade(0.0, 0.0).1 > 0.6 && puff_shade(1.0, 1.0).1 == 0.0);
        assert!(dot_shade(0.0, 0.0).1 > 0.99 && dot_shade(1.0, 0.0).1 < 0.01);
        assert!(streak_shade(0.0, 0.0).1 > 0.99 && streak_shade(0.5, 0.0).1 < 0.1);
        assert!(streak_shade(0.0, 1.0).1 < 0.01, "pointed at the ends");
        assert!(ring_shade(0.8, 0.0).1 > 0.95 && ring_shade(0.0, 0.0).1 < 0.01);
        let image = texture(puff_shade);
        let pixels = (TEXTURE_SIZE * TEXTURE_SIZE * 4) as usize;
        assert_eq!(image.data.as_ref().map(Vec::len), Some(pixels));
    }

    #[test]
    fn puffs_have_detail_inside_not_a_flat_blob() {
        let samples: Vec<f32> = (-4..=4)
            .map(|i| puff_shade(i as f32 * 0.1, 0.05).0)
            .collect();
        let (low, high) = samples
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        assert!(high - low > 0.05, "{samples:?}");
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
        assert!(full < 0.25, "never more than a few degrees");
        shake.add(5.0);
        assert_eq!(shake.trauma, 1.0);
    }

    #[test]
    fn fires_grow_fiercer_as_the_fuse_burns_down() {
        assert!(fierceness(1.0) > fierceness(0.0));
        assert_eq!(fierceness(5.0), fierceness(1.0));
        let weak = flame_particles(&mut Rng::seeded(&[4]), 3, fierceness(0.0));
        let strong = flame_particles(&mut Rng::seeded(&[4]), 3, fierceness(1.0));
        let tallest = |specs: &[Spec]| {
            specs
                .iter()
                .filter(|spec| spec.kind == Kind::Fire)
                .map(|spec| spec.velocity.y)
                .fold(0.0, f32::max)
        };
        assert!(tallest(&strong) > tallest(&weak));
    }

    #[test]
    fn flames_rise_with_smoke_and_a_hot_core() {
        let specs = flame_particles(&mut Rng::seeded(&[4]), 4, 1.0);
        assert_eq!(count(&specs, Kind::Fire), 4);
        assert_eq!(count(&specs, Kind::Flash), 4, "a hot core per tongue");
        assert_eq!(count(&specs, Kind::Smoke), 2);
        for spec in specs
            .iter()
            .filter(|spec| matches!(spec.kind, Kind::Fire | Kind::Smoke))
        {
            assert!(spec.velocity.y > 0.0 && spec.gravity < 0.0, "{spec:?}");
        }
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
