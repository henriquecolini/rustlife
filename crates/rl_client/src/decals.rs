//! Bullet and blood decals, projected onto the surfaces they strike.
//!
//! A decal is not a camera-facing or surface-facing quad: it is a small mesh
//! made by clipping every nearby triangle to the decal's box and projecting
//! the texture onto it. That lets a mark continue around a corner or over a
//! curved prop. Decals on a prop or brush entity are parented to it, so they
//! move with it.
//!
//! The mounted game's decal textures are authored as a colour to multiply
//! into the surface under an alpha mask. Source does that in gamma space;
//! Bevy samples colour textures in linear space and its multiply blend also
//! applies alpha, so the raw textures came out much too dark and opaque. Here
//! each decal texture is pre-multiplied against white in its authored space
//! (`1 - a * (1 - rgb)`) and sampled as unorm, leaving a plain multiply with
//! no second alpha factor. Transparent texels are white and change nothing.
//!
//! Translucent decal materials ([`DecalBlend::Blend`]) are blended over the
//! surface instead, lit by its lightmap wherever it has one: the projected
//! triangles carry the lightmap coordinates of what they were cut from.
//! `decal_material` builds either.
//!
//! The decals a map places itself live in `map_decals`.

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use rl_assets::{DecalAsset, DecalBlend, MapAsset, ModelAsset, SOURCE, coords};
use rl_sim::collision::Trace;
use rl_sim::combat::hitscan::Struck;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::map_decals::{MapDecals, place_map_decals};
use crate::net::{Game, LevelBodies};
use crate::props::{PhysicsBody, Prop};
use crate::surfaces::SurfaceClass;
use crate::world::{BrushEntity, MapHandle, MapScoped, baked_light_scale};

/// Most decals alive at once; the oldest is removed to make room.
const MAX_DECALS: usize = 256;
/// How far a decal's depth test leans towards the camera, on top of its lift.
const DECAL_DEPTH_BIAS: f32 = 4.0;
/// Side of the cells world triangles are bucketed into, metres.
const CELL_SIZE: f32 = 2.0;

/// Adds decals to the world.
pub struct DecalsPlugin;

