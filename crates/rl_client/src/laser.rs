//! The RPG's laser dot: a red dot where a launcher's laser meets the world,
//! for every player who holds one with its laser on. It is what their rocket
//! steers by ([`Simulation::laser_dot`]).

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::simulation::PlayerId;

use crate::AppState;
use crate::camera::FlyCam;
use crate::net::Game;
use crate::particles::Looks;
use crate::world::MapScoped;

/// Size of the dot, units.
const DOT_SIZE: f32 = 5.0;
/// Colour of the dot, linear and brighter than white so it glows.
const DOT_COLOR: [f32; 3] = [1.8, 0.06, 0.03];

/// Draws launchers' laser dots.
pub struct LaserPlugin;

impl Plugin for LaserPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, draw_dots.run_if(in_state(AppState::InGame)));
    }
}

/// The dot of a player's laser.
#[derive(Component)]
struct LaserDot(PlayerId);

fn draw_dots(
    mut commands: Commands,
    game: Game,
    looks: Option<Res<Looks>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut dots: Query<(Entity, &LaserDot, &mut Transform, &mut Visibility)>,
) {
    let (Some(sim), Some(looks), Ok(camera)) = (game.sim(), looks, camera.single()) else {
        return;
    };
    let eye = camera.translation();
    let wanted: HashMap<PlayerId, Vec3> = sim
        .players()
        .filter_map(|(id, _)| Some((id, coords::position(sim.laser_dot(id)?.to_array()))))
        .collect();
    let mut have = Vec::new();
    for (entity, dot, mut transform, mut visibility) in &mut dots {
        match wanted.get(&dot.0) {
            Some(&at) => {
                have.push(dot.0);
                // A little towards the eye, so the dot is not buried in the
                // surface it sits on.
                let towards = (eye - at).normalize_or_zero() * 0.02;
                let size = DOT_SIZE * coords::METERS_PER_UNIT;
                *transform = Transform {
                    translation: at + towards,
                    rotation: camera.rotation(),
                    scale: Vec3::splat(size),
                };
                *visibility = Visibility::Visible;
            }
            None => commands.entity(entity).try_despawn(),
        }
    }
    for &id in wanted.keys().filter(|id| !have.contains(id)) {
        let [r, g, b] = DOT_COLOR;
        commands.spawn((
            Mesh3d(looks.quad.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::linear_rgba(r, g, b, 1.0),
                base_color_texture: Some(looks.dot.clone()),
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Add,
                ..default()
            })),
            Transform::from_scale(Vec3::ZERO),
            LaserDot(id),
            MapScoped,
        ));
    }
}
