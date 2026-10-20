//! Loading a map and placing its geometry in the scene.
//!
//! M1 renders the world with a single neutral material and basic lighting, to
//! prove the full content path (install → VFS → BSP → mesh → screen). Per-face
//! materials, lightmaps and the sky arrive in later M1 increments.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::{MapAsset, SOURCE};

use crate::camera::FlyCam;

/// Name of the map to load, without path or extension (e.g. `dm_lockdown`).
#[derive(Resource)]
struct MapName(String);

/// Handle to the map currently being loaded.
#[derive(Resource)]
struct MapHandle(Handle<MapAsset>);

/// Whether the loaded map's meshes have been spawned yet (one-shot guard).
#[derive(Resource)]
struct MapSpawned(bool);

/// Loads a map and renders its geometry.
pub struct WorldPlugin {
    /// Map name without path or extension.
    pub map_name: String,
}

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MapName(self.map_name.clone()))
            .insert_resource(MapSpawned(false))
            .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
            .add_systems(Startup, (setup_lighting, start_load))
            .add_systems(Update, spawn_when_loaded);
    }
}

fn setup_lighting(mut commands: Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(1.0, 3.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

fn start_load(mut commands: Commands, asset_server: Res<AssetServer>, map_name: Res<MapName>) {
    let path = format!("{SOURCE}://maps/{}.bsp", map_name.0);
    info!("loading map {path}");
    commands.insert_resource(MapHandle(asset_server.load(path)));
}

fn spawn_when_loaded(
    mut commands: Commands,
    mut spawned: ResMut<MapSpawned>,
    handle: Res<MapHandle>,
    maps: Res<Assets<MapAsset>>,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut camera: Query<(&mut Transform, &mut FlyCam)>,
) {
    if spawned.0 {
        return;
    }
    let Some(map) = maps.get(&handle.0) else {
        return;
    };

    // One `StandardMaterial` per distinct material name. As a first texturing
    // pass we load the base texture by the convention that the VTF shares the
    // material's path (`materials/<name>.vtf`), which holds for most HL2 world
    // brushes; the full VMT pipeline ($basetexture, translucency, ...) follows
    // in a later increment. A missing texture leaves the surface its base
    // colour rather than failing.
    let mut cache: HashMap<&str, Handle<StandardMaterial>> = HashMap::new();
    for mesh in &map.meshes {
        let material = cache
            .entry(mesh.material.as_str())
            .or_insert_with(|| {
                let texture =
                    asset_server.load(format!("{SOURCE}://materials/{}.vtf", mesh.material));
                materials.add(StandardMaterial {
                    base_color_texture: Some(texture),
                    perceptual_roughness: 0.9,
                    // Source polygons are not wound for Bevy's front-face
                    // convention, so draw both sides.
                    cull_mode: None,
                    ..default()
                })
            })
            .clone();
        commands.spawn((Mesh3d(mesh.mesh.clone()), MeshMaterial3d(material)));
    }

    // Drop the camera into the middle of the level and scale its speed to the
    // map so flying across it takes a sensible number of seconds.
    if let Ok((mut transform, mut cam)) = camera.single_mut() {
        transform.translation = map.center;
        cam.speed = (map.radius * 0.15).max(4.0);
    }

    info!(
        "spawned {} surfaces (radius {:.1} m)",
        map.meshes.len(),
        map.radius
    );
    spawned.0 = true;
}
