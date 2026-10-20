//! The map's static geometry as fixed colliders.
//!
//! Props collide with the same brushes players do, minus the clip brushes
//! meant for players only: every brush that is solid, glass, a grate or a
//! moving brush entity becomes a convex hull of its corners, and brushes
//! that clip both players and NPCs ("tools/toolsclip", which blocks
//! everything) are kept too. Each displacement becomes a triangle mesh.

use glam::Vec3;
use rapier3d::prelude::{
    ColliderBuilder, InteractionGroups, PhysicsWorld as RapierWorld, RigidBodyBuilder,
};

use crate::collision::CollisionWorld;
use crate::collision::contents::{GRATE, MONSTER_CLIP, MOVEABLE, PLAYER_CLIP, SOLID, WINDOW};

/// Source units per metre: rapier scales its tolerances by this.
pub(super) const UNITS_PER_METRE: f32 = 1.0 / 0.0254;

/// Contents that always block props.
const PROP_SOLID: u32 = SOLID | WINDOW | GRATE | MOVEABLE;
/// Clip brushes block props only when they clip everything.
const FULL_CLIP: u32 = PLAYER_CLIP | MONSTER_CLIP;

/// Adds the static geometry of `map` to `rapier` as one fixed body,
/// returning how many colliders it took.
pub(super) fn add_map(
    rapier: &mut RapierWorld,
    map: &CollisionWorld,
    groups: InteractionGroups,
) -> usize {
    let body = rapier.insert_body(RigidBodyBuilder::fixed().user_data(u128::from(u32::MAX)));
    let mut count = 0;
    for corners in map.convex_solids(PROP_SOLID | FULL_CLIP) {
        let Some(builder) = ColliderBuilder::convex_hull(&corners) else {
            continue;
        };
        rapier.insert_collider(builder.collision_groups(groups), Some(body));
        count += 1;
    }
    for triangles in map.displacement_triangles(PROP_SOLID) {
        let vertices: Vec<Vec3> = triangles.iter().flatten().map(|&v| Vec3::from(v)).collect();
        let indices = (0..triangles.len() as u32)
            .map(|triangle| [triangle * 3, triangle * 3 + 1, triangle * 3 + 2])
            .collect();
        match ColliderBuilder::trimesh(vertices, indices) {
            Ok(builder) => {
                rapier.insert_collider(builder.collision_groups(groups), Some(body));
                count += 1;
            }
            Err(error) => tracing::debug!("skipping displacement collider: {error}"),
        }
    }
    count
}
