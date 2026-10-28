//! The gibs a broken prop scatters.
//!
//! When the server says a prop broke, the client throws its pieces: the
//! models the prop's `prop_data` names (wood chunks, metal gibs, glass
//! shards), flung out and bouncing on the map until they lie still, then
//! shrinking away. They are only for show — the server knows nothing of
//! them — but each is a real `Box3D` body with the model's `.phy` convex
//! pieces (or its bounding box), colliding with the map and nothing else.

use std::sync::{Mutex, PoisonError};

use b3::body::{
    body_get_transform, body_is_valid, body_set_angular_velocity, body_set_linear_velocity,
    create_body, destroy_body,
};
use b3::hull::{HullData, make_offset_box_hull};
use b3::math_functions::Vec3 as B3Vec3;
use b3::shape::create_hull_shape;
use b3::types::{BodyType, ShapeDef};
use b3::world::World;
use bevy::prelude::*;
use box3d_rust as b3;
use rl_assets::{ModelAsset, coords};
use rl_data::GameData;
use rl_sim::physics::UNITS_PER_METRE;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::net::Game;
use crate::player::Ticked;
use crate::props::{ModelHandles, Moving, Prop, spawn_model};
use crate::visual_world::{DEBRIS_GROUP, WORLD_GROUP, build_static_world, filter};

/// Most gibs one break throws.
const MAX_GIBS: u32 = 10;
/// Most pieces one break throws: a model's own chunks, all of them.
const MAX_PIECES: usize = 16;
/// How much a piece's `burst` adds to its speed, units/s per point.
const BURST: f32 = 1.5;
/// How long, seconds, a piece may linger, whatever its model says.
const PIECE_LIFE: (f32, f32) = (4.0, 30.0);
/// Gibs thrown by a break whose prop does not say how many.
const DEFAULT_GIBS: u32 = 4;
/// Seconds of physics per fixed step.
const STEP: f32 = 1.0 / 60.0;
/// Most fixed steps one frame may run.
const MAX_STEPS: usize = 4;
/// `Box3D` sub-steps per fixed step, as in the engine's samples.
const SUB_STEPS: i32 = 4;
/// Seconds a gib lasts, and how many of them are spent shrinking away.
const LIFETIME: f32 = 7.0;
const SHRINK: f32 = 1.0;
/// Speed range of a gib thrown by an ordinary break, units/s.
const THROW: (f32, f32) = (120.0, 380.0);
/// An exploding prop throws its gibs this much harder.
const BLAST_FACTOR: f32 = 2.2;
/// Spin range, radians/s.
const SPIN: f32 = 12.0;
/// Smallest collider half-extent, Source units.
const MIN_HALF_EXTENT: f32 = 0.05 * UNITS_PER_METRE;

/// Throws gibs and moves them.
pub struct DebrisPlugin;

