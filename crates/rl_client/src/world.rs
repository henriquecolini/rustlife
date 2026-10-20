//! Loading a map and placing its geometry in the scene.
//!
//! The map's pakfile is mounted, its geometry spawned as one entity per
//! material, and each surface's `StandardMaterial` is built from the resolved
//! VMT ([`rl_assets::SourceMaterial`]) as it streams in. World surfaces are lit
//! by the map's baked lightmap; dynamic lights only touch unlit content.

use std::collections::HashMap;

use bevy::camera::Exposure;
use bevy::light::Skybox;
use bevy::pbr::Lightmap;
use bevy::prelude::*;
use rl_assets::{MapAsset, SOURCE, SourceMaterial};
use source_fs::{MountOrigin, Vfs, ZipMount};

use crate::camera::FlyCam;
use crate::{AppState, CurrentMap, GameVfs};

/// Handle to the map currently being loaded.
#[derive(Resource)]
struct MapHandle(Handle<MapAsset>);

/// Whether the loaded map's meshes have been spawned yet (one-shot guard).
#[derive(Resource)]
struct MapSpawned(bool);

/// Per-material handles: the `SourceMaterial` being loaded for each material
/// name, and the materials built from it once ready.
#[derive(Resource, Default)]
struct MaterialMaps {
    source: HashMap<String, Handle<SourceMaterial>>,
    resolved: HashMap<String, Resolved>,
}

/// The Bevy materials built for one Source material.
#[derive(Clone)]
struct Resolved {
    /// The surface's own (base) material.
    base: Handle<StandardMaterial>,
    /// For `WorldVertexTransition`: the alpha-blended `$basetexture2` layer.
    blend: Option<Handle<StandardMaterial>>,
}

/// Marks a surface whose blend layer has already been spawned.
#[derive(Component)]
struct BlendLayered;

/// The neutral material a surface shows until its real material is ready.
#[derive(Resource)]
struct Placeholder(Handle<StandardMaterial>);

/// Tags a mesh entity with the material name it is waiting to resolve.
#[derive(Component)]
struct Surface(String);

/// Which spawn point the camera starts at (wrapped to the map's spawn count).
#[derive(Resource)]
struct StartSpawn(usize);

