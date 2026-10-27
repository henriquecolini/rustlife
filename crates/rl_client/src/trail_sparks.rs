//! Streaks of sparks, as the engine's `CTrailParticles` throw them.
//!
//! A spark is a bright streak that flies under gravity, bounces off the map
//! and goes out. Its streak is `length` seconds of its flight long, so a fast
//! spark is long and a slow one a dot. The sparks of one group move alike
//! (`CParticleCollision`: gravity and how much speed a bounce keeps).
//!
//! Two of the game's code effects are built here, both read from `client.so`
//! (`fx_sparks.cpp`):
//!
//! - [`gauss_explosion`], `FX_GaussExplosion`: sixteen to thirty-two sparks
//!   flung along the direction it is given, then an electric spark from the
//!   same place (what the Gravity Gun's `PhyscannonImpact` makes);
//! - [`electric_spark`], `FX_ElectricSpark`: a few long, slow sparks, and a
//!   spray of short, fast ones. Its third emitter makes three `yellowflare_noz`
//!   glints at the spot ([`electric_glints`]).
//!
//! Everything here is in Source units until a spark is drawn.

use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_sim::collision::CollisionWorld;
use rl_sim::collision::contents::SOLID;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::camera::FlyCam;
use crate::explosion::{between, random_vector};
use crate::world::MapScoped;

/// The material every spark is drawn with.
const MATERIAL: &str = "effects/spark";
/// The material of the glints.
const GLINT_MATERIAL: &str = "effects/yellowflare_noz";
/// How strongly the sparks' random scatter bends the direction they are
/// given (`RandomVector(-0.6, 0.6)`).
const GAUSS_SCATTER: f32 = 0.6;
/// A spark that bounces off a wall and is left slower than this lies still,
/// units/s.
const REST_SPEED: f32 = 30.0;
/// The longest a frame is allowed to age the sparks, seconds.
const MAX_STEP: f32 = 1.0 / 30.0;
/// Most sparks alive at once.
const MAX_SPARKS: usize = 400;
/// How far a bouncing spark is kept off the wall, units.
const WALL_GAP: f32 = 0.5;

/// One spark to make.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Spark {
    /// How it is thrown, units/s.
    pub velocity: Vec3,
    /// Seconds until it goes out (`m_flDieTime`).
    pub life: f32,
    /// Seconds of its flight its streak spans (`m_flLength`).
    pub length: f32,
    /// How wide its streak is, units (`m_flWidth`).
    pub width: f32,
}

/// Sparks that move alike: one `CTrailParticles` emitter.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Group {
    /// How fast they fall, units/s².
    pub gravity: f32,
    /// The share of its speed a spark keeps when it bounces off a wall.
    pub bounce: f32,
    /// Whether they fade out over their life (`bitsPARTICLE_TRAIL_FADE`)
    /// rather than going out at the end.
    pub fades: bool,
    /// The sparks.
    pub sparks: Vec<Spark>,
}

/// `FX_ElectricSpark`: sparks thrown out along `direction` (or up, as a
/// fountain, if it is `None`). `magnitude` and `trail` scale the number of
/// sparks and how long their streaks are; the game's Gravity Gun passes 1
/// for both.
pub(crate) fn electric_spark(
    rng: &mut Rng,
    magnitude: u32,
    trail: u32,
    direction: Option<Vec3>,
) -> Vec<Group> {
    let scale = magnitude as f32;
    let trail = trail as f32;
    // A few long, slow sparks that last a second or two and fall hard.
    let long = (between(rng, 2.0, 4.0) * scale * scale) as usize;
    let long_sparks = (0..long)
        .map(|_| {
            let life = between(rng, 1.0, 2.0);
            let scatter = Vec3::new(
                between(rng, -1.0, 1.0),
                between(rng, -1.0, 1.0),
                between(rng, 0.5, 1.0),
            );
            let aim = direction.map_or(scatter, |d| d * 2.0 + scatter);
            Spark {
                velocity: aim.normalize_or_zero() * between(rng, 64.0, 300.0),
                life,
                length: between(rng, 0.02, 0.05) * trail,
                width: between(rng, 2.0, 5.0),
            }
        })
        .collect();
    // A spray of short ones that go out in a fifth of a second.
    let spray = (rng.below(17) as f32 + 16.0) * scale;
    let spray_sparks = (0..spray as usize)
        .map(|_| {
            let scatter = random_vector(rng, 1.0);
            let aim = direction.map_or(scatter, |d| d + scatter);
            Spark {
                velocity: aim.normalize_or_zero() * between(rng, 128.0, 256.0),
                life: between(rng, 0.1, 0.2) * trail,
                length: between(rng, 0.02, 0.03) * trail,
                width: between(rng, 2.0, 4.0),
            }
        })
        .collect();
    vec![
        Group {
            gravity: 800.0,
            bounce: 0.3,
            fades: true,
            sparks: long_sparks,
        },
        Group {
            gravity: 400.0,
            bounce: 0.3,
            fades: false,
            sparks: spray_sparks,
        },
    ]
}

