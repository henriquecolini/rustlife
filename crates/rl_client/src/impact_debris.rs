//! Debris thrown off a struck surface, as the game's code effects throw it.
//!
//! HL2DM leaves `cl_new_impact_effects` at 0, so a bullet or swing striking a
//! surface plays no particle system: `PerformCustomEffects` (`client.so`,
//! `c_impact_effects.cpp`) picks a code effect by the surface's material:
//!
//! - **`FX_DebrisFlecks`** (concrete, tile, wood): a handful of tiny flecks
//!   flung off the surface, falling, bouncing off the map and fading for
//!   three seconds; two faint puffs of dust that grow as they drift; four
//!   quick dark specks; and one slow puff hanging at the hole;
//! - **`FX_DustImpact`** (dirt, sand): twelve puffs thrown hard and braked
//!   to a crawl within half a second (the game's `CDustParticle`).
//!
//! Metal's sparks are [`crate::trail_sparks`]'. The numbers are in
//! `effects.ron`'s `impact_particles`.
//!
//! Every particle is tinted by the struck surface: the game's
//! `GetColorForSurface` multiplies the texture's reflectivity by the light on
//! the spot raised to `1/2.2`. Here the reflectivity is the surface class's
//! `color` and the light is the map's ambient light there, seen from the
//! surface's side ([`surface_color`]).
//!
//! Everything is in Source units, as the engine has it, until it is drawn. A
//! particle's size is a half-size: the engine's quad runs from `-size` to
//! `+size`.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_data::{EffectsConfig, FleckConfig, PuffAim, PuffConfig, SurfaceEffect, Vertical};
use rl_sim::collision::CollisionWorld;
use rl_sim::collision::contents::{GRATE, MOVEABLE, SOLID, WINDOW};
use rl_sim::rng::Rng;
use source_formats::ambient::AmbientCube;

use crate::AppState;
use crate::camera::FlyCam;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::explosion::{between, braked, random_vector, rolled};
use crate::net::Game;
use crate::props::MapLightField;
use crate::world::MapScoped;

/// The longest a frame is allowed to age the debris, seconds.
const MAX_STEP: f32 = 1.0 / 30.0;
/// Most debris alive at once.
const MAX_DEBRIS: usize = 512;
/// How far off the surface flecks start (the game's `spawnOffset`), units.
const FLECK_LIFT: f32 = 1.0;
/// How far off the surface `FX_DebrisFlecks`' dust starts, units.
const DUST_LIFT: f32 = 2.0;
/// What flecks bounce off (`0x400b`: solid, window, grate, moveable).
const FLECK_MASK: u32 = SOLID | WINDOW | GRATE | MOVEABLE;
/// A wall whose normal points up at least this much is a floor a fleck can
/// come to rest on.
const FLOOR: f32 = 0.5;
/// How far short of a wall a bouncing fleck stops, as a share of its step
/// (`fraction - 0.01`).
const SHORT_OF_WALL: f32 = 0.01;
/// What a bounce does to a fleck's turn.
const BOUNCE_ROLL: f32 = -0.25;
/// The exponent the game raises the light on a surface to before tinting
/// debris with it (`GetColorForSurface`).
const LIGHT_GAMMA: f32 = 1.0 / 2.2;

/// Draws the debris of struck surfaces.
pub(crate) struct ImpactDebrisPlugin;

impl Plugin for ImpactDebrisPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Debris>()
            .init_resource::<DebrisSprites>()
            .add_systems(
                Update,
                (load_sprites, spawn_debris, move_debris)
                    .chain()
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// A surface struck, to throw debris off. Source space.
#[derive(Message, Debug, Clone)]
pub(crate) struct Debris {
    /// Where it was struck.
    pub at: Vec3,
    /// The way it faces there.
    pub normal: Vec3,
    /// What it throws: [`SurfaceEffect::Debris`] or [`SurfaceEffect::Dust`].
    pub effect: SurfaceEffect,
    /// Its own colour (the texture's reflectivity), linear `0..=1`.
    pub color: [f32; 3],
}

/// How a particle moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Motion {
    /// Straight on at its speed (the game's plain `CSimpleEmitter`).
    Drift,
    /// Braked hard and fading as `CDustParticle` does.
    Dust,
    /// Falling and bouncing off the map (`CFleckParticles`).
    Fleck {
        /// How fast it falls, units/s².
        gravity: f32,
        /// The share of its speed a bounce keeps, and how much that varies.
        bounce: (f32, f32),
        /// Landing on a floor slower than this (vertically) stops it.
        rest_speed: f32,
    },
}

