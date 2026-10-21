//! Grenades in flight: the model, the red dot, the red trail and the
//! ticking.
//!
//! A grenade is a physics body that exists only on the server; the client
//! is sent where it is and how it is turned ([`GrenadeView`]) and draws it
//! where [`Drawn`] places it. Besides its model it shows what the original's
//! fuse does:
//!
//! - a **red dot** on its top that flashes with every tick of the fuse and
//!   dims in between;
//! - a **red trail** of where it has been, tapering away behind it;
//! - the **tick** itself, the weapon's `blip` sound, played where it is.
//!
//! The ticks follow from the grenade's age alone (see
//! [`rl_data::BlipConfig::last`]), so the dot and the sound agree. The age
//! comes with each snapshot; between snapshots it is carried on by the
//! local clock, so the flashes do not stutter at the snapshot rate.

use std::collections::{HashMap, VecDeque};

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use rl_assets::coords;
use rl_data::{GrenadeEffectConfig, ProjectileBodyConfig, ProjectileConfig, Shot};
use rl_sim::simulation::{GrenadeView, Simulation};

use crate::AppState;
use crate::audio::SoundPlayer;
use crate::camera::FlyCam;
use crate::net::{Drawn, Game};
use crate::particles::Looks;
use crate::props::{ModelHandles, Moving, spawn_model};
use crate::world::MapScoped;

/// Draws grenades.
pub struct GrenadesPlugin;

impl Plugin for GrenadesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Fuses>().add_systems(
            Update,
            (draw_grenades, tick_fuses, flash_dots, draw_trails)
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// The model of a grenade, by the id the server gave it.
#[derive(Component)]
struct GrenadeEntity(u32);

/// The red dot of a grenade.
#[derive(Component)]
struct Dot(u32);

/// The red trail of a grenade: where it has been, newest first (seconds
/// since startup, Bevy space).
#[derive(Component)]
struct Trail {
    id: u32,
    points: VecDeque<(f32, Vec3)>,
}

/// A grenade's fuse as the client keeps time on it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fuse {
    /// The age the last snapshot gave, seconds...
    age: f32,
    /// ...and when it came, seconds since startup.
    at: f32,
    /// The latest tick that has sounded.
    blip: Option<u32>,
}

impl Fuse {
    /// The grenade's age at `now`.
    fn age_at(&self, now: f32) -> f32 {
        self.age + (now - self.at).max(0.0)
    }
}

/// The fuses of the grenades in flight.
#[derive(Resource, Default)]
struct Fuses(HashMap<u32, Fuse>);

/// The grenade `view` is of, if it is a projectile body: its shot and body
/// data.
fn body_of<'a>(
    sim: &'a Simulation,
    view: &GrenadeView,
) -> Option<(&'a ProjectileConfig, &'a ProjectileBodyConfig)> {
    match &sim.arsenal().fire(view.weapon, view.mode)?.shot {
        Shot::Projectile(projectile) => Some((projectile, projectile.body.as_ref()?)),
        _ => None,
    }
}

/// Where a grenade's model goes (Bevy space): the model's own middle is
/// laid on the body's centre, turned as the body is.
fn model_transform(view: &GrenadeView, body: &ProjectileBodyConfig) -> Transform {
    let rotation = coords::rotation_from_source(view.rotation);
    let centre = coords::position(view.position.to_array());
    Transform {
        translation: centre - rotation * coords::position(body.model_center),
        rotation,
        ..default()
    }
}

/// Where the dot sits: on the grenade's top, in the grenade's frame.
fn dot_position(view: &GrenadeView, effects: &GrenadeEffectConfig) -> Vec3 {
    let rotation = coords::rotation_from_source(view.rotation);
    coords::position(view.position.to_array()) + rotation * coords::position(effects.dot_offset)
}

/// How bright the dot is, `0..=1`, `since` seconds after a tick: full at
/// the tick, fading to its dim between ticks.
fn dot_brightness(effects: &GrenadeEffectConfig, since: f32) -> f32 {
    let left = (1.0 - since / effects.flash_fade).clamp(0.0, 1.0);
    effects.dot_dim + (1.0 - effects.dot_dim) * left * left
}