/// Loads the [`CurrentMap`] on entering [`AppState::InGame`], renders it, and
/// unloads it again on leaving (`M` returns to the map browser).
pub struct WorldPlugin {
    /// Index of the spawn point to start the camera at; wraps around, so any
    /// value is valid.
    pub spawn: usize,
}

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(StartSpawn(self.spawn))
            .insert_resource(MapSpawned(false))
            .init_resource::<MaterialMaps>()
            .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
            .add_systems(Startup, setup_lighting)
            .add_systems(OnEnter(AppState::InGame), start_load)
            .add_systems(OnExit(AppState::InGame), unload_map)
            .add_systems(
                Update,
                ((spawn_when_loaded, resolve_materials).chain(), open_browser)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Scale applied to lightmap texels so a decoded luxel of `1.0` — Source's
/// nominal full brightness — renders as the unmodified texture colour.
///
/// Bevy treats lightmap values as scene radiance and then applies the camera's
/// exposure; dividing that exposure back out makes the final colour
/// `albedo × luxel`, which is what Source's `LightmappedGeneric` produces.
fn lightmap_exposure() -> f32 {
    1.0 / Exposure::default().exposure()
}

fn setup_lighting(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(Placeholder(materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.6, 0.62),
        perceptual_roughness: 0.9,
        cull_mode: None,
        double_sided: true,
        lightmap_exposure: lightmap_exposure(),
        ..default()
    })));

    commands.spawn((
        DirectionalLight {
            illuminance: 8_000.0,
            shadow_maps_enabled: false,
            // Baked lighting already includes the sun; adding it again would
            // double-light the world.
            affects_lightmapped_mesh_diffuse: false,
            ..default()
        },
        Transform::from_xyz(1.0, 3.0, 2.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

fn start_load(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    map_name: Res<CurrentMap>,
    vfs: Res<GameVfs>,
    mut spawned: ResMut<MapSpawned>,
) {
    spawned.0 = false;
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

/// Tears down everything [`start_load`] and [`spawn_when_loaded`] created: the
/// surface entities, the camera's skybox, the material cache, the map handle
/// (letting its assets unload) and the map's pakfile mount.
fn unload_map(
    mut commands: Commands,
    surfaces: Query<Entity, With<Surface>>,
    cameras: Query<Entity, With<FlyCam>>,
    mut material_maps: ResMut<MaterialMaps>,
    mut spawned: ResMut<MapSpawned>,
    map_name: Res<CurrentMap>,
    vfs: Res<GameVfs>,
) {
    for surface in &surfaces {
        commands.entity(surface).despawn();
    }
    for camera in &cameras {
        commands.entity(camera).remove::<Skybox>();
    }
    *material_maps = MaterialMaps::default();
    spawned.0 = false;
    commands.remove_resource::<MapHandle>();
    let unmounted = vfs.0.unmount_origin(&MountOrigin::MapPakfile {
        map: map_name.0.clone(),
    });
    info!(
        "unloaded {} ({unmounted} pakfile mount removed)",
        map_name.0
    );
}

/// `M` leaves the map for the map browser.
fn open_browser(keys: Res<ButtonInput<KeyCode>>, mut next: ResMut<NextState<AppState>>) {
    if keys.just_pressed(KeyCode::KeyM) {
        next.set(AppState::Browser);
    }
}

fn spawn_when_loaded(
    mut commands: Commands,
    mut spawned: ResMut<MapSpawned>,
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    asset_server: Res<AssetServer>,
    placeholder: Res<Placeholder>,
    mut material_maps: ResMut<MaterialMaps>,
    start: Res<StartSpawn>,
    mut camera: Query<(Entity, &mut Transform, &mut FlyCam)>,
) {
    if spawned.0 {
        return;
    }
    let Some(map) = handle.and_then(|handle| maps.get(&handle.0)) else {
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
        let mut surface = commands.spawn((
            Mesh3d(mesh.mesh.clone()),
            MeshMaterial3d(placeholder.0.clone()),
            Surface(mesh.material.clone()),
        ));
        // Every surface shares the map's single atlas; its UV_1 coordinates
        // already address its own blocks, so the rect is the whole image.
        if let Some(lightmap) = &map.lightmap {
            surface.insert(Lightmap {
                image: lightmap.clone(),
                uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
                bicubic_sampling: false,
            });
        }
    }

    // Drop the camera into the middle of the level and scale its speed to the
    // map so flying across it takes a sensible number of seconds.
    if let Ok((entity, mut transform, mut cam)) = camera.single_mut() {
        cam.speed = (map.radius * 0.15).max(4.0);
        if let Some(sky) = &map.skybox {
            commands.entity(entity).insert(Skybox {
                image: Some(sky.clone()),
                // Like the lightmap, undo the camera exposure so sky texels
                // render at their authored brightness.
                brightness: lightmap_exposure(),
                rotation: Quat::IDENTITY,
            });
        }
        // Prefer a player spawn at eye height (64 units) facing the spawn's
        // direction, so the view starts inside the playable space; fall back
        // to the geometry centre for maps without spawns.
        let spawn = (!map.spawns.is_empty()).then(|| map.spawns[start.0 % map.spawns.len()]);
        if let Some(spawn) = spawn {
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

/// What [`resolve_materials`] needs from each map surface: its mesh and
/// lightmap (shared with a blend layer), its current material, and whether its
/// blend layer already exists.
type SurfaceItems = (
    Entity,
    &'static Surface,
    &'static Mesh3d,
    Option<&'static Lightmap>,
    &'static mut MeshMaterial3d<StandardMaterial>,
    Has<BlendLayered>,
);

/// Swaps each surface's placeholder material for the real one as soon as its
/// `SourceMaterial` finishes loading, building the Bevy materials once per
/// material name and caching them. Blend materials also get their second
/// layer: a child entity redrawing the same mesh with `$basetexture2`, faded in
/// by the mesh's vertex alpha.
fn resolve_materials(
    mut commands: Commands,
    mut surfaces: Query<SurfaceItems>,
    mut material_maps: ResMut<MaterialMaps>,
    source_materials: Res<Assets<SourceMaterial>>,
    mut standard_materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, surface, mesh, lightmap, mut material, layered) in &mut surfaces {
        let name = &surface.0;

        let resolved = if let Some(resolved) = material_maps.resolved.get(name) {
            resolved.clone()
        } else {
            let Some(source_handle) = material_maps.source.get(name).cloned() else {
                continue;
            };
            let Some(source) = source_materials.get(&source_handle) else {
                continue;
            };
            let resolved = Resolved {
                base: standard_materials.add(standard_from(source)),
                blend: source
                    .base_color2
                    .clone()
                    .map(|texture| standard_materials.add(blend_layer(texture))),
            };
            material_maps
                .resolved
                .insert(name.clone(), resolved.clone());
            resolved
        };

        if material.0 != resolved.base {
            material.0 = resolved.base.clone();
        }
        if let (Some(blend), false) = (resolved.blend, layered) {
            let mut layer = commands.spawn((Mesh3d(mesh.0.clone()), MeshMaterial3d(blend)));
            if let Some(lightmap) = lightmap {
                layer.insert(lightmap.clone());
            }
            let layer = layer.id();
            // As a child, the layer is despawned with its surface on unload.
            commands
                .entity(entity)
                .insert(BlendLayered)
                .add_child(layer);
        }
    }
}

/// The second layer of a `WorldVertexTransition` surface: `$basetexture2`,
/// alpha-blended so the mesh's vertex alpha (the mapper's painted blend)
/// decides how much of it shows over the base layer underneath.
fn blend_layer(texture: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color_texture: Some(texture),
        perceptual_roughness: 0.9,
        cull_mode: None,
        double_sided: true,
        alpha_mode: AlphaMode::Blend,
        // Coplanar with the base layer: nudge it towards the camera so the
        // depth test never rejects it.
        depth_bias: 1.0,
        lightmap_exposure: lightmap_exposure(),
        ..default()
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
        lightmap_exposure: lightmap_exposure(),
        ..default()
    }
}
