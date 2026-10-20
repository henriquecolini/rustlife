//! Loading a map and placing its geometry in the scene.
//!
//! The map's pakfile is mounted, its geometry spawned as one entity per
//! material, and each surface's `StandardMaterial` is built from the resolved
//! VMT ([`rl_assets::SourceMaterial`]) as it streams in. Lightmaps and the sky
//! arrive in later M1 increments.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::{MapAsset, SOURCE, SourceMaterial};
use source_fs::{MountOrigin, Vfs, ZipMount};

use crate::GameVfs;
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

/// Per-material handles: the `SourceMaterial` being loaded for each material
/// name, and the `StandardMaterial` built from it once ready.
#[derive(Resource, Default)]
struct MaterialMaps {
    source: HashMap<String, Handle<SourceMaterial>>,
    standard: HashMap<String, Handle<StandardMaterial>>,
}

/// The neutral material a surface shows until its real material is ready.
#[derive(Resource)]
struct Placeholder(Handle<StandardMaterial>);

/// Tags a mesh entity with the material name it is waiting to resolve.
#[derive(Component)]
struct Surface(String);

/// Loads a map and renders its geometry.
pub struct WorldPlugin {
    /// Map name without path or extension.
    pub map_name: String,
}

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MapName(self.map_name.clone()))
            .insert_resource(MapSpawned(false))
            .init_resource::<MaterialMaps>()
            .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
            .add_systems(Startup, (setup_lighting, start_load))
            .add_systems(Update, (spawn_when_loaded, resolve_materials).chain());
    }
}

fn setup_lighting(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(Placeholder(materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.6, 0.62),
        perceptual_roughness: 0.9,
        cull_mode: None,
        double_sided: true,
        ..default()
    })));

    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(1.0, 3.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

fn start_load(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    map_name: Res<MapName>,
    vfs: Res<GameVfs>,
) {
    // Mount the map's embedded pakfile first so its own materials and textures
    // (patched cubemap materials, custom content) are found before the map's
    // assets load.
    mount_pakfile(&vfs.0, &map_name.0);

    let path = format!("{SOURCE}://maps/{}.bsp", map_name.0);
    info!("loading map {path}");
    commands.insert_resource(MapHandle(asset_server.load(path)));
}

/// Reads the map's BSP from the VFS, extracts its pakfile and mounts it at the
/// top of the search path. Failures are logged and skipped — a map without a
/// mountable pakfile still loads, just without its map-specific content.
fn mount_pakfile(vfs: &Vfs, map: &str) {
    let bsp_path = format!("maps/{map}.bsp");
    let bytes = match vfs.read(&bsp_path) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            warn!("map {bsp_path} not found; cannot mount its pakfile");
            return;
        }
        Err(error) => {
            warn!("reading {bsp_path} for pakfile mount: {error}");
            return;
        }
    };
    let Some(pak) = source_formats::bsp::pakfile_bytes(&bytes) else {
        return;
    };
    match ZipMount::from_bytes(format!("pakfile of {map}"), pak) {
        Ok(mount) => {
            vfs.mount_front(
                mount,
                MountOrigin::MapPakfile {
                    map: map.to_owned(),
                },
            );
            info!("mounted pakfile for {map}");
        }
        Err(error) => warn!("pakfile of {map} is not a readable zip: {error}"),
    }
}

fn spawn_when_loaded(
    mut commands: Commands,
    mut spawned: ResMut<MapSpawned>,
    handle: Res<MapHandle>,
    maps: Res<Assets<MapAsset>>,
    asset_server: Res<AssetServer>,
    placeholder: Res<Placeholder>,
    mut material_maps: ResMut<MaterialMaps>,
    mut camera: Query<(&mut Transform, &mut FlyCam)>,
) {
    if spawned.0 {
        return;
    }
    let Some(map) = maps.get(&handle.0) else {
        return;
    };

    // Spawn each surface with the placeholder material and start loading its
    // VMT. `resolve_materials` swaps in the real material once it is ready.
    for mesh in &map.meshes {
        material_maps
            .source
            .entry(mesh.material.clone())
            .or_insert_with(|| {
                asset_server.load(format!("{SOURCE}://materials/{}.vmt", mesh.material))
            });
        commands.spawn((
            Mesh3d(mesh.mesh.clone()),
            MeshMaterial3d(placeholder.0.clone()),
            Surface(mesh.material.clone()),
        ));
    }

    // Drop the camera into the middle of the level and scale its speed to the
    // map so flying across it takes a sensible number of seconds.
    if let Ok((mut transform, mut cam)) = camera.single_mut() {
        cam.speed = (map.radius * 0.15).max(4.0);
        // Prefer a player spawn at eye height (64 units) facing the spawn's
        // direction, so the view starts inside the playable space; fall back
        // to the geometry centre for maps without spawns.
        if let Some(spawn) = map.spawns.first() {
            transform.translation =
                spawn.position + Vec3::Y * (64.0 * rl_assets::coords::METERS_PER_UNIT);
            cam.yaw = spawn.yaw;
            cam.pitch = 0.0;
            transform.rotation = Quat::from_rotation_y(spawn.yaw);
        } else {
            transform.translation = map.center;
        }
    }

    info!(
        "spawned {} surfaces (radius {:.1} m)",
        map.meshes.len(),
        map.radius
    );
    spawned.0 = true;
}

/// Swaps each surface's placeholder material for the real one as soon as its
/// `SourceMaterial` finishes loading, building the `StandardMaterial` once per
/// material name and caching it.
fn resolve_materials(
    mut surfaces: Query<(&Surface, &mut MeshMaterial3d<StandardMaterial>)>,
    mut material_maps: ResMut<MaterialMaps>,
    source_materials: Res<Assets<SourceMaterial>>,
    mut standard_materials: ResMut<Assets<StandardMaterial>>,
) {
    for (surface, mut material) in &mut surfaces {
        let name = &surface.0;

        if let Some(ready) = material_maps.standard.get(name) {
            if material.0 != *ready {
                material.0 = ready.clone();
            }
            continue;
        }

        let Some(source_handle) = material_maps.source.get(name).cloned() else {
            continue;
        };
        let Some(source) = source_materials.get(&source_handle) else {
            continue;
        };

        let standard = standard_materials.add(standard_from(source));
        material.0 = standard.clone();
        material_maps.standard.insert(name.clone(), standard);
    }
}

/// Builds a `StandardMaterial` from a resolved [`SourceMaterial`].
fn standard_from(source: &SourceMaterial) -> StandardMaterial {
    let alpha_mode = if source.translucent {
        AlphaMode::Blend
    } else if let Some(cutoff) = source.alpha_test {
        AlphaMode::Mask(cutoff)
    } else {
        AlphaMode::Opaque
    };
    StandardMaterial {
        base_color_texture: source.base_color.clone(),
        normal_map_texture: source.normal.clone(),
        perceptual_roughness: 0.9,
        // Source polygons are not wound for Bevy's front-face convention, so
        // draw both sides regardless of `$nocull` until winding is corrected.
        cull_mode: None,
        double_sided: true,
        alpha_mode,
        ..default()
    }
}
