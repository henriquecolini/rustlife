//! The map's static geometry as `Box3D` colliders.
//!
//! Props collide with the same brushes players do, minus the clip brushes
//! meant for players only: every brush that is solid, glass, a grate or a
//! moving brush entity becomes a convex hull of its corners, and brushes
//! that clip both players and NPCs ("tools/toolsclip", which blocks
//! everything) are kept too. Each displacement becomes a triangle mesh.
//!
//! Movers (doors, buttons, brushes map logic can switch) are not part of the
//! fixed body: each gets a kinematic body of its own, made of the same
//! brushes in the model's frame, which follows the mover as it moves.

use b3::body::create_body;
use b3::math_functions::Vec3 as B3Vec3;
use b3::mesh::{MeshDef, create_mesh};
use b3::shape::{create_hull_shape, create_mesh_shape};
use b3::types::{BodyType, ShapeDef};
use box3d_rust as b3;
use glam::Vec3;

use super::{GROUP_WORLD, MAP_BODY, collision_filter, to_b3};
use crate::collision::CollisionWorld;
use crate::collision::contents::{GRATE, MONSTER_CLIP, MOVEABLE, PLAYER_CLIP, SOLID, WINDOW};

/// Contents that always block props.
const PROP_SOLID: u32 = SOLID | WINDOW | GRATE | MOVEABLE;
/// Clip brushes block props only when they clip everything.
const FULL_CLIP: u32 = PLAYER_CLIP | MONSTER_CLIP;

/// The shape definition of fixed map geometry in `group`.
fn static_shape(group: u64) -> ShapeDef {
    let mut def = b3::default_shape_def();
    def.density = 0.0;
    def.update_body_mass = false;
    def.filter = collision_filter(group);
    def
}

/// Adds the static geometry of `map` to `world` as one fixed body,
/// returning how many shapes it took.
pub(super) fn add_map(world: &mut b3::world::World, map: &CollisionWorld) -> usize {
    let mut body_def = b3::default_body_def();
    body_def.type_ = BodyType::Static;
    body_def.user_data = MAP_BODY;
    let body = create_body(world, &body_def);
    let shape = static_shape(GROUP_WORLD);
    let mut count = 0;
    for corners in map.convex_solids(PROP_SOLID | FULL_CLIP) {
        let points: Vec<B3Vec3> = corners.iter().map(|&corner| to_b3(corner)).collect();
        let Some(hull) = b3::hull::create_hull(&points, b3::constants::MAX_HULL_VERTICES) else {
            continue;
        };
        if create_hull_shape(world, body, &shape, &hull).index1 != 0 {
            count += 1;
        }
    }

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for triangles in map.displacement_triangles(PROP_SOLID) {
        for triangle in triangles {
            let first = i32::try_from(vertices.len()).unwrap_or(i32::MAX);
            for point in triangle {
                vertices.push(to_b3(Vec3::from(*point)));
            }
            indices.extend([first, first + 1, first + 2]);
        }
    }
    if !vertices.is_empty() {
        let def = MeshDef {
            vertices,
            indices,
            ..MeshDef::default()
        };
        if let Some(mesh) = create_mesh(&def, None) {
            if create_mesh_shape(world, body, &shape, &mesh, b3::VEC3_ONE).index1 != 0 {
                count += 1;
            }
        } else {
            tracing::debug!("skipping displacement colliders: Box3D rejected the mesh");
        }
    }
    count
}

/// A mover's kinematic body.
#[derive(Debug, Clone, Copy)]
pub(super) struct MoverBody {
    /// The mover's BSP model.
    pub(super) model: usize,
    pub(super) handle: b3::BodyId,
}

/// Adds a kinematic body for each of `map`'s movers, where it is now.
pub(super) fn add_movers(world: &mut b3::world::World, map: &CollisionWorld) -> Vec<MoverBody> {
    map.movers()
        .filter_map(|model| {
            let (origin, rotation, solid) = map.mover_pose(model)?;
            let mut body_def = b3::default_body_def();
            body_def.type_ = BodyType::Kinematic;
            body_def.position = to_b3(origin);
            body_def.rotation = super::to_b3_quat(rotation);
            body_def.user_data = MAP_BODY;
            let handle = create_body(world, &body_def);
            let shape_def = static_shape(GROUP_WORLD);
            let mut any = false;
            for corners in map.model_solids(model, PROP_SOLID | FULL_CLIP) {
                let points: Vec<B3Vec3> = corners.iter().map(|&corner| to_b3(corner)).collect();
                let Some(hull) = b3::hull::create_hull(&points, b3::constants::MAX_HULL_VERTICES)
                else {
                    continue;
                };
                any |= create_hull_shape(world, handle, &shape_def, &hull).index1 != 0;
            }
            if !solid {
                b3::body::body_disable(world, handle);
            }
            any.then_some(MoverBody { model, handle })
        })
        .collect()
}