impl Plugin for DecalsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DecalAssets>()
            .init_resource::<WorldDecals>()
            .init_resource::<MapDecals>()
            .add_message::<DecalRequest>()
            .add_systems(OnExit(AppState::InGame), reset)
            .add_systems(
                Update,
                (
                    load_assets,
                    build_world_index,
                    place_map_decals,
                    expire_decals,
                    spawn_decals,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// A decal a shot or wound asks for. Positions and directions are Source
/// space.
#[derive(Message, Debug, Clone, Copy)]
pub(crate) struct DecalRequest {
    /// What kind of mark, which chooses its textures.
    pub kind: DecalKind,
    /// What it was struck on, so the decal can be projected onto and parented
    /// to that prop or brush.
    pub target: Struck,
    /// Where the surface was struck.
    pub at: Vec3,
    /// The surface normal there.
    pub normal: Vec3,
    /// The bullet's direction, for the decal's roll.
    pub direction: Vec3,
}

/// The decal sets a request can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecalKind {
    /// A bullet mark on a surface class.
    Bullet(SurfaceClass),
    /// A blood stain.
    Blood,
}

/// A decal, removed at `until` (seconds since startup).
#[derive(Component)]
struct Decal {
    until: f32,
}

/// The loaded decal textures and materials, and the decals alive.
#[derive(Resource)]
pub(crate) struct DecalAssets {
    handles: HashMap<String, Handle<DecalAsset>>,
    materials: HashMap<(String, bool), Handle<EffectMaterial>>,
    live: Vec<Entity>,
    loaded: bool,
    rng: Rng,
}

impl Default for DecalAssets {
    fn default() -> Self {
        Self {
            handles: HashMap::new(),
            materials: HashMap::new(),
            live: Vec::new(),
            loaded: false,
            rng: Rng::seeded(&[0x0064_6563_616c_7300]),
        }
    }
}

impl DecalAssets {
    /// Starts loading decal material `name`, unless it already is, and returns
    /// its handle.
    pub(crate) fn request(&mut self, name: &str, server: &AssetServer) -> Handle<DecalAsset> {
        self.handles
            .entry(name.to_owned())
            .or_insert_with(|| server.load(format!("{SOURCE}://materials/{name}.vmt")))
            .clone()
    }

    /// The material for decal `name`, once its corrected texture has loaded,
    /// drawn on a surface that has a lightmap (`lightmapped`) or does not, and
    /// whether the decal should then be given that lightmap.
    pub(crate) fn material(
        &mut self,
        name: &str,
        lightmapped: bool,
        decals: &Assets<DecalAsset>,
        materials: &mut Assets<EffectMaterial>,
    ) -> Option<(Handle<EffectMaterial>, bool)> {
        let decal = decals.get(self.handles.get(name)?)?;
        let lit = lightmapped && decal.lit;
        let key = (name.to_owned(), lit);
        if let Some(material) = self.materials.get(&key) {
            return Some((material.clone(), lit));
        }
        let material = materials.add(effect_material(decal_material(decal, lit)));
        self.materials.insert(key, material.clone());
        Some((material, lit))
    }
}

/// The material a decal is drawn with: multiplied into the surface, or blended
/// over it and, if `lit`, lit by the lightmap in its mesh's `UV_1` exactly as
/// the world is.
#[must_use]
pub(crate) fn decal_material(decal: &DecalAsset, lit: bool) -> StandardMaterial {
    let alpha_mode = match decal.blend {
        DecalBlend::Multiply => AlphaMode::Multiply,
        DecalBlend::Blend => AlphaMode::Blend,
        DecalBlend::Mask(cutoff) => AlphaMode::Mask(cutoff),
    };
    StandardMaterial {
        base_color_texture: Some(decal.image.clone()),
        unlit: !lit,
        perceptual_roughness: 0.9,
        reflectance: 0.0,
        lightmap_exposure: baked_light_scale(),
        cull_mode: None,
        double_sided: true,
        alpha_mode,
        depth_bias: DECAL_DEPTH_BIAS,
        ..default()
    }
}

/// One triangle of the map's visible world, in Bevy space.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WorldTriangle {
    pub(crate) points: [Vec3; 3],
    pub(crate) normal: Vec3,
    /// Its corners' coordinates in the map's lightmap, if it is lightmapped.
    pub(crate) lightmap: Option<[Vec2; 3]>,
}

/// The world's triangles, bucketed by position, for finding what a decal
/// touches.
#[derive(Resource, Default)]
pub(crate) struct WorldDecals {
    pub(crate) loaded: Option<AssetId<MapAsset>>,
    triangles: Vec<WorldTriangle>,
    cells: HashMap<(i32, i32, i32), Vec<usize>>,
}

impl WorldDecals {
    /// Every triangle in the cells around `point`.
    pub(crate) fn near(&self, point: Vec3, radius: f32) -> Vec<&WorldTriangle> {
        let low = cell(point - Vec3::splat(radius));
        let high = cell(point + Vec3::splat(radius));
        let mut seen = HashSet::new();
        let mut triangles = Vec::new();
        for x in low.0..=high.0 {
            for y in low.1..=high.1 {
                for z in low.2..=high.2 {
                    let Some(indices) = self.cells.get(&(x, y, z)) else {
                        continue;
                    };
                    for &index in indices {
                        if seen.insert(index)
                            && let Some(triangle) = self.triangles.get(index)
                        {
                            triangles.push(triangle);
                        }
                    }
                }
            }
        }
        triangles
    }
}

fn cell(point: Vec3) -> (i32, i32, i32) {
    (
        (point.x / CELL_SIZE).floor() as i32,
        (point.y / CELL_SIZE).floor() as i32,
        (point.z / CELL_SIZE).floor() as i32,
    )
}

fn reset(
    mut assets: ResMut<DecalAssets>,
    mut world: ResMut<WorldDecals>,
    mut map_decals: ResMut<MapDecals>,
) {
    *assets = DecalAssets::default();
    *world = WorldDecals::default();
    *map_decals = MapDecals::default();
}