/// One particle: a sprite facing the camera. Source units and seconds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mote {
    /// Its material, under `materials/`.
    pub sprite: String,
    pub position: Vec3,
    pub velocity: Vec3,
    /// Seconds it lasts.
    pub life: f32,
    /// Half-size at birth and at death.
    pub size: (f32, f32),
    /// Opacity at birth and at death, `0..=1`.
    pub alpha: (f32, f32),
    /// Roll, radians, and its speed, radians/s.
    pub roll: f32,
    pub roll_speed: f32,
    /// Its colour as the engine's vertex colour (gamma space, `0..=1`).
    pub color: [f32; 3],
    pub motion: Motion,
}

/// A whole number in `low..=high`, as the engine's `RandomInt` picks one.
fn whole(rng: &mut Rng, (low, high): (u32, u32)) -> u32 {
    low + rng.below((high.saturating_sub(low) + 1) as usize) as u32
}

/// `color` times `ramp`, each channel no brighter than full.
fn ramped(color: [f32; 3], ramp: f32) -> [f32; 3] {
    color.map(|channel| (channel * ramp).min(1.0))
}

/// The light an ambient cube sheds on a surface facing `normal`:
/// `Σ nᵢ² · cube[side of nᵢ]`.
pub(crate) fn light_on(cube: &AmbientCube, normal: Vec3) -> [f32; 3] {
    let n = normal.normalize_or(Vec3::Z);
    let mut light = [0.0; 3];
    for (axis, component) in n.to_array().into_iter().enumerate() {
        let face = &cube[axis * 2 + usize::from(component < 0.0)];
        for (sum, value) in light.iter_mut().zip(face) {
            *sum += component * component * value;
        }
    }
    light
}

/// The colour the game tints a surface's debris with: its reflectivity times
/// the light on it raised to `1/2.2` (`GetColorForSurface`).
pub(crate) fn surface_color(reflectivity: [f32; 3], light: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| reflectivity[i] * light[i].max(0.0).powf(LIGHT_GAMMA))
}

/// `FX_DebrisFlecks`' flecks, off a surface struck at `at` facing `normal`,
/// tinted from `color`.
pub(crate) fn flecks(
    rng: &mut Rng,
    config: &FleckConfig,
    sprites: &[String],
    (at, normal): (Vec3, Vec3),
    color: [f32; 3],
) -> Vec<Mote> {
    if sprites.is_empty() {
        return Vec::new();
    }
    let origin = at + normal * FLECK_LIFT;
    let count = whole(rng, config.count);
    (0..count)
        .map(|_| {
            let sprite = sprites[rng.below(sprites.len())].clone();
            let aim = normal + random_vector(rng, config.spread);
            let size = whole(rng, config.size);
            let speed = between(rng, config.speed.0, config.speed.1) * (3.0 - size as f32);
            let roll = between(rng, 0.0, 360.0);
            let roll_speed = between(rng, 0.0, config.roll_speed);
            let ramp = between(rng, config.color_ramp.0, config.color_ramp.1);
            Mote {
                sprite,
                position: origin,
                velocity: aim * speed,
                life: config.life,
                size: (size as f32, size as f32),
                alpha: (1.0, 0.0),
                roll,
                roll_speed,
                color: ramped(color, ramp),
                motion: Motion::Fleck {
                    gravity: config.gravity,
                    bounce: (config.bounce, config.bounce_jitter),
                    rest_speed: config.rest_speed,
                },
            }
        })
        .collect()
}