/// `FX_GaussExplosion`: sixteen to thirty-two sparks flung along
/// `direction` (a unit vector, the way the burst faces), then an electric
/// spark along the last one's heading.
pub(crate) fn gauss_explosion(rng: &mut Rng, direction: Vec3) -> Vec<Group> {
    let count = 16 + rng.below(17);
    let mut last = direction;
    let sparks = (0..count)
        .map(|_| {
            let aim = (direction + random_vector(rng, GAUSS_SCATTER)).normalize_or_zero();
            last = aim;
            Spark {
                velocity: aim * between(rng, 128.0, 512.0),
                life: between(rng, 0.25, 1.0),
                length: between(rng, 0.01, 0.1),
                width: between(rng, 1.0, 4.0),
            }
        })
        .collect();
    let mut groups = vec![Group {
        gravity: 800.0,
        bounce: 0.3,
        fades: true,
        sparks,
    }];
    groups.extend(electric_spark(rng, 1, 1, Some(last)));
    groups
}

/// One glint of light to make: a sprite that faces the camera, added to what
/// is behind it (`FX_ElectricSpark`'s third emitter, a `CSimpleEmitter`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Glint {
    /// Where, relative to the burst, units.
    pub offset: Vec3,
    /// How it drifts, units/s.
    pub velocity: Vec3,
    /// Seconds it lasts.
    pub life: f32,
    /// Its colour, `0..=1` each.
    pub color: [f32; 3],
    /// How opaque it is at birth and at death, `0..=1`.
    pub alpha: (f32, f32),
    /// Its half-size at birth and at death, units.
    pub size: (f32, f32),
    /// Starting roll, degrees, and roll speed, degrees/s.
    pub roll: f32,
    pub roll_speed: f32,
}

/// A whole number in `low..=high` as the engine's `RandomInt` gives one.
fn whole(rng: &mut Rng, low: u32, high: u32) -> f32 {
    (low + rng.below((high - low + 1) as usize) as u32) as f32
}

/// The three glints of `FX_ElectricSpark`: a white flash that shrinks away
/// in a tenth of a second, a big dim grey flare that does the same, and a
/// faint warm glint that drifts up for a second, growing. `magnitude`
/// scales the first two.
pub(crate) fn electric_glints(rng: &mut Rng, magnitude: u32) -> [Glint; 3] {
    let scale = magnitude as f32;
    let flash = Glint {
        offset: Vec3::ZERO,
        velocity: Vec3::ZERO,
        life: 0.1,
        color: [1.0; 3],
        alpha: (1.0, 1.0),
        size: (whole(rng, 4, 8) * scale, 0.0),
        roll: whole(rng, 0, 360),
        roll_speed: 0.0,
    };
    let grey = whole(rng, 32, 64) / 255.0;
    let flare = Glint {
        offset: Vec3::ZERO,
        velocity: Vec3::ZERO,
        life: 0.1,
        color: [grey; 3],
        alpha: (grey, 0.0),
        size: (whole(rng, 32, 64) * scale, 0.0),
        roll: whole(rng, 0, 360),
        roll_speed: between(rng, -1.0, 1.0),
    };
    let offset = Vec3::new(between(rng, -4.0, 4.0), between(rng, -4.0, 4.0), 0.0);
    let velocity = Vec3::new(between(rng, -16.0, 16.0), between(rng, -16.0, 16.0), 16.0);
    let small = whole(rng, 4, 8);
    let glint = Glint {
        offset,
        velocity,
        life: 1.0,
        color: [1.0, 1.0, 200.0 / 255.0],
        alpha: (whole(rng, 16, 32) / 255.0, 0.0),
        size: (small, small * 4.0),
        roll: whole(rng, 0, 360),
        roll_speed: between(rng, -2.0, 2.0),
    };
    [flash, flare, glint]
}