/// Starts loading every decal `effects.ron` names.
fn load_assets(game: Game, mut decals: ResMut<DecalAssets>, asset_server: Res<AssetServer>) {
    let Some(sim) = game.sim() else {
        return;
    };
    if decals.loaded {
        return;
    }
    decals.loaded = true;
    let effects = &sim.data().effects;
    for name in effects
        .bullet_impacts
        .values()
        .chain([&effects.blood])
        .flat_map(|impact| &impact.decals)
    {
        decals.request(name, &asset_server);
    }
}

/// Builds the world triangle index once the map's meshes have loaded.
fn build_world_index(
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    meshes: Res<Assets<Mesh>>,
    mut world: ResMut<WorldDecals>,
) {
    let Some((id, map)) = handle
        .as_ref()
        .and_then(|handle| Some((handle.0.id(), maps.get(&handle.0)?)))
    else {
        return;
    };
    if world.loaded == Some(id) {
        return;
    }
    let mut triangles = Vec::new();
    for map_mesh in &map.meshes {
        if let Some(mesh) = meshes.get(&map_mesh.mesh) {
            mesh_triangles(mesh, &mut triangles);
        }
    }
    let mut cells: HashMap<(i32, i32, i32), Vec<usize>> = HashMap::new();
    for (index, triangle) in triangles.iter().enumerate() {
        let low = cell(
            triangle.points[0]
                .min(triangle.points[1])
                .min(triangle.points[2]),
        );
        let high = cell(
            triangle.points[0]
                .max(triangle.points[1])
                .max(triangle.points[2]),
        );
        // A malformed or enormous triangle need not be indexed everywhere;
        // its centroid cell catches decals that actually touch it.
        if (high.0 - low.0 > 32) || (high.1 - low.1 > 32) || (high.2 - low.2 > 32) {
            cells
                .entry(cell(
                    (triangle.points[0] + triangle.points[1] + triangle.points[2]) / 3.0,
                ))
                .or_default()
                .push(index);
            continue;
        }
        for x in low.0..=high.0 {
            for y in low.1..=high.1 {
                for z in low.2..=high.2 {
                    cells.entry((x, y, z)).or_default().push(index);
                }
            }
        }
    }
    tracing::debug!(
        triangles = triangles.len(),
        cells = cells.len(),
        "indexed the world for decals"
    );
    world.loaded = Some(id);
    world.triangles = triangles;
    world.cells = cells;
}

/// Appends every triangle of `mesh` to `out`, in the mesh's own frame.
fn mesh_triangles(mesh: &Mesh, out: &mut Vec<WorldTriangle>) {
    let Some(VertexAttributeValues::Float32x3(positions)) =
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return;
    };
    let positions: Vec<Vec3> = positions.iter().map(|p| Vec3::from_array(*p)).collect();
    let lightmap: Option<Vec<Vec2>> = match mesh.attribute(Mesh::ATTRIBUTE_UV_1) {
        Some(VertexAttributeValues::Float32x2(uvs)) => {
            Some(uvs.iter().map(|uv| Vec2::from_array(*uv)).collect())
        }
        _ => None,
    };
    let indices: Vec<usize> = match mesh.indices() {
        Some(Indices::U16(indices)) => indices.iter().map(|&i| usize::from(i)).collect(),
        Some(Indices::U32(indices)) => indices.iter().map(|&i| i as usize).collect(),
        None => (0..positions.len()).collect(),
    };
    for triangle in indices.as_chunks::<3>().0 {
        let Some(a) = positions.get(triangle[0]).copied() else {
            continue;
        };
        let Some(b) = positions.get(triangle[1]).copied() else {
            continue;
        };
        let Some(c) = positions.get(triangle[2]).copied() else {
            continue;
        };
        let normal = (b - a).cross(c - a).normalize_or_zero();
        if normal != Vec3::ZERO {
            let lightmap = lightmap.as_ref().and_then(|uvs| {
                Some([
                    *uvs.get(triangle[0])?,
                    *uvs.get(triangle[1])?,
                    *uvs.get(triangle[2])?,
                ])
            });
            out.push(WorldTriangle {
                points: [a, b, c],
                normal,
                lightmap,
            });
        }
    }
}