/// Places every grenade where it is drawn this frame, turned as it
/// tumbles: new ones get a model, a dot and a trail, gone ones lose them.
fn draw_grenades(
    mut commands: Commands,
    drawn: Res<Drawn>,
    game: Game,
    looks: Option<Res<Looks>>,
    mut models: ResMut<ModelHandles>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    asset_server: Res<AssetServer>,
    mut views: Query<(Entity, &GrenadeEntity, &mut Transform)>,
    dots: Query<(Entity, &Dot)>,
    trails: Query<(Entity, &Trail)>,
) {
    let (Some(world), Some(sim)) = (&drawn.0, game.sim()) else {
        return;
    };
    let mut shown = Vec::new();
    for (entity, view, mut transform) in &mut views {
        let grenade = world.grenades.iter().find(|grenade| grenade.id == view.0);
        match grenade.and_then(|grenade| Some((grenade, body_of(sim, grenade)?.1))) {
            Some((grenade, body)) => {
                *transform = model_transform(grenade, body);
                shown.push(view.0);
            }
            None => commands.entity(entity).try_despawn(),
        }
    }
    let gone = |id: u32| !world.grenades.iter().any(|grenade| grenade.id == id);
    for (entity, dot) in &dots {
        if gone(dot.0) {
            commands.entity(entity).try_despawn();
        }
    }
    for (entity, trail) in &trails {
        if gone(trail.id) {
            commands.entity(entity).try_despawn();
        }
    }
    let Some(looks) = looks else {
        return;
    };
    for grenade in &world.grenades {
        if shown.contains(&grenade.id) {
            continue;
        }
        let Some((projectile, body)) = body_of(sim, grenade) else {
            continue;
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            &projectile.model,
            model_transform(grenade, body),
        );
        commands
            .entity(entity)
            .insert((GrenadeEntity(grenade.id), Moving));
        commands.spawn((
            Mesh3d(looks.quad.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::NONE,
                base_color_texture: Some(looks.dot.clone()),
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Add,
                ..default()
            })),
            Transform::from_scale(Vec3::ZERO),
            Dot(grenade.id),
            MapScoped,
        ));
        commands.spawn((
            Mesh3d(meshes.add(empty_ribbon())),
            MeshMaterial3d(materials.add(StandardMaterial {
                unlit: true,
                double_sided: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Add,
                ..default()
            })),
            Transform::IDENTITY,
            // Its vertices are in the world, and change every frame.
            NoFrustumCulling,
            Visibility::Hidden,
            Trail {
                id: grenade.id,
                points: VecDeque::new(),
            },
            MapScoped,
        ));
    }
}

/// Sounds the tick of every grenade whose fuse has come to one.
fn tick_fuses(
    time: Res<Time>,
    drawn: Res<Drawn>,
    game: Game,
    mut fuses: ResMut<Fuses>,
    mut player: SoundPlayer,
) {
    let (Some(world), Some(sim)) = (&drawn.0, game.sim()) else {
        return;
    };
    let now = time.elapsed_secs();
    fuses
        .0
        .retain(|id, _| world.grenades.iter().any(|g| g.id == *id));
    for grenade in &world.grenades {
        let fuse = fuses.0.entry(grenade.id).or_insert(Fuse {
            age: grenade.age,
            at: now,
            blip: None,
        });
        // A newer snapshot corrects the clock.
        if grenade.age > fuse.age {
            fuse.age = grenade.age;
            fuse.at = now;
        }
        let Some((projectile, body)) = body_of(sim, grenade) else {
            continue;
        };
        let (Some(blip), Some(fuse_time)) = (&body.blip, projectile.fuse) else {
            continue;
        };
        let (number, _) = blip.last(fuse.age_at(now), fuse_time);
        if fuse.blip == Some(number) {
            continue;
        }
        fuse.blip = Some(number);
        let sound = sim
            .arsenal()
            .weapon(grenade.weapon)
            .and_then(|weapon| weapon.sounds.get("blip"));
        if let Some(sound) = sound {
            player.play(sound, Some(grenade.position), 1.0);
        }
    }
}

