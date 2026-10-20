//! Combat effects: bullet tracers, impact marks and explosion flashes.
//!
//! These are placeholders drawn without content (sprites and particle
//! systems from the mounted game come with M7): a tracer is a short-lived
//! line from just below the shooter's eyes to where the bullet stopped, an
//! impact a small cross where it hit, and an explosion a glowing ball that
//! swells and fades.

use bevy::prelude::*;
use rl_assets::coords;
use rl_data::Shot;
use rl_sim::combat::hitscan::Struck;

use crate::AppState;
use crate::net::Game;
use crate::player::Ticked;
use crate::world::MapScoped;

/// Seconds a tracer stays visible.
const TRACER_SECS: f32 = 0.07;
/// Seconds an impact mark stays visible.
const IMPACT_SECS: f32 = 0.25;
/// How far below and right of the eyes tracers start, units: about where
/// a gun is held, so a player's own tracers are seen.
const TRACER_DROP: f32 = 6.0;
const TRACER_SIDE: f32 = 5.0;
/// Size of an impact mark, units.
const IMPACT_SIZE: f32 = 3.0;
/// Seconds an explosion flash lasts...
const FLASH_SECS: f32 = 0.35;
/// ...growing to this fraction of the blast's radius.
const FLASH_REACH: f32 = 0.6;

/// Draws combat effects.
pub struct EffectsPlugin;

impl Plugin for EffectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Lines>()
            .add_systems(OnEnter(AppState::InGame), clear)
            .add_systems(
                Update,
                (collect, draw_lines, fade_flashes)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Tracers and impacts being drawn: Bevy-space segments and when each was
/// made.
#[derive(Resource, Default)]
struct Lines {
    tracers: Vec<(Vec3, Vec3, f32)>,
    impacts: Vec<(Vec3, f32)>,
}

/// An explosion's glowing ball.
#[derive(Component)]
struct Flash {
    born: f32,
    /// Full size, metres.
    radius: f32,
}

fn clear(mut lines: ResMut<Lines>) {
    *lines = Lines::default();
}

/// Turns the tick events' shots and explosions into effects.
fn collect(
    mut commands: Commands,
    time: Res<Time>,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    mut lines: ResMut<Lines>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    let now = time.elapsed_secs();
    for Ticked(events) in ticks.read() {
        for shot in &events.shots {
            let bullet = sim.arsenal().weapon(shot.weapon).is_some_and(|weapon| {
                weapon
                    .fire_modes()
                    .any(|fire| matches!(fire.shot, Shot::Hitscan(_)))
            });
            let end = coords::position(shot.trace.end.to_array());
            if bullet {
                let yaw = sim
                    .player(shot.player)
                    .map_or(0.0, |player| player.look.1)
                    .to_radians();
                let right = Vec3::new(yaw.sin(), -yaw.cos(), 0.0);
                let start = shot.start - Vec3::Z * TRACER_DROP + right * TRACER_SIDE;
                lines
                    .tracers
                    .push((coords::position(start.to_array()), end, now));
            }
            if !matches!(shot.trace.struck, Struck::Nothing) {
                lines.impacts.push((end, now));
            }
        }
        for explosion in &events.explosions {
            commands.spawn((
                Flash {
                    born: now,
                    radius: explosion.radius * FLASH_REACH * coords::METERS_PER_UNIT,
                },
                Mesh3d(meshes.add(Sphere::new(1.0))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgba(1.0, 0.7, 0.25, 0.7),
                    unlit: true,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                })),
                Transform::from_translation(coords::position(explosion.position.to_array()))
                    .with_scale(Vec3::splat(0.01)),
                MapScoped,
            ));
        }
    }
}

/// Draws the live tracers and impacts; forgets old ones.
fn draw_lines(time: Res<Time>, mut lines: ResMut<Lines>, mut gizmos: Gizmos) {
    let now = time.elapsed_secs();
    lines.tracers.retain(|&(.., at)| now - at < TRACER_SECS);
    lines.impacts.retain(|&(_, at)| now - at < IMPACT_SECS);
    for &(start, end, _) in &lines.tracers {
        gizmos.line(start, end, Color::srgba(1.0, 0.92, 0.6, 0.9));
    }
    let size = IMPACT_SIZE * coords::METERS_PER_UNIT;
    for &(point, _) in &lines.impacts {
        for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
            gizmos.line(
                point - axis * size,
                point + axis * size,
                Color::srgb(1.0, 0.85, 0.4),
            );
        }
    }
}

/// Swells and fades explosion flashes, then removes them.
fn fade_flashes(
    mut commands: Commands,
    time: Res<Time>,
    mut flashes: Query<(
        Entity,
        &Flash,
        &mut Transform,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let now = time.elapsed_secs();
    for (entity, flash, mut transform, material) in &mut flashes {
        let t = (now - flash.born) / FLASH_SECS;
        if t >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }
        transform.scale = Vec3::splat(flash.radius * (0.3 + 0.7 * t.sqrt()));
        if let Some(mut material) = materials.get_mut(&material.0) {
            material.base_color.set_alpha(0.7 * (1.0 - t));
        }
    }
}
