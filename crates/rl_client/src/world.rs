//! Loading a map and placing its geometry in the scene.
//!
//! M1 renders the world with a single neutral material and basic lighting, to
//! prove the full content path (install → VFS → BSP → mesh → screen). Per-face
//! materials, lightmaps and the sky arrive in later M1 increments.

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

/// The neutral placeholder material used for every surface in M1.
#[derive(Resource)]
struct PlaceholderMaterial(Handle<StandardMaterial>);

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

fn setup_lighting(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(1.0, 3.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    let material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.65, 0.65, 0.68),
        perceptual_roughness: 0.9,
        // Source polygons are not consistently wound for Bevy's front-face
        // convention, so draw both sides until real materials land.
        cull_mode: None,
        ..default()
    });
    commands.insert_resource(PlaceholderMaterial(material));
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
    placeholder: Res<PlaceholderMaterial>,
    mut camera: Query<(&mut Transform, &mut FlyCam)>,
) {
    if spawned.0 {
        return;
    }
    let Some(map) = maps.get(&handle.0) else {
        return;
    };

    for mesh in &map.meshes {
        commands.spawn((
            Mesh3d(mesh.mesh.clone()),
            MeshMaterial3d(placeholder.0.clone()),
        ));
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