/// The parts of the scene a decal can be projected onto.
#[derive(bevy::ecs::system::SystemParam)]
struct DecalScene<'w, 's> {
    props: Query<'w, 's, (Entity, &'static Prop, &'static GlobalTransform)>,
    physics: Query<'w, 's, (Entity, &'static PhysicsBody, &'static GlobalTransform)>,
    brushes: Query<'w, 's, (Entity, &'static BrushEntity, &'static GlobalTransform)>,
    globals: Query<'w, 's, &'static GlobalTransform>,
    level_bodies: Res<'w, LevelBodies>,
}

impl DecalScene<'_, '_> {
    /// The entity a prop body belongs to, through its `Prop.index` and the
    /// level's placement order.
    fn prop_entity(&self, body: rl_sim::physics::BodyId) -> Option<Entity> {
        if let Some((entity, ..)) = self.physics.iter().find(|(_, held, _)| held.0 == body) {
            return Some(entity);
        }
        self.props.iter().find_map(|(entity, prop, _)| {
            let index = prop.index?;
            (self.level_bodies.0.get(index).copied().flatten() == Some(body)).then_some(entity)
        })
    }
}

/// Removes old decals and forgets the entities that have gone with their
/// map or prop.
fn expire_decals(
    mut commands: Commands,
    time: Res<Time>,
    mut assets: ResMut<DecalAssets>,
    decals: Query<(Entity, &Decal)>,
) {
    let now = time.elapsed_secs();
    let mut alive = HashSet::new();
    for (entity, decal) in &decals {
        alive.insert(entity);
        if now >= decal.until {
            commands.entity(entity).try_despawn();
        }
    }
    assets.live.retain(|entity| alive.contains(entity));
}

/// Turns the tick's decal requests into projected meshes.
fn spawn_decals(
    mut commands: Commands,
    mut requests: MessageReader<DecalRequest>,
    time: Res<Time>,
    game: Game,
    mut assets: ResMut<DecalAssets>,
    world: Res<WorldDecals>,
    map_handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    mut meshes: ResMut<Assets<Mesh>>,
    models: Res<Assets<ModelAsset>>,
    decal_assets: Res<Assets<DecalAsset>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
    scene: DecalScene,
) {
    let Some(sim) = game.sim() else {
        requests.clear();
        return;
    };
    let effects = &sim.data().effects;
    let map = map_handle.and_then(|handle| maps.get(&handle.0));
    for request in requests.read() {
        let config = match request.kind {
            DecalKind::Bullet(class) => effects.bullet_impact(class.name()),
            DecalKind::Blood => Some(&effects.blood),
        };
        let Some(config) = config else {
            continue;
        };
        if config.decals.is_empty() || config.decal_life <= 0.0 {
            continue;
        }
        let name = &config.decals[assets.rng.below(config.decals.len())];
        let point = coords::position(request.at.to_array());
        let normal = coords::direction(request.normal.to_array()).normalize_or(Vec3::Y);
        let direction = coords::direction(request.direction.to_array());
        let size = config.decal_size * coords::METERS_PER_UNIT;
        let (triangles, parent) =
            target_triangles(request, point, size, &world, map, &meshes, &models, &scene);
        let parent_transform = parent.and_then(|parent| scene.globals.get(parent).ok());
        let target = DecalBox::facing(point, normal, direction, size);
        let Some(geometry) = project(&target, &triangles, parent_transform) else {
            continue;
        };
        let lightmap = map.and_then(|map| map.lightmap.clone());
        let lightmapped = lightmap.is_some() && geometry.has_lightmap();
        let Some((material, lit)) =
            assets.material(name, lightmapped, &decal_assets, &mut materials)
        else {
            continue;
        };
        let until = time.elapsed_secs() + config.decal_life;
        let mut entity = commands.spawn((
            Mesh3d(meshes.add(geometry.into_mesh(lit))),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            Decal { until },
        ));
        if let (true, Some(image)) = (lit, lightmap) {
            entity.insert(map_lightmap(image));
        }
        if let Some(parent) = parent {
            entity.insert(ChildOf(parent));
        } else {
            entity.insert(MapScoped);
        }
        let entity = entity.id();
        assets.live.push(entity);
        while assets.live.len() > MAX_DECALS {
            let old = assets.live.remove(0);
            commands.entity(old).try_despawn();
        }
    }
}