/// One group of puffs, from `origin` off a surface facing `normal`, tinted
/// from `color`.
pub(crate) fn puffs(
    rng: &mut Rng,
    config: &PuffConfig,
    (origin, normal): (Vec3, Vec3),
    color: [f32; 3],
) -> Vec<Mote> {
    (0..config.count as usize)
        .map(|index| {
            let step = if config.steps.is_empty() {
                1.0
            } else {
                config.steps[index % config.steps.len()]
            };
            let life = between(rng, config.life.0, config.life.1);
            let aim = match config.aim {
                PuffAim::Scatter(spread) => normal + random_vector(rng, spread),
                PuffAim::Pushed { scatter, push } => (random_vector(rng, scatter)
                    + normal * between(rng, push.0, push.1))
                .normalize_or(normal),
            };
            let mut velocity = aim * between(rng, config.speed.0, config.speed.1) * step;
            match config.vertical {
                Vertical::Keep => {}
                Vertical::Sink(low, high) => velocity.z -= between(rng, low, high) * step,
                Vertical::Set(low, high) => velocity.z = between(rng, low, high),
            }
            let grown = if config.size_by_step { step + 1.0 } else { 1.0 };
            let size = whole(rng, config.size) as f32 * grown;
            let alpha = whole(rng, config.alpha) as f32 / 255.0;
            let roll = between(rng, 0.0, 360.0);
            let roll_speed = between(rng, -config.roll_speed, config.roll_speed);
            let ramp = between(rng, config.color_ramp.0, config.color_ramp.1);
            Mote {
                sprite: config.sprite.clone(),
                position: origin,
                velocity,
                life,
                size: (size, size * config.end_size),
                alpha: (alpha, 0.0),
                roll,
                roll_speed,
                color: ramped(color, ramp),
                motion: if config.dust {
                    Motion::Dust
                } else {
                    Motion::Drift
                },
            }
        })
        .collect()
}

/// Everything a struck surface throws off: `FX_DebrisFlecks` or
/// `FX_DustImpact`, by `effect`, tinted from `color`.
pub(crate) fn debris(
    rng: &mut Rng,
    effects: &EffectsConfig,
    effect: &SurfaceEffect,
    (at, normal): (Vec3, Vec3),
    color: [f32; 3],
) -> Vec<Mote> {
    let particles = &effects.impact_particles;
    match effect {
        SurfaceEffect::Debris(sprites) => {
            let mut motes = flecks(rng, &particles.flecks, sprites, (at, normal), color);
            let origin = at + normal * DUST_LIFT;
            for group in &particles.debris_dust {
                motes.extend(puffs(rng, group, (origin, normal), color));
            }
            motes
        }
        SurfaceEffect::Dust => particles
            .dust
            .iter()
            .flat_map(|group| puffs(rng, group, (at, normal), color))
            .collect(),
        SurfaceEffect::None | SurfaceEffect::Sparks => Vec::new(),
    }
}

/// A wall a fleck ran into: how far along its step, and which way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Wall {
    pub fraction: f32,
    pub normal: Vec3,
}

/// A particle alive.
#[derive(Component, Debug)]
struct Alive {
    mote: Mote,
    age: f32,
    /// The sprite's own opacity (`$alpha`).
    opacity: f32,
    material: Handle<EffectMaterial>,
}

