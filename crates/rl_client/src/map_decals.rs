//! The decals a map places itself: `infodecal` entities and overlays.
//!
//! Graffiti, stains, posters and grime are in the map, not made by play. They
//! stay for as long as it is loaded and do not count against the cap on the
//! decals shots leave.
//!
//! An `infodecal` is a point near a surface and a material name: as
//! `CDecal::StaticDecal` and `R_DecalShoot` do, it goes on the surface closest
//! to that point, projected from there at the material's own size, with its
//! axes chosen from the surface's normal alone (`R_DecalComputeBasis`). An
//! overlay has been clipped to its faces already (`source_formats::overlays`).
//!
//! Drawn one by one these would be hundreds of draw calls, so they are merged
//! by material and part of the map. Those whose material is lit use the map's
//! lightmap, at the coordinates of the surface under them.

use std::collections::{HashMap, HashSet};

use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use rl_assets::{DecalAsset, MapAsset, coords};
use source_formats::entities::MapEntity;

use crate::decals::{
    DecalAssets, DecalBox, DecalGeometry, WorldDecals, WorldTriangle, map_lightmap, project,
};
use crate::effect_material::EffectMaterial;
use crate::world::{Clusters, MapHandle, MapScoped, SKY_LAYER};

/// How far from an `infodecal` its surface may be, units. The engine traces 5
/// units along each axis, up to 8.66 units from the point.
const REACH_UNITS: f32 = 8.0;
/// How far off the surface's plane the decal's box reaches, units: enough to
/// follow a bumpy displacement.
const DEPTH_UNITS: f32 = 8.0;
/// `infodecal` spawn flag: not in deathmatch games, which HL2DM always is.
const SF_DECAL_NOT_IN_DEATHMATCH: u32 = 2048;
/// Side of the cubes decals are merged by, units, as the world's surfaces are.
const CHUNK_UNITS: f32 = 1024.0;
/// Below this share of a decal's normal, a surface is too far turned from the
/// one the decal is on to take part of it.
const MIN_FACING: f32 = 0.1;

/// Whether the current map's decals are placed yet.
#[derive(Resource, Default)]
pub(crate) struct MapDecals {
    map: Option<AssetId<MapAsset>>,
    materials: HashSet<String>,
    placed: bool,
}

/// An `infodecal` to place.
#[derive(Debug, Clone, PartialEq)]
struct InfoDecal {
    /// Its material, relative to `materials/`, lower case.
    material: String,
    /// Where it is, Bevy space.
    origin: Vec3,
}

/// The `infodecal`s that apply themselves as the map starts: not those with a
/// name, which wait to be triggered, nor those barred from deathmatch.
fn info_decals(entities: &[MapEntity]) -> Vec<InfoDecal> {
    entities
        .iter()
        .filter(|entity| entity.classname == "infodecal")
        .filter(|entity| {
            entity.property("targetname").is_none_or(str::is_empty)
                && entity.number("spawnflags").unwrap_or(0.0) as u32 & SF_DECAL_NOT_IN_DEATHMATCH
                    == 0
        })
        .filter_map(|entity| {
            let texture = entity.property("texture")?;
            (!texture.is_empty()).then(|| InfoDecal {
                material: texture.to_ascii_lowercase().replace('\\', "/"),
                origin: coords::position(entity.origin),
            })
        })
        .collect()
}

/// The point of a surface a decal goes on, and which way that surface faces.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SurfaceHit {
    point: Vec3,
    normal: Vec3,
}

/// The closest point to `origin` on any of the `triangles`, if within `reach`.
fn nearest_surface<'a>(
    origin: Vec3,
    triangles: impl IntoIterator<Item = &'a WorldTriangle>,
    reach: f32,
) -> Option<SurfaceHit> {
    let mut best: Option<(f32, SurfaceHit)> = None;
    for triangle in triangles {
        let point = closest_point(origin, triangle.points);
        let distance = origin.distance_squared(point);
        if distance <= reach * reach && best.is_none_or(|(nearest, _)| distance < nearest) {
            let normal = triangle.normal;
            best = Some((distance, SurfaceHit { point, normal }));
        }
    }
    best.map(|(_, hit)| hit)
}