/// The triangles the request's decal may touch, in world space, and the
/// entity to parent it to (`None` for the world).
#[allow(clippy::too_many_arguments)]
fn target_triangles(
    request: &DecalRequest,
    point: Vec3,
    size: f32,
    world: &WorldDecals,
    map: Option<&MapAsset>,
    meshes: &Assets<Mesh>,
    models: &Assets<ModelAsset>,
    scene: &DecalScene,
) -> (Vec<WorldTriangle>, Option<Entity>) {
    let radius = size * 0.5 + 0.15;
    match request.target {
        Struck::Prop(body) => {
            let Some(entity) = scene.prop_entity(body) else {
                return (Vec::new(), None);
            };
            let Some((_, prop, global)) = scene.props.get(entity).ok() else {
                return (Vec::new(), None);
            };
            let Some(model) = models.get(&prop.model) else {
                return (Vec::new(), None);
            };
            if model.animation.is_some() {
                return (Vec::new(), None);
            }
            let mut triangles = Vec::new();
            for part in &model.parts {
                if let Some(mesh) = meshes.get(&part.mesh) {
                    mesh_triangles(mesh, &mut triangles);
                }
            }
            for triangle in &mut triangles {
                for vertex in &mut triangle.points {
                    *vertex = global.transform_point(*vertex);
                }
                triangle.normal = (triangle.points[1] - triangle.points[0])
                    .cross(triangle.points[2] - triangle.points[0])
                    .normalize_or_zero();
            }
            triangles.retain(|triangle| {
                triangle
                    .normal
                    .dot(coords::direction(request.normal.to_array()))
                    > 0.0
            });
            (triangles, Some(entity))
        }
        Struck::Brush(model) => {
            let Some(map) = map else {
                return (Vec::new(), None);
            };
            let Some(brush) = map
                .brush_entities
                .iter()
                .find(|brush| brush.model == model as usize)
            else {
                return (Vec::new(), None);
            };
            let Some((entity, _, global)) = scene
                .brushes
                .iter()
                .find(|(_, brush, _)| brush.model == model as usize)
            else {
                return (Vec::new(), None);
            };
            let mut triangles = Vec::new();
            for map_mesh in &brush.meshes {
                if let Some(mesh) = meshes.get(&map_mesh.mesh) {
                    mesh_triangles(mesh, &mut triangles);
                }
            }
            for triangle in &mut triangles {
                for vertex in &mut triangle.points {
                    *vertex = global.transform_point(*vertex);
                }
                triangle.normal = (triangle.points[1] - triangle.points[0])
                    .cross(triangle.points[2] - triangle.points[0])
                    .normalize_or_zero();
            }
            (triangles, Some(entity))
        }
        Struck::World | Struck::Player(_) | Struck::Nothing => {
            // Players are not decalled directly; the blood stain was traced
            // to the world or a prop before it was requested.
            let triangles = world
                .near(point, radius)
                .into_iter()
                .copied()
                .collect::<Vec<_>>();
            (triangles, None)
        }
    }
}

/// The lightmap every world surface shares (see `spawn_surface`).
pub(crate) fn map_lightmap(image: Handle<Image>) -> bevy::pbr::Lightmap {
    bevy::pbr::Lightmap {
        image,
        uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0),
        bicubic_sampling: false,
    }
}

/// The box a decal's texture is projected through, Bevy space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DecalBox {
    /// The centre of the texture.
    pub(crate) point: Vec3,
    /// The direction the surface faces, along which the box is `2 * depth`
    /// thick.
    pub(crate) normal: Vec3,
    /// The direction the texture's *u* grows in, across its width.
    pub(crate) tangent: Vec3,
    /// The direction its *v* grows in, across its height.
    pub(crate) bitangent: Vec3,
    /// Its `[width, height]`.
    pub(crate) size: Vec2,
    /// How far from `point` it reaches along the normal, either way.
    pub(crate) depth: f32,
}

impl DecalBox {
    /// A square decal of side `size`, with its *u* along `direction` as it lies
    /// in the surface: a bullet mark, turned the way the bullet came.
    fn facing(point: Vec3, normal: Vec3, direction: Vec3, size: f32) -> Self {
        let tangent = (direction - normal * direction.dot(normal))
            .try_normalize()
            .unwrap_or_else(|| normal.any_orthonormal_vector());
        Self {
            point,
            normal,
            tangent,
            bitangent: normal.cross(tangent).normalize_or_zero(),
            size: Vec2::splat(size),
            depth: (size * 0.5 * 0.35).max(0.03),
        }
    }
}

