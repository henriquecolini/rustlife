//! The gibs a broken prop scatters.
//!
//! When the server says a prop broke, the client throws its pieces: the
//! models the prop's `prop_data` names (wood chunks, metal gibs, glass
//! shards), flung out and bouncing on the map until they lie still, then
//! shrinking away. They are only for show — the server knows nothing of
//! them — so they are simple: gravity, bounces off the map's brushes, a
//! little friction.

use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::collision::contents::SOLID;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::net::Game;
use crate::player::Ticked;
use crate::props::{ModelHandles, Moving, spawn_model};

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
/// Gravity on gibs, units/s².
const GRAVITY: f32 = 800.0;
/// How much speed a bounce keeps along the surface normal.
const RESTITUTION: f32 = 0.35;
/// How much speed a bounce keeps along the surface.
const SLIDE_KEPT: f32 = 0.6;
/// A gib slower than this lies still, units/s.
const REST_SPEED: f32 = 35.0;
/// Seconds a gib lasts, and how many of them are spent shrinking away.
const LIFETIME: f32 = 7.0;
const SHRINK: f32 = 1.0;
/// Speed range of a gib thrown by an ordinary break, units/s.
const THROW: (f32, f32) = (120.0, 380.0);
/// An exploding prop throws its gibs this much harder.
const BLAST_FACTOR: f32 = 2.2;
/// Spin range, radians/s.
const SPIN: f32 = 12.0;

/// Throws gibs and moves them.
pub struct DebrisPlugin;

impl Plugin for DebrisPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (throw_gibs, move_gibs)
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// One piece flying about.
#[derive(Component, Debug)]
struct Gib {
    /// Where it is, Source space.
    position: Vec3,
    /// How it moves, units/s.
    velocity: Vec3,
    /// How it tumbles, radians/s, about Bevy's axes.
    spin: Vec3,
    /// Seconds since it was thrown.
    age: f32,
    /// Seconds it lasts, the last [`SHRINK`] of them shrinking away.
    life: f32,
    /// It has come to rest.
    at_rest: bool,
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
                    let spin = Vec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5)
                        * (2.0 * SPIN);
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
                            age: 0.0,
                            life: if piece.fade > 0.0 {
                                piece.fade.clamp(PIECE_LIFE.0, PIECE_LIFE.1)
                            } else {
                                LIFETIME
                            },
                            at_rest: false,
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
                let spin =
                    Vec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5) * (2.0 * SPIN);
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
                        spin,
                        age: 0.0,
                        life: LIFETIME,
                        at_rest: false,
                    },
                    Moving,
                ));
            }
        }
    }
}

fn move_gibs(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    mut gibs: Query<(Entity, &mut Gib, &mut Transform)>,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let dt = time.delta_secs().min(0.05);
    for (entity, mut gib, mut transform) in &mut gibs {
        gib.age += dt;
        if gib.age >= gib.life {
            commands.entity(entity).try_despawn();
            continue;
        }
        if !gib.at_rest {
            step(&mut gib, dt, sim);
            let turn = gib.spin * dt;
            transform.rotation = Quat::from_scaled_axis(turn) * transform.rotation;
        }
        transform.translation = coords::position(gib.position.to_array());
        let shrink = ((gib.life - gib.age) / SHRINK).clamp(0.0, 1.0);
        transform.scale = Vec3::splat(shrink);
    }
}

/// Advances a flying gib one frame, bouncing it off the map.
fn step(gib: &mut Gib, dt: f32, sim: &rl_sim::simulation::Simulation) {
    gib.velocity.z -= GRAVITY * dt;
    let next = gib.position + gib.velocity * dt;
    let trace = sim.world().trace_ray(gib.position, next, SOLID);
    if !trace.hit() {
        gib.position = next;
        return;
    }
    gib.position = trace.end + trace.normal * 0.5;
    let along = gib.velocity.dot(trace.normal);
    let slide = gib.velocity - trace.normal * along;
    gib.velocity = slide * SLIDE_KEPT - trace.normal * along * RESTITUTION;
    gib.spin *= 0.7;
    if gib.velocity.length() < REST_SPEED {
        gib.velocity = Vec3::ZERO;
        gib.at_rest = true;
    }
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
        Simulation::new(
            Arc::new(world),
            &rl_data::GameData::builtin().expect("builtin"),
        )
    }

    #[test]
    fn a_thrown_gib_falls_bounces_and_comes_to_rest_on_the_floor() {
        let sim = floor();
        let mut gib = Gib {
            position: Vec3::new(0.0, 0.0, 40.0),
            velocity: Vec3::new(150.0, 0.0, 200.0),
            spin: Vec3::ONE,
            age: 0.0,
            life: LIFETIME,
            at_rest: false,
        };
        let mut lowest = f32::MAX;
        for _ in 0..600 {
            if gib.at_rest {
                break;
            }
            step(&mut gib, 1.0 / 60.0, &sim);
            lowest = lowest.min(gib.position.z);
        }
        assert!(gib.at_rest, "it settles: {gib:?}");
        assert!(
            gib.position.z > -1.0 && gib.position.z < 3.0,
            "on the floor: {gib:?}"
        );
        assert!(lowest > -1.0, "never through it");
        assert!(gib.position.x > 0.0, "travelled on");
    }
}
