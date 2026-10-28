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

use std::collections::{HashMap, HashSet};

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use rl_assets::{DecalAsset, MapAsset, ModelAsset, SOURCE, coords};
use rl_sim::collision::Trace;
use rl_sim::combat::hitscan::Struck;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::net::{Game, LevelBodies};
use crate::props::{PhysicsBody, Prop};
use crate::surfaces::SurfaceClass;
use crate::world::{BrushEntity, MapHandle, MapScoped};

/// Most decals alive at once; the oldest is removed to make room.
const MAX_DECALS: usize = 256;
/// Side of the cells world triangles are bucketed into, metres.
const CELL_SIZE: f32 = 2.0;

/// Adds decals to the world.
pub struct DecalsPlugin;

impl Plugin for DecalsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DecalAssets>()
            .init_resource::<WorldDecals>()
            .add_message::<DecalRequest>()
            .add_systems(OnExit(AppState::InGame), reset)
            .add_systems(
                Update,
                (load_assets, build_world_index, expire_decals, spawn_decals)
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
struct DecalAssets {
    handles: HashMap<String, Handle<DecalAsset>>,
    materials: HashMap<String, Handle<EffectMaterial>>,
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
    /// The material for decal `name`, once its corrected texture has loaded.
    fn material(
        &mut self,
        name: &str,
        decals: &Assets<DecalAsset>,
        materials: &mut Assets<EffectMaterial>,
    ) -> Option<Handle<EffectMaterial>> {
        if let Some(material) = self.materials.get(name) {
            return Some(material.clone());
        }
        let decal = decals.get(self.handles.get(name)?)?;
        let material = materials.add(effect_material(StandardMaterial {
            base_color_texture: Some(decal.image.clone()),
            unlit: true,
            cull_mode: None,
            double_sided: true,
            alpha_mode: AlphaMode::Multiply,
            depth_bias: 4.0,
            ..default()
        }));
        self.materials.insert(name.to_owned(), material.clone());
        Some(material)
    }
}

/// One triangle of the map's visible world, in Bevy space.
#[derive(Debug, Clone, Copy)]
struct WorldTriangle {
    points: [Vec3; 3],
    normal: Vec3,
}

/// The world's triangles, bucketed by position, for finding what a decal
/// touches.
#[derive(Resource, Default)]
struct WorldDecals {
    loaded: Option<AssetId<MapAsset>>,
    triangles: Vec<WorldTriangle>,
    cells: HashMap<(i32, i32, i32), Vec<usize>>,
}

impl WorldDecals {
    /// Every triangle in the cells around `point`.
    fn near(&self, point: Vec3, radius: f32) -> Vec<&WorldTriangle> {
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

fn reset(mut assets: ResMut<DecalAssets>, mut world: ResMut<WorldDecals>) {
    *assets = DecalAssets::default();
    *world = WorldDecals::default();
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
        let handle = asset_server.load(format!("{SOURCE}://materials/{name}.vmt"));
        decals.handles.entry(name.clone()).or_insert(handle);
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
            out.push(WorldTriangle {
                points: [a, b, c],
                normal,
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
        let Some(material) = assets.material(name, &decal_assets, &mut materials) else {
            continue;
        };
        let point = coords::position(request.at.to_array());
        let normal = coords::direction(request.normal.to_array()).normalize_or(Vec3::Y);
        let direction = coords::direction(request.direction.to_array());
        let size = config.decal_size * coords::METERS_PER_UNIT;
        let (triangles, parent) =
            target_triangles(request, point, size, &world, map, &meshes, &models, &scene);
        let parent_transform = parent.and_then(|parent| scene.globals.get(parent).ok());
        let Some(mesh) = project(point, normal, direction, size, &triangles, parent_transform)
        else {
            continue;
        };
        let until = time.elapsed_secs() + config.decal_life;
        let mut entity = commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            Decal { until },
        ));
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

/// Clips the nearby triangles to the decal's box and turns them into a mesh.
#[allow(clippy::too_many_arguments)]
fn project(
    point: Vec3,
    normal: Vec3,
    direction: Vec3,
    size: f32,
    triangles: &[WorldTriangle],
    parent: Option<&GlobalTransform>,
) -> Option<Mesh> {
    if triangles.is_empty() || size <= 0.0 {
        return None;
    }
    let tangent = (direction - normal * direction.dot(normal))
        .try_normalize()
        .unwrap_or_else(|| normal.any_orthonormal_vector());
    let bitangent = normal.cross(tangent).normalize_or_zero();
    let half = size * 0.5;
    let depth = (half * 0.35).max(0.03);
    let planes = [
        (tangent, half),
        (-tangent, half),
        (bitangent, half),
        (-bitangent, half),
        (normal, depth),
        (-normal, depth),
    ];
    let to_local = parent.map(|parent| parent.affine().inverse());
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    for triangle in triangles {
        let mut polygon = triangle.points.to_vec();
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
            let world = [
                polygon[0] + lift,
                polygon[index] + lift,
                polygon[index + 1] + lift,
            ];
            let local = if let Some(to_local) = to_local {
                world.map(|vertex| to_local.transform_point3(vertex))
            } else {
                world
            };
            let local_normal = (local[1] - local[0])
                .cross(local[2] - local[0])
                .normalize_or_zero();
            if local_normal == Vec3::ZERO {
                continue;
            }
            for (vertex, world_vertex) in local.iter().zip(world) {
                positions.push(vertex.to_array());
                normals.push(local_normal.to_array());
                let projected = world_vertex - point;
                uvs.push([
                    projected.dot(tangent) / size + 0.5,
                    projected.dot(bitangent) / size + 0.5,
                ]);
            }
        }
    }
    if positions.is_empty() {
        return None;
    }
    Some(
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs),
    )
}

/// Keeps the part of `polygon` inside `normal · (p - point) <= distance`.
fn clip(mut polygon: Vec<Vec3>, normal: Vec3, distance: f32, point: Vec3) -> Vec<Vec3> {
    if polygon.is_empty() {
        return polygon;
    }
    let mut out = Vec::with_capacity(polygon.len() + 1);
    let required = |p: Vec3| normal.dot(p - point) <= distance;
    for index in 0..polygon.len() {
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        let (inside_a, inside_b) = (required(a), required(b));
        if inside_a {
            out.push(a);
        }
        if inside_a != inside_b {
            let da = normal.dot(a - point) - distance;
            let db = normal.dot(b - point) - distance;
            let t = da / (da - db);
            out.push(a + (b - a) * t);
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