/// Flashes each grenade's dot with its ticks, facing the camera.
fn flash_dots(
    time: Res<Time>,
    drawn: Res<Drawn>,
    game: Game,
    fuses: Res<Fuses>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut dots: Query<(&Dot, &MeshMaterial3d<StandardMaterial>, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Some(world), Some(sim), Ok(camera)) = (&drawn.0, game.sim(), camera.single()) else {
        return;
    };
    let effects = &sim.data().effects.grenade;
    let now = time.elapsed_secs();
    let facing = camera.rotation();
    for (dot, material, mut transform) in &mut dots {
        let Some(grenade) = world.grenades.iter().find(|grenade| grenade.id == dot.0) else {
            continue;
        };
        let Some(fuse) = fuses.0.get(&dot.0) else {
            continue;
        };
        let Some((projectile, body)) = body_of(sim, grenade) else {
            continue;
        };
        let age = fuse.age_at(now);
        let since = match (&body.blip, projectile.fuse) {
            (Some(blip), Some(fuse_time)) => age - blip.last(age, fuse_time).1,
            _ => age,
        };
        let brightness = dot_brightness(effects, since);
        let size = effects.dot_size * coords::METERS_PER_UNIT * (0.6 + 0.4 * brightness);
        *transform = Transform {
            translation: dot_position(grenade, effects),
            rotation: facing,
            scale: Vec3::splat(size),
        };
        if let Some(mut material) = materials.get_mut(&material.0) {
            let [r, g, b] = effects.dot_color;
            material.base_color = Color::linear_rgba(r, g, b, brightness);
        }
    }
}

/// A ribbon with nothing to show yet: a degenerate sliver (a mesh without
/// vertices is a warning), kept hidden.
fn empty_ribbon() -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleStrip,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0_f32; 3]; 4]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![[0.0_f32; 4]; 4]);
    mesh
}

/// The vertices of a trail's ribbon.
struct Ribbon {
    positions: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
}

/// The ribbon through `points` (newest first, with their age in seconds):
/// two vertices across the line at each, turned to face `eye`, narrowing
/// from `width.0` to `width.1` and fading out over `life`. `None` with
/// fewer than two points.
fn ribbon(
    points: &[(f32, Vec3)],
    now: f32,
    eye: Vec3,
    effects: &GrenadeEffectConfig,
) -> Option<Ribbon> {
    if points.len() < 2 {
        return None;
    }
    let mut positions = Vec::with_capacity(points.len() * 2);
    let mut colors = Vec::with_capacity(points.len() * 2);
    for (index, &(at, point)) in points.iter().enumerate() {
        let ahead = points.get(index.wrapping_sub(1)).map_or(point, |p| p.1);
        let behind = points.get(index + 1).map_or(point, |p| p.1);
        let along = (ahead - behind).normalize_or_zero();
        let across = along.cross(eye - point).normalize_or_zero();
        let age = ((now - at) / effects.trail_life).clamp(0.0, 1.0);
        let (near, far) = effects.trail_width;
        let half = (near + (far - near) * age) * coords::METERS_PER_UNIT / 2.0;
        let [r, g, b] = effects.trail_color;
        let alpha = effects.trail_alpha * (1.0 - age);
        let color = [r * alpha, g * alpha, b * alpha, alpha];
        positions.push((point + across * half).to_array());
        positions.push((point - across * half).to_array());
        colors.extend([color, color]);
    }
    Some(Ribbon { positions, colors })
}

/// Lays down each grenade's trail behind it and draws it.
fn draw_trails(
    time: Res<Time>,
    drawn: Res<Drawn>,
    game: Game,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut trails: Query<(&mut Trail, &Mesh3d, &mut Visibility)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(world), Some(sim), Ok(camera)) = (&drawn.0, game.sim(), camera.single()) else {
        return;
    };
    let effects = &sim.data().effects.grenade;
    let now = time.elapsed_secs();
    for (mut trail, mesh, mut visibility) in &mut trails {
        if let Some(grenade) = world.grenades.iter().find(|g| g.id == trail.id) {
            let here = coords::position(grenade.position.to_array());
            let moved = trail.points.front().is_none_or(|&(_, last)| {
                last.distance(here) > TRAIL_STEP * coords::METERS_PER_UNIT
            });
            if moved {
                trail.points.push_front((now, here));
            }
        }
        while trail
            .points
            .back()
            .is_some_and(|&(at, _)| now - at > effects.trail_life)
        {
            trail.points.pop_back();
        }
        let points: Vec<(f32, Vec3)> = trail.points.iter().copied().collect();
        let Some(ribbon) = ribbon(&points, now, camera.translation(), effects) else {
            *visibility = Visibility::Hidden;
            continue;
        };
        *visibility = Visibility::Inherited;
        if let Some(mut mesh) = meshes.get_mut(&mesh.0) {
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, ribbon.positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, ribbon.colors);
        }
    }
}