impl Plugin for DebrisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebrisPhysics>()
            .add_systems(OnExit(AppState::InGame), reset)
            .add_systems(
                Update,
                (throw_gibs, make_gib_bodies, move_gibs)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The `Box3D` world the client's debris lives in.
#[derive(Resource, Default)]
struct DebrisPhysics {
    world: Option<Mutex<World>>,
    /// Leftover time, so physics always advances in fixed 1/60 s steps.
    accumulator: f32,
}

impl DebrisPhysics {
    /// The world, built from the map's collision when first needed.
    fn world(&mut self, sim: &rl_sim::simulation::Simulation) -> &mut World {
        self.world
            .get_or_insert_with(|| Mutex::new(build_static_world(sim, DEBRIS_GROUP)))
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn current(&mut self) -> Option<&mut World> {
        self.world
            .as_mut()
            .map(|world| world.get_mut().unwrap_or_else(PoisonError::into_inner))
    }
}

fn reset(mut physics: ResMut<DebrisPhysics>) {
    *physics = DebrisPhysics::default();
}

/// One piece flying about.
#[derive(Component, Debug)]
struct Gib {
    /// Where it is, Source space.
    position: Vec3,
    /// How it moves, units/s.
    velocity: Vec3,
    /// How it tumbles, radians/s, Source axes.
    spin: Vec3,
    /// Its source-space orientation.
    rotation: Quat,
    /// Seconds since it was thrown.
    age: f32,
    /// Seconds it lasts, the last [`SHRINK`] of them shrinking away.
    life: f32,
    /// Its body, once its model has loaded.
    body: Option<b3::BodyId>,
}

fn throw_gibs(
    mut commands: Commands,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    for Ticked(events) in ticks.read() {
        for broke in &events.prop_breaks {
            let Some(spec) = sim.breakable(broke.body) else {
                continue;
            };
            let mut rng = Rng::seeded(&[sim.tick_count(), u64::from(broke.body.index() as u32)]);
            let factor = if broke.exploded { BLAST_FACTOR } else { 1.0 };
            if !spec.pieces.is_empty() {
                for piece in spec.pieces.iter().take(MAX_PIECES) {
                    let (x, y) = rng.in_disc();
                    let up = 0.3 + 0.7 * rng.unit();
                    let direction = Vec3::new(x, y, up).normalize_or(Vec3::Z);
                    let speed =
                        (THROW.0 + (THROW.1 - THROW.0) * rng.unit() + piece.burst * BURST) * factor;
                    let spin = random_spin(&mut rng);
                    let entity = spawn_model(
                        &mut commands,
                        &mut models,
                        &asset_server,
                        &piece.model,
                        Transform {
                            translation: coords::position(broke.origin.to_array()),
                            rotation: coords::rotation_from_source(broke.rotation),
                            scale: Vec3::ONE,
                        },
                    );
                    commands.entity(entity).insert((
                        Gib {
                            position: broke.origin,
                            velocity: broke.velocity + direction * speed,
                            spin,
                            rotation: broke.rotation,
                            age: 0.0,
                            life: if piece.fade > 0.0 {
                                piece.fade.clamp(PIECE_LIFE.0, PIECE_LIFE.1)
                            } else {
                                LIFETIME
                            },
                            body: None,
                        },
                        Moving,
                    ));
                }
                continue;
            }
            if spec.gibs.is_empty() {
                continue;
            }
            let count = match spec.gib_count {
                0 => DEFAULT_GIBS,
                count => count.min(MAX_GIBS),
            };
            for _ in 0..count {
                let model = &spec.gibs[rng.below(spec.gibs.len())];
                let (x, y) = rng.in_disc();
                let up = 0.35 + 0.65 * rng.unit();
                let direction = Vec3::new(x, y, up).normalize_or(Vec3::Z);
                let speed = (THROW.0 + (THROW.1 - THROW.0) * rng.unit()) * factor;
                let position = broke.position + direction * 6.0;
                let entity = spawn_model(
                    &mut commands,
                    &mut models,
                    &asset_server,
                    model,
                    Transform::from_translation(coords::position(position.to_array())),
                );
                commands.entity(entity).insert((
                    Gib {
                        position,
                        velocity: direction * speed,
                        spin: random_spin(&mut rng),
                        rotation: Quat::IDENTITY,
                        age: 0.0,
                        life: LIFETIME,
                        body: None,
                    },
                    Moving,
                ));
            }
        }
    }
}

fn random_spin(rng: &mut Rng) -> Vec3 {
    Vec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5) * (2.0 * SPIN)
}

/// Creates a body for each gib once its model's assets are loaded.
fn make_gib_bodies(
    mut physics: ResMut<DebrisPhysics>,
    game: Game,
    assets: Res<Assets<ModelAsset>>,
    mut gibs: Query<(&mut Gib, &Prop)>,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let world = physics.world(sim);
    for (mut gib, prop) in &mut gibs {
        if gib.body.is_some() {
            continue;
        }
        let Some(model) = assets.get(&prop.model) else {
            continue;
        };
        if let Some(body) = add_gib_body(world, model, &gib, sim.data()) {
            gib.body = Some(body);
        } else {
            tracing::debug!(model = ?prop.model, "gib model has no usable collision");
        }
    }
}

/// Advances every debris body, poses its entity, and removes old pieces.
fn move_gibs(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    mut physics: ResMut<DebrisPhysics>,
    mut gibs: Query<(Entity, &mut Gib, &mut Transform)>,
) {
    if game.sim().is_none() {
        return;
    }
    let frame = time.delta_secs().min(STEP * MAX_STEPS as f32);
    let mut expired = Vec::new();
    for (entity, mut gib, _) in &mut gibs {
        gib.age += frame;
        if gib.age >= gib.life {
            expired.push((entity, gib.body));
        }
    }

    physics.accumulator += frame;
    let steps = ((physics.accumulator / STEP).floor() as usize).min(MAX_STEPS);
    physics.accumulator -= STEP * steps as f32;
    if let Some(world) = physics.current() {
        for _ in 0..steps {
            world.step(STEP, SUB_STEPS);
        }
        for (_, gib, mut transform) in &mut gibs {
            let Some(body) = gib.body else {
                continue;
            };
            if !body_is_valid(world, body) {
                continue;
            }
            let body_transform = body_get_transform(world, body);
            let position = b3v_to_glam(body_transform.p);
            transform.translation = coords::position(position.to_array());
            transform.rotation = coords::rotation_from_source(b3q_to_glam(body_transform.q));
            let shrink = ((gib.life - gib.age) / SHRINK).clamp(0.0, 1.0);
            transform.scale = Vec3::splat(shrink);
        }
    }

    for (entity, body) in expired {
        if let (Some(world), Some(body)) = (physics.current(), body)
            && body_is_valid(world, body)
        {
            destroy_body(world, body);
        }
        commands.entity(entity).try_despawn();
    }
}

/// Builds a debris body from a gib model's `.phy`, falling back to its
/// bounds. The shape collides only with the map.
fn add_gib_body(
    world: &mut World,
    model: &ModelAsset,
    gib: &Gib,
    data: &GameData,
) -> Option<b3::BodyId> {
    let (hulls, volume) = gib_hulls(model);
    if hulls.is_empty() {
        return None;
    }
    let material = data.physics.materials.get(&model.surface_prop);
    let mass = model.physics.as_ref().map(|physics| physics.mass());
    let density = match mass {
        Some(mass) if mass > 0.0 && volume > 0.0 => mass / volume,
        _ => material.density / (UNITS_PER_METRE * UNITS_PER_METRE * UNITS_PER_METRE),
    }
    .max(f32::EPSILON);

    let mut body_def = b3::default_body_def();
    body_def.type_ = BodyType::Dynamic;
    body_def.position = b3v(gib.position);
    body_def.rotation = b3q(gib.rotation);
    body_def.linear_damping = 0.05;
    body_def.angular_damping = 0.2;
    body_def.is_bullet = true;
    let body = create_body(world, &body_def);

    let mut shape_def = ShapeDef {
        density,
        ..ShapeDef::default()
    };
    shape_def.base_material.friction = material.friction;
    shape_def.base_material.restitution = 0.12;
    shape_def.filter = filter(DEBRIS_GROUP, WORLD_GROUP);
    let mut created = false;
    for hull in &hulls {
        created |= create_hull_shape(world, body, &shape_def, hull).index1 != 0;
    }
    if !created {
        destroy_body(world, body);
        return None;
    }
    body_set_linear_velocity(world, body, b3v(gib.velocity));
    body_set_angular_velocity(world, body, b3v(gib.spin));
    Some(body)
}

/// The gib model's convex hulls and their total volume.
fn gib_hulls(model: &ModelAsset) -> (Vec<HullData>, f32) {
    let mut hulls = Vec::new();
    let mut volume = 0.0;
    if let Some(physics) = &model.physics {
        for solid in &physics.solids {
            for piece in &solid.pieces {
                let points: Vec<B3Vec3> = piece
                    .vertices
                    .iter()
                    .map(|point| b3v(Vec3::from(*point)))
                    .collect();
                if let Some(hull) = b3::hull::create_hull(&points, b3::constants::MAX_HULL_VERTICES)
                {
                    volume += hull.volume.max(0.0);
                    hulls.push(hull);
                }
            }
        }
    }
    if hulls.is_empty() {
        let (mins, maxs) = model.source_bounds;
        let half = ((Vec3::from(maxs) - Vec3::from(mins)) * 0.5).max(Vec3::splat(MIN_HALF_EXTENT));
        let center = (Vec3::from(mins) + Vec3::from(maxs)) * 0.5;
        let box_hull = make_offset_box_hull(half.x, half.y, half.z, b3v(center)).base;
        volume = box_hull.volume.max(0.0);
        hulls.push(box_hull);
    }
    (hulls, volume)
}

fn b3v(value: Vec3) -> B3Vec3 {
    B3Vec3 {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}

fn b3v_to_glam(value: B3Vec3) -> Vec3 {
    Vec3::new(value.x, value.y, value.z)
}

fn b3q(value: Quat) -> b3::Quat {
    b3::Quat {
        v: b3v(value.xyz()),
        s: value.w,
    }
}

fn b3q_to_glam(value: b3::Quat) -> Quat {
    Quat::from_xyzw(value.v.x, value.v.y, value.v.z, value.s)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rl_sim::collision::WorldBuilder;
    use rl_sim::simulation::Simulation;

    use super::*;

    fn floor() -> Simulation {
        let world = WorldBuilder::new()
            .solid_box(
                Vec3::new(-500.0, -500.0, -64.0),
                Vec3::new(500.0, 500.0, 0.0),
            )
            .build();
        Simulation::new(Arc::new(world), &GameData::builtin().expect("builtin"))
    }

    fn box_model() -> ModelAsset {
        ModelAsset {
            parts: Vec::new(),
            skins: Vec::new(),
            bounds: (Vec3::splat(-3.0), Vec3::splat(3.0)),
            checksum: 0,
            strip_vertex_ids: Vec::new(),
            source_bounds: ([-3.0; 3], [3.0; 3]),
            surface_prop: "wood".to_owned(),
            physics: None,
            animation: None,
            root_idle: None,
        }
    }

    #[test]
    fn a_thrown_gib_falls_bounces_and_comes_to_rest_on_the_floor() {
        let sim = floor();
        let mut world = build_static_world(&sim, DEBRIS_GROUP);
        let gib = Gib {
            position: Vec3::new(0.0, 0.0, 40.0),
            velocity: Vec3::new(150.0, 0.0, 200.0),
            spin: Vec3::ONE,
            rotation: Quat::IDENTITY,
            age: 0.0,
            life: LIFETIME,
            body: None,
        };
        let body = add_gib_body(&mut world, &box_model(), &gib, sim.data()).expect("body");
        let mut lowest = f32::MAX;
        for _ in 0..600 {
            world.step(STEP, SUB_STEPS);
            lowest = lowest.min(b3v_to_glam(b3::body::body_get_world_center(&world, body)).z);
        }
        let rest = b3v_to_glam(body_get_transform(&world, body).p);
        assert!(lowest > -1.0, "never through the floor");
        assert!((2.0..4.0).contains(&rest.z), "on the floor: {rest}");
        assert!(rest.x > 0.0, "travelled on: {rest}");
    }
}