/// What a burst is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Burst {
    /// `FX_GaussExplosion`.
    Gauss,
}

/// A burst of sparks to draw: where (Bevy space, metres) and which way
/// (a Source-space unit vector).
#[derive(Message, Debug, Clone, Copy)]
pub(crate) struct Sparks {
    pub at: Vec3,
    pub direction: Vec3,
    pub burst: Burst,
}

/// A wall a spark ran into, Source units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hit {
    /// Where it struck.
    pub end: Vec3,
    /// The wall's normal.
    pub normal: Vec3,
}

/// A spark in flight, Source units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Flight {
    pub position: Vec3,
    pub velocity: Vec3,
    /// Whether it came to rest on something.
    pub settled: bool,
}

/// Moves a spark for `dt` seconds: it falls at `gravity`, and bounces off
/// whatever `trace` (from its place to where it would be) says it hits,
/// keeping `bounce` of its speed, lying still once that leaves it slow.
pub(crate) fn fly(
    flight: &mut Flight,
    gravity: f32,
    bounce: f32,
    dt: f32,
    trace: impl Fn(Vec3, Vec3) -> Option<Hit>,
) {
    if flight.settled {
        return;
    }
    flight.velocity.z -= gravity * dt;
    let to = flight.position + flight.velocity * dt;
    let Some(hit) = trace(flight.position, to) else {
        flight.position = to;
        return;
    };
    flight.position = hit.end + hit.normal * WALL_GAP;
    let along = flight.velocity.dot(hit.normal);
    flight.velocity = (flight.velocity - 2.0 * along * hit.normal) * bounce;
    if flight.velocity.length() < REST_SPEED {
        flight.velocity = Vec3::ZERO;
        flight.settled = true;
    }
}

