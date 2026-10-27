//! Loading a map and placing its geometry in the scene.
//!
//! The map's pakfile is mounted, its geometry spawned as one entity per
//! material and cube of the map, and each surface's `StandardMaterial` is
//! built from the resolved
//! VMT ([`rl_assets::SourceMaterial`]) as it streams in. World surfaces are lit
//! by the map's baked lightmap; models (see [`crate::props`]) by its ambient
//! cubes, through an irradiance volume that leaves lightmapped surfaces alone.
//!
//! Each frame, only the surfaces in clusters the camera's cluster can see
//! (the map's PVS) are drawn (`cull_by_visibility`). Surfaces and props of
//! the 3D skybox are drawn by the sky camera alone (`SKY_LAYER`, see
//! [`crate::sky`]); a model's meshes take its root's render layers.
//!
//! Every root entity a map creates carries `MapScoped`, so leaving the map
//! tears all of it down in one place.

use std::collections::HashMap;

use bevy::camera::Exposure;
use bevy::camera::visibility::RenderLayers;
use bevy::light::{IrradianceVolume, Skybox};
use bevy::math::Affine2;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::pbr::Lightmap;
use bevy::prelude::*;
use rl_assets::{MapAsset, MapMesh, SOURCE, SourceMaterial};
use source_fs::{MountOrigin, Vfs, ZipMount};

use crate::camera::FlyCam;
use crate::{AppState, CurrentMap, GameVfs};

/// The render layer of the 3D skybox, which only the sky camera sees.
pub(crate) const SKY_LAYER: usize = 2;

/// The visibility clusters a world surface lies in.
#[derive(Component, Debug, Clone)]
pub(crate) struct Clusters(Vec<u16>);

/// Handle to the map currently being loaded.
#[derive(Resource)]
pub(crate) struct MapHandle(pub(crate) Handle<MapAsset>);

/// Marks a root entity that belongs to the loaded map and is despawned (with
/// its children) when the map is left.
#[derive(Component)]
pub(crate) struct MapScoped;

/// Whether the loaded map's meshes have been spawned yet (one-shot guard).
#[derive(Resource)]
struct MapSpawned(bool);

/// Per-material handles: the `SourceMaterial` being loaded for each material
/// name, and the materials built from it once ready.
#[derive(Resource, Default)]
pub(crate) struct MaterialMaps {
    source: HashMap<String, Handle<SourceMaterial>>,
    resolved: HashMap<String, Resolved>,
    /// Materials whose texture slides (`TextureScroll`): the Bevy materials
    /// built for them, and the proxy's rate and angle.
    scrolling: Vec<(Vec<Handle<StandardMaterial>>, f32, f32)>,
}

impl MaterialMaps {
    /// Starts loading material `name` (relative to `materials/`, without
    /// extension) unless it is already loading.
    pub(crate) fn request(&mut self, name: &str, asset_server: &AssetServer) {
        if !self.source.contains_key(name) {
            let handle = asset_server.load(format!("{SOURCE}://materials/{name}.vmt"));
            self.source.insert(name.to_owned(), handle);
        }
    }
}

/// The Bevy materials built for one Source material.
#[derive(Clone)]
struct Resolved {
    /// The surface's own (base) material.
    base: Handle<StandardMaterial>,
    /// The same material drawn unlit, for meshes whose vertex colours carry
    /// baked light ([`VertexLit`]).
    vertex_lit: Handle<StandardMaterial>,
    /// For `WorldVertexTransition`: the alpha-blended `$basetexture2` layer.
    blend: Option<Handle<StandardMaterial>>,
    /// For `$selfillum`: the light the surface gives off, added over a
    /// [`VertexLit`] surface (which, being unlit, has no emissive of its own).
    glow: Option<Handle<StandardMaterial>>,
}

/// The root of a brush entity (door, breakable, ...).
#[derive(Component, Debug)]
pub struct BrushEntity {
    /// Its class, e.g. `func_door`.
    pub classname: String,
    /// Its BSP model, which map logic knows it by (see `movers`).
    pub model: usize,
}

/// Marks a [`Surface`] whose vertex colours are its baked lighting: it is
/// drawn unlit, as `albedo × vertex colour`, like a lightmapped surface.
#[derive(Component)]
pub(crate) struct VertexLit;

/// Marks a surface whose blend layer has already been spawned.
#[derive(Component)]
struct BlendLayered;

/// Marks a surface whose glow layer has already been spawned.
#[derive(Component)]
struct GlowLayered;

/// The neutral material a surface shows until its real material is ready.
#[derive(Resource)]
pub(crate) struct Placeholder(pub(crate) Handle<StandardMaterial>);