/// A trail gets a new point once the grenade has moved this far, units.
const TRAIL_STEP: f32 = 0.75;

#[cfg(test)]
mod tests {
    use rl_data::GameData;

    use super::*;

    fn effects() -> GrenadeEffectConfig {
        GameData::builtin().expect("data").effects.grenade
    }

    fn view(position: Vec3, rotation: Quat) -> GrenadeView {
        GrenadeView {
            id: 1,
            body: rl_sim::physics::BodyId::from_index(3),
            weapon: rl_sim::weapons::WeaponId::from_index(0),
            mode: rl_sim::weapons::Mode::Primary,
            position,
            rotation,
            age: 0.0,
        }
    }

    fn body() -> ProjectileBodyConfig {
        let data = GameData::builtin().expect("data");
        let frag = data.weapons.get("frag").expect("frag");
        match &frag.primary.as_ref().expect("primary").shot {
            Shot::Projectile(projectile) => projectile.body.clone().expect("body"),
            _ => unreachable!("frag throws a body"),
        }
    }

    #[test]
    fn the_models_middle_sits_on_the_bodys_centre() {
        let body = body();
        let centre = Vec3::new(100.0, 50.0, 20.0);
        for rotation in [
            Quat::IDENTITY,
            Quat::from_rotation_x(1.0),
            Quat::from_euler(EulerRot::XYZ, 0.3, 2.0, -1.1),
        ] {
            let transform = model_transform(&view(centre, rotation), &body);
            // The model's own middle, carried by its transform.
            let middle =
                transform.translation + transform.rotation * coords::position(body.model_center);
            let wanted = coords::position(centre.to_array());
            assert!(middle.abs_diff_eq(wanted, 1e-5), "{middle} vs {wanted}");
        }
    }

    #[test]
    fn the_dot_rides_on_the_top_of_a_turned_grenade() {
        let effects = effects();
        let upright = dot_position(&view(Vec3::ZERO, Quat::IDENTITY), &effects);
        assert!(upright.y > 0.0, "above the middle (Bevy up is +Y)");
        let on_its_side = dot_position(
            &view(
                Vec3::ZERO,
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            ),
            &effects,
        );
        assert!(on_its_side.y.abs() < 1e-5, "{on_its_side}");
        assert!((upright.length() - on_its_side.length()).abs() < 1e-5);
    }

    #[test]
    fn the_dot_flashes_at_a_tick_and_dims_between() {
        let effects = effects();
        assert!((dot_brightness(&effects, 0.0) - 1.0).abs() < 1e-6);
        let later = dot_brightness(&effects, effects.flash_fade / 2.0);
        assert!(later < 1.0 && later > effects.dot_dim, "{later}");
        assert!((dot_brightness(&effects, 5.0) - effects.dot_dim).abs() < 1e-6);
    }

    #[test]
    fn fuses_keep_time_between_snapshots() {
        let fuse = Fuse {
            age: 0.4,
            at: 10.0,
            blip: None,
        };
        assert!((fuse.age_at(10.25) - 0.65).abs() < 1e-6);
        assert!((fuse.age_at(9.0) - 0.4).abs() < 1e-6, "never backwards");
    }

    #[test]
    fn the_trail_tapers_fades_and_faces_the_eye() {
        let effects = effects();
        let eye = Vec3::new(0.0, 0.0, 5.0);
        // Moving along +X, newest first.
        let points = [
            (1.0, Vec3::new(2.0, 0.0, 0.0)),
            (0.8, Vec3::new(1.0, 0.0, 0.0)),
            (0.6, Vec3::ZERO),
        ];
        assert!(ribbon(&points[..1], 1.0, eye, &effects).is_none());
        let Ribbon { positions, colors } = ribbon(&points, 1.0, eye, &effects).expect("ribbon");
        assert_eq!((positions.len(), colors.len()), (6, 6));
        // Across the line, in the plane facing the eye (here ±Y).
        for pair in positions.chunks(2) {
            assert!((pair[0][0] - pair[1][0]).abs() < 1e-6);
            assert!((pair[0][2] - pair[1][2]).abs() < 1e-6);
        }
        let width = |index: usize| (positions[index * 2][1] - positions[index * 2 + 1][1]).abs();
        assert!(width(0) > width(2), "narrower at its far end");
        assert!(colors[0][3] > colors[4][3], "fainter at its far end");
        assert!((colors[0][3] - effects.trail_alpha).abs() < 1e-6);
    }
}