impl Mote {
    /// Moves it on by `dt`. A fleck bounces off what `trace` (from where it
    /// is to where it would be) says it hits, keeping a share of its speed
    /// picked from `rng`, and lies still once it lands slowly on a floor
    /// (`CParticleCollision::MoveParticle`).
    pub(crate) fn step(
        &mut self,
        dt: f32,
        rng: &mut Rng,
        trace: impl Fn(Vec3, Vec3) -> Option<Wall>,
    ) {
        match self.motion {
            Motion::Drift => {
                self.position += self.velocity * dt;
                self.roll += self.roll_speed * dt;
            }
            Motion::Dust => {
                self.velocity = braked(self.velocity, dt);
                self.position += self.velocity * dt;
                (self.roll, self.roll_speed) = rolled(self.roll, self.roll_speed, dt);
            }
            Motion::Fleck {
                gravity,
                bounce,
                rest_speed,
            } => {
                self.roll += self.roll_speed * dt;
                if self.velocity == Vec3::ZERO {
                    return;
                }
                self.velocity.z -= gravity * dt;
                let to = self.position + self.velocity * dt;
                let Some(wall) = trace(self.position, to) else {
                    self.position = to;
                    return;
                };
                self.position += self.velocity * dt * (wall.fraction - SHORT_OF_WALL).max(0.0);
                if wall.normal.z >= FLOOR && self.velocity.z.abs() <= rest_speed {
                    self.velocity = Vec3::ZERO;
                    self.roll_speed = 0.0;
                    return;
                }
                let along = self.velocity.dot(wall.normal);
                let kept = between(rng, bounce.0 - bounce.1, bounce.0 + bounce.1);
                self.velocity = (self.velocity - 2.0 * along * wall.normal) * kept;
                self.roll_speed *= BOUNCE_ROLL;
            }
        }
    }

    /// Its half-size and opacity `age` seconds in.
    pub(crate) fn look(&self, age: f32) -> (f32, f32) {
        let t = (age / self.life).clamp(0.0, 1.0);
        let size = self.size.0 + (self.size.1 - self.size.0) * t;
        let alpha = match self.motion {
            // `CDustParticle::UpdateAlpha`: its start opacity is not used.
            Motion::Dust => {
                let left = 1.0 - t;
                if left < 0.75 { left * left } else { left }
            }
            Motion::Drift | Motion::Fleck { .. } => {
                self.alpha.0 + (self.alpha.1 - self.alpha.0) * t
            }
        };
        (size, alpha)
    }
}

/// The game's debris sprites, by material name.
#[derive(Resource)]
struct DebrisSprites {
    handles: HashMap<String, Handle<SpriteAsset>>,
    quad: Option<Handle<Mesh>>,
    rng: Rng,
}

impl Default for DebrisSprites {
    fn default() -> Self {
        Self {
            handles: HashMap::new(),
            quad: None,
            rng: Rng::seeded(&[0x0064_6562_7269_7300]),
        }
    }
}

/// Starts loading every sprite the debris uses, once the game's data is known.
fn load_sprites(
    game: Game,
    mut sprites: ResMut<DebrisSprites>,
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if sprites.quad.is_some() {
        return;
    }
    let Some(sim) = game.sim() else {
        return;
    };
    let effects = &sim.data().effects;
    let particles = &effects.impact_particles;
    let flecks = effects
        .bullet_impacts
        .values()
        .filter_map(|impact| match &impact.effect {
            SurfaceEffect::Debris(flecks) => Some(flecks),
            _ => None,
        })
        .flatten();
    let puffs = particles
        .debris_dust
        .iter()
        .chain(&particles.dust)
        .map(|puff| &puff.sprite);
    for name in flecks.chain(puffs) {
        if !sprites.handles.contains_key(name) {
            let handle = assets.load(format!("{SOURCE}://materials/{name}.vmt"));
            sprites.handles.insert(name.clone(), handle);
        }
    }
    sprites.quad = Some(meshes.add(Rectangle::new(1.0, 1.0)));
}