/// Tags a mesh entity (world surface or model part) with the material name
/// whose `StandardMaterial` it should be drawn with once resolved.
#[derive(Component)]
pub(crate) struct Surface(pub(crate) String);

/// The spawn point asked to start at, if any (wrapped to the map's spawn
/// count): where the camera starts, and in a hosted game the player.
#[derive(Resource)]
pub(crate) struct StartSpawn(pub(crate) Option<usize>);

/// Loads the [`CurrentMap`] on entering [`AppState::InGame`], renders it, and
/// unloads it again on leaving.
pub struct WorldPlugin {
    /// Index of the spawn point to start at, if one was asked for; wraps
    /// around, so any value is valid.
    pub spawn: Option<usize>,
}

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(StartSpawn(self.spawn))
            .insert_resource(MapSpawned(false))
            .init_resource::<MaterialMaps>()
            .insert_resource(ClearColor(Color::srgb(0.10, 0.10, 0.12)))
            .add_systems(Startup, setup_placeholder)
            .add_systems(OnEnter(AppState::InGame), start_load)
            .add_systems(OnExit(AppState::InGame), unload_map)
            .add_systems(
                Update,
                (
                    (spawn_when_loaded, resolve_materials, scroll_textures).chain(),
                    cull_by_visibility,
                    inherit_render_layers,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Specular reflectance of world surfaces: none. Source's `LightmappedGeneric`
/// is diffuse-only (shine comes from an explicit `$envmap`), while Bevy's
/// default reflectance of 0.5 adds a grey specular sheen from the sun that
/// washes out floors at grazing angles. With reflectance 0, Bevy's Fresnel
/// term is zero too, so lightmapped surfaces show `albedo × lightmap` alone.
const WORLD_REFLECTANCE: f32 = 0.0;

/// Scale applied to baked light (lightmap texels, ambient cubes, sky texels)
/// so a value of `1.0` — Source's nominal full brightness — renders as the
/// unmodified texture colour.
///
/// Bevy treats baked light as scene radiance and then applies the camera's
/// exposure; dividing that exposure back out makes the final colour
/// `albedo × light`, which is what Source's shaders produce.
pub(crate) fn baked_light_scale() -> f32 {
    1.0 / Exposure::default().exposure()
}

/// Creates the placeholder material. There are deliberately no real-time
/// lights: the world is lit by its lightmaps and models by the ambient cubes,
/// both baked by `vrad`.
fn setup_placeholder(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    commands.insert_resource(Placeholder(materials.add(StandardMaterial {
        base_color: Color::srgb(0.6, 0.6, 0.62),
        perceptual_roughness: 0.9,
        reflectance: WORLD_REFLECTANCE,
        cull_mode: None,
        double_sided: true,
        lightmap_exposure: baked_light_scale(),
        ..default()
    })));
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

/// Tears down everything the map created: the [`MapScoped`] entities, the
/// camera's skybox, the material cache, the map handle (letting its assets
/// unload) and the map's pakfile mount.
fn unload_map(
    mut commands: Commands,
    scoped: Query<Entity, With<MapScoped>>,
    cameras: Query<Entity, With<FlyCam>>,
    mut material_maps: ResMut<MaterialMaps>,
    mut spawned: ResMut<MapSpawned>,
    map_name: Res<CurrentMap>,
    vfs: Res<GameVfs>,
) {
    for entity in &scoped {
        commands.entity(entity).despawn();
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

/// The camera's cluster when the surfaces were last shown or hidden.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LastCluster {
    /// Not looked at yet.
    #[default]
    Unknown,
    /// In this cluster, or outside the map.
    Known(Option<u16>),
}

/// Meshes just spawned without render layers of their own.
type Unlayered = (Added<Mesh3d>, Without<RenderLayers>);

/// Shows only the surfaces in clusters the camera's cluster may see, as
/// the camera moves between clusters (and new surfaces as they appear).
/// From outside the map — a noclip camera in the void — everything shows.
fn cull_by_visibility(
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut last: Local<LastCluster>,
    mut surfaces: Query<(Ref<Clusters>, &mut Visibility)>,
) {
    let Some(visibility) = handle
        .and_then(|handle| maps.get(&handle.0))
        .and_then(|map| map.visibility.clone())
    else {
        return;
    };
    let Ok(camera) = camera.single() else {
        return;
    };
    let eye = rl_assets::coords::to_source_position(camera.translation());
    let cluster = visibility.cluster_at(eye);
    let moved = *last != LastCluster::Known(cluster);
    *last = LastCluster::Known(cluster);
    for (clusters, mut shown) in &mut surfaces {
        if !moved && !clusters.is_added() {
            continue;
        }
        let seen =
            cluster.is_none_or(|from| clusters.0.iter().any(|&to| visibility.sees(from, to)));
        shown.set_if_neq(if seen {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// Gives each new mesh its parent's render layers, which Bevy does not
/// pass down: the meshes of a skybox prop or of the view model draw on its
/// root's layer.
fn inherit_render_layers(
    mut commands: Commands,
    meshes: Query<(Entity, &ChildOf), Unlayered>,
    parents: Query<&RenderLayers>,
) {
    for (entity, parent) in &meshes {
        if let Ok(layers) = parents.get(parent.parent()) {
            commands.entity(entity).insert(layers.clone());
        }
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
        material_maps.request(&mesh.material, &asset_server);
        let surface = spawn_surface(&mut commands, mesh, map, &placeholder);
        let mut surface = commands.entity(surface);
        surface.insert(MapScoped);
        if mesh.skybox {
            surface.insert(RenderLayers::layer(SKY_LAYER));
        } else if !mesh.clusters.is_empty() {
            surface.insert(Clusters(mesh.clusters.clone()));
        }
    }
    // Brush entities: a placed root per entity, its surfaces as children.
    for entity in &map.brush_entities {
        let root = commands
            .spawn((
                BrushEntity {
                    classname: entity.classname.clone(),
                    model: entity.model,
                },
                Transform::from_translation(entity.translation).with_rotation(entity.rotation),
                if entity.start_disabled {
                    Visibility::Hidden
                } else {
                    Visibility::default()
                },
                MapScoped,
            ))
            .id();
        for mesh in &entity.meshes {
            material_maps.request(&mesh.material, &asset_server);
            let surface = spawn_surface(&mut commands, mesh, map, &placeholder);
            commands.entity(root).add_child(surface);
        }
    }

    // Models are lit from the ambient cubes; lightmapped surfaces already
    // include that light and must not receive it twice.
    if let Some(volume) = &map.irradiance {
        commands.spawn((
            IrradianceVolume {
                voxels: volume.voxels.clone(),
                intensity: baked_light_scale(),
                affects_lightmapped_meshes: false,
            },
            Transform::from_translation(volume.placement.center).with_scale(volume.placement.size),
            MapScoped,
        ));
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
                brightness: baked_light_scale(),
                rotation: Quat::IDENTITY,
            });
        }
        // Prefer a player spawn at eye height (64 units) facing the spawn's
        // direction, so the view starts inside the playable space; fall back
        // to the geometry centre for maps without spawns.
        let spawn =
            (!map.spawns.is_empty()).then(|| map.spawns[start.0.unwrap_or(0) % map.spawns.len()]);
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
        "spawned {} surfaces and {} brush entities (radius {:.1} m)",
        map.meshes.len(),
        map.brush_entities.len(),
        map.radius
    );
    spawned.0 = true;
}

/// Spawns one lightmapped surface with the placeholder material.
fn spawn_surface(
    commands: &mut Commands,
    mesh: &MapMesh,
    map: &MapAsset,
    placeholder: &Placeholder,
) -> Entity {
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
    surface.id()
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
    Has<VertexLit>,
    Has<GlowLayered>,
    Option<&'static ChildOf>,
    Option<&'static SkinnedMesh>,
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
    for (
        entity,
        surface,
        mesh,
        lightmap,
        mut material,
        layered,
        vertex_lit,
        glowing,
        parent,
        skin,
    ) in &mut surfaces
    {
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
            let base = standard_from(source);
            let resolved = Resolved {
                vertex_lit: standard_materials.add(StandardMaterial {
                    unlit: true,
                    ..base.clone()
                }),
                base: standard_materials.add(base),
                blend: source
                    .base_color2
                    .clone()
                    .map(|texture| standard_materials.add(blend_layer(texture))),
                glow: source
                    .emissive
                    .clone()
                    .map(|texture| standard_materials.add(glow_layer(texture))),
            };
            if let Some((rate, angle)) = source.scroll {
                material_maps.scrolling.push((
                    vec![resolved.base.clone(), resolved.vertex_lit.clone()],
                    rate,
                    angle,
                ));
            }
            material_maps
                .resolved
                .insert(name.clone(), resolved.clone());
            resolved
        };

        let wanted = if vertex_lit {
            &resolved.vertex_lit
        } else {
            &resolved.base
        };
        if material.0 != *wanted {
            material.0 = wanted.clone();
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
        if let (Some(glow), true, false) = (resolved.glow, vertex_lit, glowing) {
            // The same mesh, skin included, drawn again with the surface's own
            // light added. Beside the surface rather than under it, where it
            // is a part of its model like the surface.
            let mut layer = commands.spawn((Mesh3d(mesh.0.clone()), MeshMaterial3d(glow)));
            if let Some(skin) = skin {
                layer.insert(skin.clone());
            }
            let layer = layer.id();
            commands.entity(entity).insert(GlowLayered);
            commands
                .entity(parent.map_or(entity, ChildOf::parent))
                .add_child(layer);
        }
    }
}

/// Slides the textures of materials with a `TextureScroll` proxy.
fn scroll_textures(
    time: Res<Time>,
    maps: Res<MaterialMaps>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (handles, rate, angle) in &maps.scrolling {
        let heading = angle.to_radians();
        let offset = Vec2::new(heading.cos(), heading.sin()) * (rate * time.elapsed_secs());
        for handle in handles {
            if let Some(mut material) = materials.get_mut(handle) {
                material.uv_transform = Affine2::from_translation(offset.fract());
            }
        }
    }
}

/// How opaque a water surface is drawn: what lies below shows through
/// faintly, tinted by the water's fog.
const WATER_OPACITY: f32 = 0.75;

/// How bright a `$selfillum` surface's light is, over the 1.0 of full
/// white: Source's HDR lets it exceed what the lit surface can, which is
/// what the bloom of a view model feeds on.
const SELF_ILLUM_GAIN: f32 = 2.0;

/// The light a `$selfillum` surface gives off: its masked colour, added to
/// what is behind it and lit by nothing.
fn glow_layer(texture: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::linear_rgb(SELF_ILLUM_GAIN, SELF_ILLUM_GAIN, SELF_ILLUM_GAIN),
        base_color_texture: Some(texture),
        unlit: true,
        cull_mode: None,
        double_sided: true,
        alpha_mode: AlphaMode::Add,
        // Coplanar with the surface it lights.
        depth_bias: 1.0,
        ..default()
    }
}

/// A water surface: its fog colour, mostly opaque, unlit (water has no
/// lightmap; its colour is the light it carries), and its base texture —
/// the "sludge" some waters float — over that if it has one. Source's
/// refraction and reflection are left out.
fn water_from(source: &SourceMaterial, fog: rl_assets::material::WaterFog) -> StandardMaterial {
    let [red, green, blue] = fog.color;
    StandardMaterial {
        base_color: Color::linear_rgba(red, green, blue, WATER_OPACITY),
        base_color_texture: source.base_color.clone(),
        unlit: true,
        cull_mode: None,
        double_sided: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    }
}

/// The second layer of a `WorldVertexTransition` surface: `$basetexture2`,
/// alpha-blended so the mesh's vertex alpha (the mapper's painted blend)
/// decides how much of it shows over the base layer underneath.
fn blend_layer(texture: Handle<Image>) -> StandardMaterial {
    StandardMaterial {
        base_color_texture: Some(texture),
        perceptual_roughness: 0.9,
        reflectance: WORLD_REFLECTANCE,
        cull_mode: None,
        double_sided: true,
        alpha_mode: AlphaMode::Blend,
        // Coplanar with the base layer: nudge it towards the camera so the
        // depth test never rejects it.
        depth_bias: 1.0,
        lightmap_exposure: baked_light_scale(),
        ..default()
    }
}

/// Builds a `StandardMaterial` from a resolved [`SourceMaterial`].
fn standard_from(source: &SourceMaterial) -> StandardMaterial {
    if let Some(fog) = source.water_fog {
        return water_from(source, fog);
    }
    if source.additive {
        // Energy effects: their colour added to what is behind, lit by
        // nothing.
        return StandardMaterial {
            base_color_texture: source.base_color.clone(),
            unlit: true,
            cull_mode: None,
            double_sided: true,
            alpha_mode: AlphaMode::Add,
            ..default()
        };
    }
    let alpha_mode = if source.translucent {
        AlphaMode::Blend
    } else if source.alpha_test.is_some() {
        // Alpha-tested texels (grates, foliage, perforated metal) cover the
        // multisampled pixel by their alpha rather than being cut at a
        // threshold, so fine holes stay smooth instead of shimmering as
        // their mips blur them.
        AlphaMode::AlphaToCoverage
    } else {
        AlphaMode::Opaque
    };
    StandardMaterial {
        base_color_texture: source.base_color.clone(),
        normal_map_texture: source.normal.clone(),
        perceptual_roughness: 0.9,
        reflectance: WORLD_REFLECTANCE,
        // Source polygons are not wound for Bevy's front-face convention, so
        // draw both sides regardless of `$nocull` until winding is corrected.
        cull_mode: None,
        double_sided: true,
        alpha_mode,
        lightmap_exposure: baked_light_scale(),
        // `$selfillum`: where the base texture's alpha says so, it shines with
        // its own colour, as bright as a lit surface at full light.
        emissive: if source.emissive.is_some() {
            LinearRgba::gray(baked_light_scale())
        } else {
            LinearRgba::BLACK
        },
        emissive_texture: source.emissive.clone(),
        ..default()
    }
}