/// A decal's triangle list, in the frame of whatever it is parented to.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DecalGeometry {
    pub(crate) positions: Vec<[f32; 3]>,
    pub(crate) normals: Vec<[f32; 3]>,
    pub(crate) uvs: Vec<[f32; 2]>,
    /// One per position, or empty if any triangle had no lightmap.
    pub(crate) lightmap_uvs: Vec<[f32; 2]>,
}

impl DecalGeometry {
    /// Whether every vertex has a lightmap coordinate.
    pub(crate) fn has_lightmap(&self) -> bool {
        !self.positions.is_empty() && self.lightmap_uvs.len() == self.positions.len()
    }

    /// Appends `other`, dropping the lightmap if the two do not both have one.
    pub(crate) fn append(&mut self, mut other: Self) {
        if !(self.positions.is_empty() || self.has_lightmap() && other.has_lightmap()) {
            self.lightmap_uvs.clear();
            other.lightmap_uvs.clear();
        }
        self.positions.append(&mut other.positions);
        self.normals.append(&mut other.normals);
        self.uvs.append(&mut other.uvs);
        self.lightmap_uvs.append(&mut other.lightmap_uvs);
    }

    /// The mesh to draw; with the lightmap coordinates only if the decal is
    /// `lit`, as Bevy then expects a lightmap to go with them.
    pub(crate) fn into_mesh(self, lit: bool) -> Mesh {
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        if lit {
            mesh.with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, self.lightmap_uvs)
        } else {
            mesh
        }
    }
}

/// Clips the nearby triangles to the decal's box and turns them into geometry.
pub(crate) fn project(
    target: &DecalBox,
    triangles: &[WorldTriangle],
    parent: Option<&GlobalTransform>,
) -> Option<DecalGeometry> {
    let half = target.size * 0.5;
    if triangles.is_empty() || half.min_element() <= 0.0 {
        return None;
    }
    let DecalBox {
        point,
        normal,
        tangent,
        bitangent,
        ..
    } = *target;
    let planes = [
        (tangent, half.x),
        (-tangent, half.x),
        (bitangent, half.y),
        (-bitangent, half.y),
        (normal, target.depth),
        (-normal, target.depth),
    ];
    let to_local = parent.map(|parent| parent.affine().inverse());
    let mut geometry = DecalGeometry::default();
    let lightmapped = triangles.iter().all(|triangle| triangle.lightmap.is_some());
    for triangle in triangles {
        let mut polygon: Vec<ClipVertex> = (0..3)
            .map(|corner| ClipVertex {
                position: triangle.points[corner],
                lightmap: triangle.lightmap.map_or(Vec2::ZERO, |uvs| uvs[corner]),
            })
            .collect();
        for &(plane_normal, distance) in &planes {
            polygon = clip(polygon, plane_normal, distance, point);
            if polygon.len() < 3 {
                break;
            }
        }
        if polygon.len() < 3 {
            continue;
        }
        for index in 1..polygon.len() - 1 {
            let lift = triangle.normal * 0.0015;
            let fan = [polygon[0], polygon[index], polygon[index + 1]];
            let world = fan.map(|vertex| vertex.position + lift);
            let placed = if let Some(to_local) = to_local {
                world.map(|vertex| to_local.transform_point3(vertex))
            } else {
                world
            };
            let placed_normal = (placed[1] - placed[0])
                .cross(placed[2] - placed[0])
                .normalize_or_zero();
            if placed_normal == Vec3::ZERO {
                continue;
            }
            for ((vertex, world_vertex), corner) in placed.iter().zip(world).zip(fan) {
                geometry.positions.push(vertex.to_array());
                geometry.normals.push(placed_normal.to_array());
                let projected = world_vertex - point;
                geometry.uvs.push([
                    projected.dot(tangent) / target.size.x + 0.5,
                    projected.dot(bitangent) / target.size.y + 0.5,
                ]);
                if lightmapped {
                    geometry.lightmap_uvs.push(corner.lightmap.to_array());
                }
            }
        }
    }
    (!geometry.positions.is_empty()).then_some(geometry)
}