/// Throws the debris of each struck surface.
fn spawn_debris(
    mut commands: Commands,
    mut requests: MessageReader<Debris>,
    game: Game,
    mut sprites: ResMut<DebrisSprites>,
    assets: Res<Assets<SpriteAsset>>,
    field: Option<Res<MapLightField>>,
    live: Query<(), With<Alive>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
) {
    let (Some(sim), Some(quad)) = (game.sim(), sprites.quad.clone()) else {
        requests.clear();
        return;
    };
    let effects = &sim.data().effects;
    let mut room = MAX_DEBRIS.saturating_sub(live.iter().count());
    for request in requests.read() {
        let light = field
            .as_deref()
            .and_then(|field| field.0.cube_at((request.at + request.normal).to_array()))
            .map_or([1.0; 3], |cube| light_on(&cube, request.normal));
        let color = surface_color(request.color, light);
        let motes = debris(
            &mut sprites.rng,
            effects,
            &request.effect,
            (request.at, request.normal),
            color,
        );
        for mote in motes {
            if room == 0 {
                return;
            }
            let Some(sprite) = sprites
                .handles
                .get(&mote.sprite)
                .and_then(|handle| assets.get(handle))
            else {
                continue;
            };
            room -= 1;
            let material = materials.add(effect_material(StandardMaterial {
                base_color: Color::NONE,
                base_color_texture: Some(sprite.image.clone()),
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: if sprite.additive {
                    AlphaMode::Add
                } else {
                    AlphaMode::Blend
                },
                ..default()
            }));
            commands.spawn((
                Mesh3d(quad.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(coords::position(mote.position.to_array()))
                    .with_scale(Vec3::ZERO),
                Alive {
                    mote,
                    age: 0.0,
                    opacity: sprite.alpha,
                    material,
                },
                MapScoped,
            ));
        }
    }
}

/// The wall a fleck flying from `from` to `to` hits in `world`, if any.
fn wall(world: &CollisionWorld, from: Vec3, to: Vec3) -> Option<Wall> {
    let trace = world.trace_ray(from, to, FLECK_MASK);
    (trace.hit() && !trace.start_solid).then_some(Wall {
        fraction: trace.fraction,
        normal: trace.normal,
    })
}

/// Moves, grows, fades and turns the debris to face the camera; removes the
/// spent.
fn move_debris(
    mut commands: Commands,
    game: Game,
    time: Res<Time>,
    mut sprites: ResMut<DebrisSprites>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut debris: Query<(Entity, &mut Alive, &mut Transform)>,
    mut materials: ResMut<Assets<EffectMaterial>>,
) {
    // A long frame must not age the briefest specks to death unseen.
    let dt = time.delta_secs().min(MAX_STEP);
    let Ok(camera) = camera.single() else {
        return;
    };
    let facing = camera.to_scale_rotation_translation().1;
    let world = game.sim().map(rl_sim::simulation::Simulation::world);
    for (entity, mut alive, mut transform) in &mut debris {
        alive.age += dt;
        if alive.age >= alive.mote.life {
            commands.entity(entity).try_despawn();
            continue;
        }
        alive.mote.step(dt, &mut sprites.rng, |from, to| {
            world.and_then(|world| wall(world, from, to))
        });
        let (size, alpha) = alive.mote.look(alive.age);
        transform.translation = coords::position(alive.mote.position.to_array());
        transform.rotation = facing * Quat::from_rotation_z(alive.mote.roll);
        transform.scale = Vec3::splat(2.0 * size * coords::METERS_PER_UNIT);
        if let Some(mut material) = materials.get_mut(&alive.material) {
            let [r, g, b] = alive.mote.color;
            material.base.base_color = Color::srgba(r, g, b, alpha * alive.opacity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effects() -> EffectsConfig {
        rl_data::GameData::builtin().expect("builtin data").effects
    }

    fn rng() -> Rng {
        Rng::seeded(&[7])
    }

    fn cement() -> SurfaceEffect {
        SurfaceEffect::Debris(vec!["effects/fleck_cement1".to_owned()])
    }

    #[test]
    fn concrete_throws_four_to_sixteen_flecks_and_seven_puffs() {
        let effects = effects();
        let normal = Vec3::X;
        for seed in 0..32 {
            let mut rng = Rng::seeded(&[seed]);
            let motes = debris(
                &mut rng,
                &effects,
                &cement(),
                (Vec3::ZERO, normal),
                [0.5; 3],
            );
            let flecks: Vec<&Mote> = motes
                .iter()
                .filter(|mote| matches!(mote.motion, Motion::Fleck { .. }))
                .collect();
            assert!((4..=16).contains(&flecks.len()), "{}", flecks.len());
            assert_eq!(
                motes.len() - flecks.len(),
                7,
                "2 puffs, 4 specks, 1 slow puff"
            );
            for fleck in &flecks {
                assert_eq!(fleck.position, normal * FLECK_LIFT);
                assert!([1.0, 2.0].contains(&fleck.size.0));
                assert_eq!(fleck.life, 3.0);
                // Off the wall, at 64..128 times three less its size.
                let speed = fleck.velocity.length();
                let aim = fleck.velocity / speed;
                assert!(aim.x > 0.0, "{aim}");
            }
        }
    }

    #[test]
    fn the_debris_dust_is_small_and_faint() {
        let effects = effects();
        let motes = debris(
            &mut rng(),
            &effects,
            &cement(),
            (Vec3::ZERO, Vec3::Z),
            [0.5; 3],
        );
        let puffs: Vec<&Mote> = motes
            .iter()
            .filter(|mote| mote.sprite == "particle/particle_smokegrenade")
            .collect();
        assert_eq!(puffs.len(), 3);
        for puff in puffs {
            assert_eq!(puff.position, Vec3::Z * DUST_LIFT);
            // 2..4 growing eightfold, or 4..8 growing fourfold: at most 32
            // units across at death.
            assert!(puff.size.1 <= 32.0, "{:?}", puff.size);
            assert!(puff.alpha.0 <= 200.0 / 255.0 && puff.alpha.1 == 0.0);
            assert_eq!(puff.motion, Motion::Drift);
        }
    }

    #[test]
    fn dirt_throws_twelve_braking_puffs_and_metal_glass_nothing_here() {
        let effects = effects();
        let at = (Vec3::ZERO, Vec3::Z);
        let dust = debris(&mut rng(), &effects, &SurfaceEffect::Dust, at, [0.5; 3]);
        assert_eq!(dust.len(), 12);
        assert!(dust.iter().all(|mote| mote.motion == Motion::Dust));
        // The game's speed table 3, 1, 2, 0 leaves one of each group still.
        assert!(dust.iter().any(|mote| mote.velocity == Vec3::ZERO));
        for effect in [SurfaceEffect::Sparks, SurfaceEffect::None] {
            assert!(debris(&mut rng(), &effects, &effect, at, [0.5; 3]).is_empty());
        }
    }

    #[test]
    fn stepped_puffs_go_faster_and_grow_bigger() {
        let config = PuffConfig {
            sprite: "s".to_owned(),
            count: 4,
            life: (1.0, 1.0),
            aim: PuffAim::Pushed {
                scatter: 0.0,
                push: (1.0, 1.0),
            },
            speed: (100.0, 100.0),
            steps: vec![3.0, 1.0, 2.0, 0.0],
            vertical: Vertical::Keep,
            size: (2, 2),
            size_by_step: true,
            end_size: 4.0,
            alpha: (255, 255),
            roll_speed: 0.0,
            color_ramp: (1.0, 1.0),
            dust: true,
        };
        let motes = puffs(&mut rng(), &config, (Vec3::ZERO, Vec3::Z), [0.5; 3]);
        let speeds: Vec<f32> = motes.iter().map(|mote| mote.velocity.z).collect();
        assert_eq!(speeds, [300.0, 100.0, 200.0, 0.0]);
        let sizes: Vec<(f32, f32)> = motes.iter().map(|mote| mote.size).collect();
        assert_eq!(sizes, [(8.0, 32.0), (4.0, 16.0), (6.0, 24.0), (2.0, 8.0)]);
    }

    #[test]
    fn sinking_and_set_vertical_speeds() {
        let mut config = PuffConfig {
            sprite: "s".to_owned(),
            count: 2,
            life: (1.0, 1.0),
            aim: PuffAim::Scatter(0.0),
            speed: (10.0, 10.0),
            steps: vec![1.0, 2.0],
            vertical: Vertical::Sink(5.0, 5.0),
            size: (1, 1),
            size_by_step: false,
            end_size: 1.0,
            alpha: (100, 100),
            roll_speed: 1.0,
            color_ramp: (1.0, 1.0),
            dust: false,
        };
        let motes = puffs(&mut rng(), &config, (Vec3::ZERO, Vec3::X), [0.5; 3]);
        assert_eq!(motes[0].velocity, Vec3::new(10.0, 0.0, -5.0));
        assert_eq!(motes[1].velocity, Vec3::new(20.0, 0.0, -10.0));
        config.vertical = Vertical::Set(3.0, 3.0);
        let motes = puffs(&mut rng(), &config, (Vec3::ZERO, Vec3::X), [0.5; 3]);
        assert_eq!(motes[1].velocity, Vec3::new(20.0, 0.0, 3.0));
    }

    fn fleck(velocity: Vec3) -> Mote {
        Mote {
            sprite: "f".to_owned(),
            position: Vec3::new(0.0, 0.0, 10.0),
            velocity,
            life: 3.0,
            size: (1.0, 1.0),
            alpha: (1.0, 0.0),
            roll: 0.0,
            roll_speed: 4.0,
            color: [1.0; 3],
            motion: Motion::Fleck {
                gravity: 800.0,
                bounce: (0.3, 0.0),
                rest_speed: 48.0,
            },
        }
    }

    /// A floor at z = 0.
    fn floor(from: Vec3, to: Vec3) -> Option<Wall> {
        (to.z < 0.0).then(|| Wall {
            fraction: from.z / (from.z - to.z),
            normal: Vec3::Z,
        })
    }

    #[test]
    fn a_fleck_falls_and_a_slow_landing_stops_it() {
        let mut mote = fleck(Vec3::new(10.0, 0.0, 0.0));
        mote.step(0.01, &mut rng(), floor);
        assert!((mote.velocity.z + 8.0).abs() < 1e-4, "gravity");
        mote.position.z = 0.05;
        mote.step(0.01, &mut rng(), floor);
        assert_eq!(mote.velocity, Vec3::ZERO, "slow onto a floor: stops");
        assert_eq!(mote.roll_speed, 0.0);
        let resting = mote.position;
        mote.step(0.1, &mut rng(), floor);
        assert_eq!(mote.position, resting);
    }

    #[test]
    fn a_fast_fleck_bounces_keeping_a_share_of_its_speed() {
        let mut mote = fleck(Vec3::new(0.0, 0.0, -500.0));
        mote.position.z = 1.0;
        mote.step(0.01, &mut rng(), floor);
        assert!(mote.velocity.z > 0.0, "{}", mote.velocity);
        assert!((mote.velocity.z - 508.0 * 0.3).abs() < 1e-2);
        assert!(mote.position.z >= 0.0);
        assert_eq!(mote.roll_speed, -1.0);
    }

    #[test]
    fn dust_fades_from_full_and_others_from_their_start() {
        let mut mote = fleck(Vec3::ZERO);
        assert_eq!(mote.look(0.0), (1.0, 1.0));
        assert!((mote.look(1.5).1 - 0.5).abs() < 1e-6);
        mote.motion = Motion::Dust;
        mote.alpha = (0.2, 0.0);
        mote.life = 1.0;
        assert_eq!(mote.look(0.0).1, 1.0, "the start opacity is not used");
        assert!((mote.look(0.2).1 - 0.8).abs() < 1e-6);
        assert!(
            (mote.look(0.5).1 - 0.25).abs() < 1e-6,
            "squared past a quarter"
        );
    }

    #[test]
    fn the_surface_colour_is_its_reflectivity_under_the_light() {
        let mut cube: AmbientCube = [[0.0; 3]; 6];
        cube[0] = [1.0, 0.25, 0.0];
        cube[4] = [0.5; 3];
        assert_eq!(light_on(&cube, Vec3::X), [1.0, 0.25, 0.0]);
        assert_eq!(light_on(&cube, Vec3::NEG_X), [0.0; 3]);
        let diagonal = light_on(&cube, Vec3::new(1.0, 0.0, 1.0));
        assert!((diagonal[0] - 0.75).abs() < 1e-5);
        let color = surface_color([0.4; 3], [1.0, 0.25, 0.0]);
        assert!((color[0] - 0.4).abs() < 1e-6);
        assert!((color[1] - 0.4 * 0.25f32.powf(1.0 / 2.2)).abs() < 1e-6);
        assert_eq!(color[2], 0.0);
        assert_eq!(ramped([0.6, 0.2, 1.0], 2.0), [1.0, 0.4, 1.0]);
    }
}