/// The closest point to `p` on the triangle `[a, b, c]`.
fn closest_point(p: Vec3, [a, b, c]: [Vec3; 3]) -> Vec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && d4 >= d3 && d5 >= d6 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denominator = 1.0 / (va + vb + vc);
    a + ab * (vb * denominator) + ac * (vc * denominator)
}

/// The directions a static decal's *u* and *v* grow in on a surface facing
/// `normal`, Source space (`R_DecalComputeBasis`): on a floor or ceiling *u*
/// keeps to +X, and on a wall *v* runs down, so that it stands upright.
fn decal_basis(normal: Vec3) -> (Vec3, Vec3) {
    let (s, t) = if normal.z.abs() > std::f32::consts::FRAC_1_SQRT_2 {
        let t = Vec3::X.cross(normal);
        (normal.cross(t), t)
    } else {
        let s = normal.cross(Vec3::NEG_Z);
        (s, s.cross(normal))
    };
    (s.normalize_or_zero(), t.normalize_or_zero())
}

/// What an `infodecal` of `decal` makes of the surface around `origin`.
fn place_info_decal(
    origin: Vec3,
    size: Vec2,
    triangles: &[WorldTriangle],
) -> Option<DecalGeometry> {
    let reach = REACH_UNITS * coords::METERS_PER_UNIT;
    let hit = nearest_surface(origin, triangles, reach)?;
    let (s, t) = decal_basis(Vec3::from(coords::to_source_direction(hit.normal)));
    let target = DecalBox {
        point: hit.point,
        normal: hit.normal,
        tangent: coords::direction(s.to_array()),
        bitangent: coords::direction(t.to_array()),
        size,
        depth: DEPTH_UNITS * coords::METERS_PER_UNIT,
    };
    let facing: Vec<WorldTriangle> = triangles
        .iter()
        .filter(|triangle| triangle.normal.dot(hit.normal) > MIN_FACING)
        .copied()
        .collect();
    project(&target, &facing, None)
}

/// The cube of the map a point lies in, for merging.
fn chunk_of(point: Vec3) -> [i32; 3] {
    let side = CHUNK_UNITS * coords::METERS_PER_UNIT;
    (point / side).floor().as_ivec3().to_array()
}

/// The resources placing a map's decals draws on.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct DecalResources<'w> {
    server: Res<'w, AssetServer>,
    assets: ResMut<'w, DecalAssets>,
    decals: Res<'w, Assets<DecalAsset>>,
    materials: ResMut<'w, Assets<EffectMaterial>>,
    meshes: ResMut<'w, Assets<Mesh>>,
}

impl DecalResources<'_> {
    /// Whether a material has loaded, or failed to and never will.
    fn settled(&mut self, name: &str) -> bool {
        let handle = self.assets.request(name, &self.server);
        self.decals.contains(&handle) || self.server.load_state(&handle).is_failed()
    }

    /// The loaded asset of a material.
    fn decal(&mut self, name: &str) -> Option<&DecalAsset> {
        let handle = self.assets.request(name, &self.server);
        self.decals.get(&handle)
    }
}

/// Places the current map's decals, once its world is indexed and their
/// materials have loaded.
pub(crate) fn place_map_decals(
    mut commands: Commands,
    map_handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    world: Res<WorldDecals>,
    mut state: ResMut<MapDecals>,
    mut resources: DecalResources,
) {
    let Some((id, map)) = map_handle
        .as_ref()
        .and_then(|handle| Some((handle.0.id(), maps.get(&handle.0)?)))
    else {
        return;
    };
    if world.loaded != Some(id) {
        return;
    }
    let infos = info_decals(&map.entities);
    if state.map != Some(id) {
        *state = MapDecals {
            map: Some(id),
            materials: infos
                .iter()
                .map(|decal| decal.material.clone())
                .chain(map.overlays.iter().map(|mesh| mesh.material.clone()))
                .collect(),
            placed: false,
        };
    }
    if state.placed {
        return;
    }
    let names: Vec<String> = state.materials.iter().cloned().collect();
    if !names.iter().all(|name| resources.settled(name)) {
        return;
    }
    state.placed = true;

    let unplaced = place_info_decals(&mut commands, map, &world, &infos, &mut resources);
    for mesh in &map.overlays {
        let lightmap = map.lightmap.is_some();
        let Some((material, lit)) = resources.assets.material(
            &mesh.material,
            lightmap,
            &resources.decals,
            &mut resources.materials,
        ) else {
            continue;
        };
        let mut overlay = commands.spawn((
            Mesh3d(mesh.mesh.clone()),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            MapScoped,
        ));
        if let (true, Some(image)) = (lit, &map.lightmap) {
            overlay.insert(map_lightmap(image.clone()));
        }
        if mesh.skybox {
            overlay.insert(RenderLayers::layer(SKY_LAYER));
        } else if !mesh.clusters.is_empty() {
            overlay.insert(Clusters(mesh.clusters.clone()));
        }
    }
    info!(
        "placed {} map decals ({unplaced} on no surface) and {} overlay meshes",
        infos.len() - unplaced,
        map.overlays.len()
    );
}