/// A corner of a triangle being clipped.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ClipVertex {
    position: Vec3,
    lightmap: Vec2,
}

/// Keeps the part of `polygon` inside `normal · (p - point) <= distance`.
fn clip(mut polygon: Vec<ClipVertex>, normal: Vec3, distance: f32, point: Vec3) -> Vec<ClipVertex> {
    if polygon.is_empty() {
        return polygon;
    }
    let mut out = Vec::with_capacity(polygon.len() + 1);
    let above = |vertex: ClipVertex| normal.dot(vertex.position - point) - distance;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let (da, db) = (above(a), above(b));
        if da <= 0.0 {
            out.push(a);
        }
        if (da <= 0.0) != (db <= 0.0) {
            let t = da / (da - db);
            out.push(ClipVertex {
                position: a.position + (b.position - a.position) * t,
                lightmap: a.lightmap + (b.lightmap - a.lightmap) * t,
            });
        }
    }
    polygon.clear();
    out
}

/// The hit point and surface of a blood stain, in Source space.
pub(crate) fn trace_stain(
    at: Vec3,
    direction: Vec3,
    trace: impl Fn(Vec3) -> Trace,
) -> Option<(Vec3, Vec3, Struck)> {
    let floor = trace(at - Vec3::Z * 192.0);
    let ahead = trace(at + direction * 192.0);
    let floor_distance = floor.hit().then(|| at.distance(floor.end));
    let ahead_distance = ahead.hit().then(|| at.distance(ahead.end));
    let hit = match (floor_distance, ahead_distance) {
        (Some(floor_distance), Some(ahead_distance)) => {
            if floor_distance <= ahead_distance {
                &floor
            } else {
                &ahead
            }
        }
        (Some(_), None) => &floor,
        (None, Some(_)) => &ahead,
        (None, None) => return None,
    };
    let struck = match (hit.body, hit.model) {
        (Some(body), _) => Struck::Prop(body),
        (None, Some(model)) => Struck::Brush(u32::try_from(model).unwrap_or(u32::MAX)),
        (None, None) => Struck::World,
    };
    Some((hit.end, hit.normal, struck))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4-by-2 metre floor at y = 0, centred on the origin, facing +Y, with a
    /// lightmap whose coordinates are its x and z.
    fn floor() -> Vec<WorldTriangle> {
        let corners = [
            Vec3::new(-2.0, 0.0, -1.0),
            Vec3::new(-2.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, 1.0),
            Vec3::new(2.0, 0.0, -1.0),
        ];
        let lightmap = |point: Vec3| Vec2::new(point.x, point.z);
        [[0, 1, 2], [0, 2, 3]]
            .map(|[a, b, c]| WorldTriangle {
                points: [corners[a], corners[b], corners[c]],
                normal: Vec3::Y,
                lightmap: Some([corners[a], corners[b], corners[c]].map(lightmap)),
            })
            .to_vec()
    }

    fn on_floor(size: Vec2) -> DecalBox {
        DecalBox {
            point: Vec3::ZERO,
            normal: Vec3::Y,
            tangent: Vec3::X,
            bitangent: Vec3::Z,
            size,
            depth: 0.1,
        }
    }

    #[test]
    fn a_wide_decal_keeps_its_shape_and_its_texture_spans_it() {
        let geometry =
            project(&on_floor(Vec2::new(2.0, 0.5)), &floor(), None).expect("lands on the floor");
        for (position, uv) in geometry.positions.iter().zip(&geometry.uvs) {
            assert!(position[0].abs() <= 1.0 + 1e-5, "{position:?}");
            assert!(position[2].abs() <= 0.25 + 1e-5, "{position:?}");
            assert!((uv[0] - (position[0] / 2.0 + 0.5)).abs() < 1e-5, "{uv:?}");
            assert!((uv[1] - (position[2] / 0.5 + 0.5)).abs() < 1e-5, "{uv:?}");
        }
        // The texture's corners are all reached.
        assert!(geometry.uvs.iter().any(|uv| uv[0] < 1e-5 && uv[1] < 1e-5));
        assert!(
            geometry
                .uvs
                .iter()
                .any(|uv| uv[0] > 1.0 - 1e-5 && uv[1] > 1.0 - 1e-5)
        );
    }

    #[test]
    fn a_decal_carries_the_lightmap_of_what_it_was_cut_from() {
        let geometry =
            project(&on_floor(Vec2::new(1.0, 1.0)), &floor(), None).expect("lands on the floor");
        assert!(geometry.has_lightmap());
        for (position, lightmap) in geometry.positions.iter().zip(&geometry.lightmap_uvs) {
            assert!((lightmap[0] - position[0]).abs() < 1e-5);
            assert!((lightmap[1] - position[2]).abs() < 1e-5);
        }
        assert!(
            geometry
                .clone()
                .into_mesh(true)
                .attribute(Mesh::ATTRIBUTE_UV_1)
                .is_some()
        );
        assert!(
            geometry
                .into_mesh(false)
                .attribute(Mesh::ATTRIBUTE_UV_1)
                .is_none()
        );
    }

    #[test]
    fn a_surface_without_a_lightmap_gives_none_to_the_decal() {
        let unlit: Vec<WorldTriangle> = floor()
            .into_iter()
            .map(|triangle| WorldTriangle {
                lightmap: None,
                ..triangle
            })
            .collect();
        let geometry = project(&on_floor(Vec2::ONE), &unlit, None).expect("lands");
        assert!(!geometry.has_lightmap());
    }

    #[test]
    fn nothing_is_projected_past_the_boxs_depth_or_with_no_size() {
        let far = DecalBox {
            point: Vec3::Y,
            ..on_floor(Vec2::ONE)
        };
        assert!(project(&far, &floor(), None).is_none());
        assert!(project(&on_floor(Vec2::new(1.0, 0.0)), &floor(), None).is_none());
        assert!(project(&on_floor(Vec2::ONE), &[], None).is_none());
    }

    #[test]
    fn a_bullets_decal_is_square_and_turned_by_the_bullet() {
        let target = DecalBox::facing(Vec3::ZERO, Vec3::Y, Vec3::new(1.0, -1.0, 0.0), 0.2);
        assert_eq!(target.size, Vec2::splat(0.2));
        assert!((target.tangent - Vec3::X).length() < 1e-5);
        assert!((target.bitangent.dot(target.tangent)).abs() < 1e-5);
        // Straight into the surface: any direction in it will do.
        let head_on = DecalBox::facing(Vec3::ZERO, Vec3::Y, Vec3::NEG_Y, 0.2);
        assert!(head_on.tangent.dot(Vec3::Y).abs() < 1e-5);
        assert_eq!(head_on.depth, 0.035);
    }

    #[test]
    fn merged_geometry_keeps_a_lightmap_only_if_all_of_it_has_one() {
        let lit = project(&on_floor(Vec2::ONE), &floor(), None).expect("lands");
        let mut merged = DecalGeometry::default();
        merged.append(lit.clone());
        assert!(merged.has_lightmap());
        merged.append(lit.clone());
        assert!(merged.has_lightmap());
        assert_eq!(merged.positions.len(), lit.positions.len() * 2);
        let unlit = DecalGeometry {
            lightmap_uvs: Vec::new(),
            ..lit
        };
        merged.append(unlit);
        assert!(!merged.has_lightmap());
        assert!(merged.lightmap_uvs.is_empty());
    }

    #[test]
    fn a_multiplying_decal_is_unlit_and_a_translucent_one_takes_the_lightmap() {
        let decal = |blend| DecalAsset {
            image: Handle::default(),
            blend,
            lit: true,
            size: [1.0, 1.0],
        };
        let multiply = decal_material(&decal(DecalBlend::Multiply), false);
        assert_eq!(multiply.alpha_mode, AlphaMode::Multiply);
        assert!(multiply.unlit);
        let blend = decal_material(&decal(DecalBlend::Blend), true);
        assert_eq!(blend.alpha_mode, AlphaMode::Blend);
        assert!(!blend.unlit);
        assert_eq!(blend.lightmap_exposure, baked_light_scale());
        let mask = decal_material(&decal(DecalBlend::Mask(0.5)), true);
        assert_eq!(mask.alpha_mode, AlphaMode::Mask(0.5));
    }
}