/// How much of a spark is left at `age` of its `life`: all of it, or, if the
/// group fades, what is left of a straight line down to nothing.
pub(crate) fn brightness(age: f32, life: f32, fades: bool) -> f32 {
    if fades {
        (1.0 - age / life).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// Draws bursts of sparks.
pub(crate) struct TrailSparksPlugin;

impl Plugin for TrailSparksPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<Sparks>()
            .init_resource::<SparkSprite>()
            .add_systems(Startup, load_sprite)
            .add_systems(
                Update,
                (spawn_sparks, move_sparks, move_glints)
                    .chain()
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The game's spark and glint textures.
#[derive(Resource, Default)]
struct SparkSprite {
    spark: Handle<SpriteAsset>,
    glint: Handle<SpriteAsset>,
}

fn load_sprite(mut sprite: ResMut<SparkSprite>, assets: Res<AssetServer>) {
    let load = |name: &str| assets.load(format!("{SOURCE}://materials/{name}.vmt"));
    sprite.spark = load(MATERIAL);
    sprite.glint = load(GLINT_MATERIAL);
}

/// A glint alive.
#[derive(Component, Debug)]
struct GlintAlive {
    glint: Glint,
    /// Where it is, Source units.
    position: Vec3,
    age: f32,
    material: Handle<StandardMaterial>,
}

/// A spark alive.
#[derive(Component, Debug)]
struct Alive {
    flight: Flight,
    age: f32,
    life: f32,
    length: f32,
    width: f32,
    gravity: f32,
    bounce: f32,
    fades: bool,
    material: Handle<StandardMaterial>,
}

/// Makes the sparks of each asked-for burst.
fn spawn_sparks(
    mut commands: Commands,
    mut bursts: MessageReader<Sparks>,
    sprite: Res<SparkSprite>,
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
    let rng = rng.get_or_insert_with(|| Rng::seeded(&[0x0073_7061_726b]));
    let (Some(spark_sprite), Some(glint_sprite)) =
        (assets.get(&sprite.spark), assets.get(&sprite.glint))
    else {
        bursts.clear();
        return;
    };
    let sprite = spark_sprite;
    let mut room = MAX_SPARKS.saturating_sub(live.iter().count());
    for burst in bursts.read() {
        let groups = match burst.burst {
            Burst::Gauss => gauss_explosion(rng, burst.direction),
        };
        let origin = Vec3::from(coords::to_source_position(burst.at));
        for group in groups {
            for spark in group.sparks {
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
                    alpha_mode: if sprite.additive {
                        AlphaMode::Add
                    } else {
                        AlphaMode::Blend
                    },
                    ..default()
                });
                commands.spawn((
                    Alive {
                        flight: Flight {
                            position: origin,
                            velocity: spark.velocity,
                            settled: false,
                        },
                        age: 0.0,
                        life: spark.life,
                        length: spark.length,
                        width: spark.width,
                        gravity: group.gravity,
                        bounce: group.bounce,
                        fades: group.fades,
                        material: material.clone(),
                    },
                    Mesh3d(quad.clone()),
                    MeshMaterial3d(material),
                    Transform::from_translation(burst.at).with_scale(Vec3::ZERO),
                    MapScoped,
                ));
            }
        }
        for glint in electric_glints(rng, 1) {
            let material = materials.add(StandardMaterial {
                base_color: Color::NONE,
                base_color_texture: Some(glint_sprite.image.clone()),
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Add,
                ..default()
            });
            commands.spawn((
                GlintAlive {
                    position: origin + glint.offset,
                    glint,
                    age: 0.0,
                    material: material.clone(),
                },
                Mesh3d(quad.clone()),
                MeshMaterial3d(material),
                Transform::from_translation(burst.at).with_scale(Vec3::ZERO),
                MapScoped,
            ));
        }
    }
}

/// The wall a spark flying from `from` to `to` hits in `world`, if any.
fn wall(world: &CollisionWorld, from: Vec3, to: Vec3) -> Option<Hit> {
    let trace = world.trace_ray(from, to, SOLID);
    (trace.hit() && !trace.start_solid).then_some(Hit {
        end: trace.end,
        normal: trace.normal,
    })
}

/// Moves, fades and removes the sparks, and stretches each along its flight.
fn move_sparks(
    mut commands: Commands,
    game: crate::net::Game,
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut sparks: Query<(Entity, &mut Alive, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // A long frame must not age the briefest sparks to death unseen.
    let dt = time.delta_secs().min(MAX_STEP);
    let Ok(camera) = camera.single() else {
        return;
    };
    let eye = camera.translation();
    let world = game.sim().map(rl_sim::simulation::Simulation::world);
    for (entity, mut spark, mut transform) in &mut sparks {
        spark.age += dt;
        if spark.age >= spark.life {
            commands.entity(entity).try_despawn();
            continue;
        }
        let (gravity, bounce) = (spark.gravity, spark.bounce);
        fly(&mut spark.flight, gravity, bounce, dt, |from, to| {
            world.and_then(|world| wall(world, from, to))
        });
        let head = coords::position(spark.flight.position.to_array());
        let velocity = spark.flight.velocity;
        let heading = coords::direction(velocity.to_array()).normalize_or_zero();
        let length = velocity.length() * spark.length * coords::METERS_PER_UNIT;
        let toward = (eye - head).normalize_or_zero();
        let across = heading.cross(toward).normalize_or_zero();
        let facing = across.cross(heading);
        transform.rotation = Quat::from_mat3(&Mat3::from_cols(across, heading, facing));
        transform.scale = Vec3::new(
            spark.width * coords::METERS_PER_UNIT,
            length.max(spark.width * coords::METERS_PER_UNIT),
            1.0,
        );
        // The head leads; the streak's middle is half its length behind it.
        transform.translation = head - heading * (0.5 * transform.scale.y);
        let light = brightness(spark.age, spark.life, spark.fades);
        if let Some(mut material) = materials.get_mut(&spark.material) {
            material.base_color = Color::linear_rgba(light, light, light, light);
        }
    }
}

/// Drifts, rolls, shrinks or grows and fades the glints, facing the camera;
/// removes the spent.
fn move_glints(
    mut commands: Commands,
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut glints: Query<(Entity, &mut GlintAlive, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs().min(MAX_STEP);
    let Ok(camera) = camera.single() else {
        return;
    };
    let facing = camera.to_scale_rotation_translation().1;
    for (entity, mut alive, mut transform) in &mut glints {
        alive.age += dt;
        let glint = alive.glint;
        if alive.age >= glint.life {
            commands.entity(entity).try_despawn();
            continue;
        }
        alive.position += glint.velocity * dt;
        let t = alive.age / glint.life;
        let half = glint.size.0 + (glint.size.1 - glint.size.0) * t;
        let alpha = glint.alpha.0 + (glint.alpha.1 - glint.alpha.0) * t;
        let roll = (glint.roll + glint.roll_speed * alive.age).to_radians();
        transform.translation = coords::position(alive.position.to_array());
        transform.rotation = facing * Quat::from_rotation_z(roll);
        transform.scale = Vec3::splat(2.0 * half * coords::METERS_PER_UNIT);
        if let Some(mut material) = materials.get_mut(&alive.material) {
            let [r, g, b] = glint.color.map(|c| c * alpha);
            material.base_color = Color::linear_rgba(r, g, b, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng() -> Rng {
        Rng::seeded(&[42])
    }

    #[test]
    fn a_gauss_explosion_flings_sixteen_to_thirty_two_sparks_then_an_electric_spark() {
        for seed in 0..20 {
            let groups = gauss_explosion(&mut Rng::seeded(&[seed]), Vec3::Z);
            assert_eq!(
                groups.len(),
                3,
                "its own sparks and the electric spark's two"
            );
            assert!((16..=32).contains(&groups[0].sparks.len()));
            assert!(
                (2..=3).contains(&groups[1].sparks.len()),
                "{}",
                groups[1].sparks.len()
            );
            assert!((16..=32).contains(&groups[2].sparks.len()));
        }
    }

    #[test]
    fn the_gauss_sparks_have_the_engines_speeds_lives_and_streaks() {
        let groups = gauss_explosion(&mut rng(), Vec3::X);
        let gauss = &groups[0];
        assert_eq!(
            (gauss.gravity, gauss.bounce, gauss.fades),
            (800.0, 0.3, true)
        );
        for spark in &gauss.sparks {
            assert!((128.0..=512.0).contains(&spark.velocity.length()));
            assert!((0.25..=1.0).contains(&spark.life));
            assert!((0.01..=0.1).contains(&spark.length));
            assert!((1.0..=4.0).contains(&spark.width));
        }
    }

    #[test]
    fn the_electric_sparks_come_in_a_slow_group_and_a_fast_one() {
        let groups = electric_spark(&mut rng(), 1, 1, Some(Vec3::Y));
        let (slow, fast) = (&groups[0], &groups[1]);
        assert_eq!((slow.gravity, slow.fades), (800.0, true));
        assert_eq!((fast.gravity, fast.fades), (400.0, false));
        for spark in &slow.sparks {
            assert!((64.0..=300.0).contains(&spark.velocity.length()));
            assert!((1.0..=2.0).contains(&spark.life));
            assert!((2.0..=5.0).contains(&spark.width));
        }
        for spark in &fast.sparks {
            assert!((128.0..=256.0).contains(&spark.velocity.length()));
            assert!((0.1..=0.2).contains(&spark.life), "a fifth of a second");
            assert!((0.02..=0.03).contains(&spark.length));
        }
    }

    #[test]
    fn a_bigger_magnitude_throws_more_sparks() {
        let count = |magnitude| {
            electric_spark(&mut Rng::seeded(&[7]), magnitude, 1, None)
                .iter()
                .map(|group| group.sparks.len())
                .sum::<usize>()
        };
        assert!(count(3) > 2 * count(1), "{} vs {}", count(3), count(1));
    }

    #[test]
    fn without_a_direction_the_electric_sparks_fountain_up() {
        let groups = electric_spark(&mut rng(), 1, 1, None);
        assert!(groups[0].sparks.iter().all(|spark| spark.velocity.z > 0.0));
    }

    #[test]
    fn the_sparks_head_where_they_are_aimed() {
        let mut up = 0;
        let mut total = 0;
        for seed in 0..30 {
            for spark in &gauss_explosion(&mut Rng::seeded(&[seed]), Vec3::Z)[0].sparks {
                total += 1;
                up += i32::from(spark.velocity.z > 0.0);
            }
        }
        assert!(up * 10 > total * 9, "{up} of {total}");
    }

    fn open(_: Vec3, _: Vec3) -> Option<Hit> {
        None
    }

    #[test]
    fn a_spark_falls_at_its_groups_gravity() {
        let mut flight = Flight {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            settled: false,
        };
        for _ in 0..60 {
            fly(&mut flight, 800.0, 0.3, 1.0 / 60.0, open);
        }
        assert!(
            (flight.velocity.z + 800.0).abs() < 1e-2,
            "{:?}",
            flight.velocity
        );
        assert!(flight.position.z < -300.0);
    }

    #[test]
    fn a_spark_bounces_off_a_floor_keeping_a_share_of_its_speed() {
        let floor = |from: Vec3, to: Vec3| {
            (to.z <= 0.0).then_some(Hit {
                end: Vec3::new(from.x, from.y, 0.0),
                normal: Vec3::Z,
            })
        };
        let mut flight = Flight {
            position: Vec3::new(0.0, 0.0, 1.0),
            velocity: Vec3::new(100.0, 0.0, -400.0),
            settled: false,
        };
        fly(&mut flight, 0.0, 0.3, 0.1, floor);
        assert!(
            (flight.velocity.z - 120.0).abs() < 1e-3,
            "{:?}",
            flight.velocity
        );
        assert!(
            (flight.velocity.x - 30.0).abs() < 1e-3,
            "{:?}",
            flight.velocity
        );
        assert!(flight.position.z > 0.0, "kept off the floor");
        assert!(!flight.settled);
    }

    #[test]
    fn a_slow_bounce_leaves_a_spark_lying_still() {
        let floor = |from: Vec3, _| {
            Some(Hit {
                end: from,
                normal: Vec3::Z,
            })
        };
        let mut flight = Flight {
            position: Vec3::ZERO,
            velocity: Vec3::new(0.0, 0.0, -50.0),
            settled: false,
        };
        fly(&mut flight, 0.0, 0.3, 0.1, floor);
        assert!(flight.settled);
        assert_eq!(flight.velocity, Vec3::ZERO);
        let before = flight;
        fly(&mut flight, 800.0, 0.3, 0.1, open);
        assert_eq!(flight, before, "a settled spark stays put");
    }

    #[test]
    fn the_third_emitter_makes_a_flash_a_flare_and_a_glint() {
        for seed in 0..20 {
            let [flash, flare, glint] = electric_glints(&mut Rng::seeded(&[seed]), 1);
            assert_eq!(
                (flash.life, flash.color, flash.alpha),
                (0.1, [1.0; 3], (1.0, 1.0))
            );
            assert!((4.0..=8.0).contains(&flash.size.0) && flash.size.1 == 0.0);
            assert!((32.0..=64.0).contains(&flare.size.0), "a big dim flare");
            assert!((0.12..=0.26).contains(&flare.alpha.0), "{}", flare.alpha.0);
            assert_eq!(flare.alpha.1, 0.0);
            assert_eq!(glint.life, 1.0);
            assert_eq!(glint.velocity.z, 16.0, "it drifts up");
            assert!(glint.velocity.x.abs() <= 16.0 && glint.velocity.y.abs() <= 16.0);
            assert!(glint.offset.x.abs() <= 4.0 && glint.offset.y.abs() <= 4.0);
            assert!((0.06..=0.13).contains(&glint.alpha.0), "{}", glint.alpha.0);
            assert_eq!(glint.size.1, glint.size.0 * 4.0, "it grows fourfold");
            assert!(
                (glint.color[2] - 200.0 / 255.0).abs() < 1e-6,
                "a warm white"
            );
        }
    }

    #[test]
    fn a_bigger_magnitude_makes_the_flash_and_flare_bigger() {
        let size = |magnitude| electric_glints(&mut Rng::seeded(&[3]), magnitude)[1].size.0;
        assert!((size(2) - 2.0 * size(1)).abs() < 1e-3);
    }

    #[test]
    fn fading_sparks_dim_in_a_line_and_the_rest_hold_until_they_go() {
        assert_eq!(brightness(0.0, 2.0, true), 1.0);
        assert!((brightness(1.0, 2.0, true) - 0.5).abs() < 1e-6);
        assert_eq!(brightness(3.0, 2.0, true), 0.0);
        assert_eq!(brightness(1.9, 2.0, false), 1.0);
    }
}