/// Places the map's `infodecal`s, merged by material and cube of the map, and
/// returns how many found no surface.
fn place_info_decals(
    commands: &mut Commands,
    map: &MapAsset,
    world: &WorldDecals,
    infos: &[InfoDecal],
    resources: &mut DecalResources,
) -> usize {
    let mut unplaced = 0;
    let mut batches: HashMap<(String, [i32; 3]), DecalGeometry> = HashMap::new();
    for decal in infos {
        let Some(size) = resources.decal(&decal.material).map(|asset| asset.size) else {
            unplaced += 1;
            continue;
        };
        let size = Vec2::from(size) * coords::METERS_PER_UNIT;
        let radius = size.length() * 0.5 + DEPTH_UNITS * coords::METERS_PER_UNIT;
        let triangles: Vec<WorldTriangle> = world
            .near(decal.origin, radius)
            .into_iter()
            .copied()
            .collect();
        let Some(geometry) = place_info_decal(decal.origin, size, &triangles) else {
            unplaced += 1;
            continue;
        };
        batches
            .entry((decal.material.clone(), chunk_of(decal.origin)))
            .or_default()
            .append(geometry);
    }
    for ((name, _), geometry) in batches {
        let lightmapped = map.lightmap.is_some() && geometry.has_lightmap();
        let Some((material, lit)) = resources.assets.material(
            &name,
            lightmapped,
            &resources.decals,
            &mut resources.materials,
        ) else {
            continue;
        };
        let mesh = resources.meshes.add(geometry.into_mesh(lit));
        let mut entity = commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            MapScoped,
        ));
        if let (true, Some(image)) = (lit, &map.lightmap) {
            entity.insert(map_lightmap(image.clone()));
        }
    }
    unplaced
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A triangle with a right angle at `a`, facing along `(b - a) × (c - a)`.
    fn triangle(a: Vec3, b: Vec3, c: Vec3) -> WorldTriangle {
        WorldTriangle {
            points: [a, b, c],
            normal: (b - a).cross(c - a).normalize(),
            lightmap: None,
        }
    }

    /// A floor at y = 0 and a wall at x = 0 facing +X, meeting along the
    /// z axis, both reaching from the corner out to 4 metres.
    fn corner() -> Vec<WorldTriangle> {
        let (x, y, z) = (Vec3::X * 4.0, Vec3::Y * 4.0, Vec3::Z * 4.0);
        vec![triangle(Vec3::ZERO, z, x), triangle(Vec3::ZERO, y, z)]
    }

    #[test]
    fn only_decals_that_apply_themselves_in_deathmatch_are_placed() {
        let entities = [
            MapEntity::new("infodecal", [100.0, 0.0, 0.0]).with("texture", "Decals\\Stain"),
            MapEntity::new("infodecal", [0.0; 3])
                .with("texture", "decals/named")
                .with("targetname", "wait_for_trigger"),
            MapEntity::new("infodecal", [0.0; 3])
                .with("texture", "decals/not_in_dm")
                .with("spawnflags", "2048"),
            MapEntity::new("infodecal", [0.0; 3]),
            MapEntity::new("info_target", [0.0; 3]).with("texture", "decals/other"),
        ];
        assert_eq!(
            info_decals(&entities),
            [InfoDecal {
                material: "decals/stain".to_owned(),
                origin: coords::position([100.0, 0.0, 0.0]),
            }]
        );
    }

    #[test]
    fn a_decal_goes_on_the_closest_surface_within_reach() {
        let triangles = corner();
        // Half a metre out from the wall, a tenth above the floor.
        let on_floor = Vec3::new(0.5, 0.1, 1.0);
        let hit = nearest_surface(on_floor, &triangles, 0.2).expect("reaches the floor");
        assert!(hit.point.abs_diff_eq(Vec3::new(0.5, 0.0, 1.0), 1e-5));
        assert!(hit.normal.abs_diff_eq(Vec3::Y, 1e-5));
        // A tenth from the wall, half a metre above the floor.
        let on_wall = Vec3::new(0.1, 0.5, 1.0);
        let hit = nearest_surface(on_wall, &triangles, 0.2).expect("reaches the wall");
        assert!(hit.normal.abs_diff_eq(Vec3::X, 1e-5));
        // Out of reach of both.
        assert_eq!(
            nearest_surface(Vec3::new(1.0, 1.0, 1.0), &triangles, 0.2),
            None
        );
        // Beyond a triangle's edge the closest point is on that edge.
        let past = closest_point(Vec3::new(-1.0, 0.0, 1.0), triangles[0].points);
        assert!(past.abs_diff_eq(Vec3::new(0.0, 0.0, 1.0), 1e-5));
    }

    #[test]
    fn a_decal_stands_upright_on_a_wall_and_keeps_to_x_on_a_floor() {
        // A wall facing +X: u runs along +Y, the viewer's right, and v down.
        let (s, t) = decal_basis(Vec3::X);
        assert!(s.abs_diff_eq(Vec3::Y, 1e-5), "{s}");
        assert!(t.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{t}");
        // A floor.
        let (s, t) = decal_basis(Vec3::Z);
        assert!(s.abs_diff_eq(Vec3::X, 1e-5), "{s}");
        assert!(t.abs_diff_eq(Vec3::NEG_Y, 1e-5), "{t}");
        // A steep slope still counts as a wall, a shallow one as a floor.
        assert!(decal_basis(Vec3::new(0.8, 0.0, 0.6).normalize()).1.z < -0.5);
        assert!(decal_basis(Vec3::new(0.6, 0.0, 0.8).normalize()).0.x > 0.5);
    }

    #[test]
    fn an_info_decal_lies_on_its_surface_at_its_own_size() {
        // A wide decal, half a metre by a tenth, near the floor.
        let triangles = corner();
        let origin = Vec3::new(1.0, 0.05, 1.0);
        let size = Vec2::new(0.5, 0.1);
        let geometry = place_info_decal(origin, size, &triangles).expect("placed");
        for (position, normal) in geometry.positions.iter().zip(&geometry.normals) {
            assert!((position[1] - 0.0015).abs() < 1e-5, "lifted off the floor");
            assert!((position[0] - 1.0).abs() <= 0.25 + 1e-5);
            assert!((position[2] - 1.0).abs() <= 0.05 + 1e-5);
            assert!(Vec3::from(*normal).abs_diff_eq(Vec3::Y, 1e-6));
        }
    }

    #[test]
    fn an_info_decal_in_a_corner_keeps_off_the_other_wall() {
        // Right at the corner: the wall is as near as makes no odds, but faces
        // away from the floor it went on.
        let geometry = place_info_decal(Vec3::new(0.02, 0.01, 1.0), Vec2::splat(0.5), &corner())
            .expect("placed");
        assert!(
            geometry
                .normals
                .iter()
                .all(|normal| Vec3::from(*normal).abs_diff_eq(Vec3::Y, 1e-6))
        );
    }

    #[test]
    fn an_info_decal_with_nothing_near_is_not_placed() {
        assert!(place_info_decal(Vec3::splat(3.0), Vec2::ONE, &corner()).is_none());
    }

    #[test]
    fn decals_merge_by_cubes_of_the_map() {
        let side = CHUNK_UNITS * coords::METERS_PER_UNIT;
        assert_eq!(chunk_of(Vec3::splat(0.5)), [0, 0, 0]);
        assert_eq!(chunk_of(Vec3::new(side + 0.1, -0.1, 0.0)), [1, -1, 0]);
    }
}
