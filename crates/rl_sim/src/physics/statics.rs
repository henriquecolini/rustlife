//! The map's static geometry as fixed colliders.
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

use glam::Vec3;
use rapier3d::prelude::{
    ColliderBuilder, ColliderHandle, InteractionGroups, PhysicsWorld as RapierWorld, Pose,
    RigidBodyBuilder, RigidBodyHandle, SharedShape,
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

/// A mover's kinematic body.
#[derive(Debug, Clone, Copy)]
pub(super) struct MoverBody {
    /// The mover's BSP model.
    pub(super) model: usize,
    pub(super) handle: RigidBodyHandle,
    /// `None` for a model with nothing props collide with.
    pub(super) collider: Option<ColliderHandle>,
}

/// Adds a kinematic body for each of `map`'s movers, where it is now.
pub(super) fn add_movers(
    rapier: &mut RapierWorld,
    map: &CollisionWorld,
    groups: InteractionGroups,
) -> Vec<MoverBody> {
    map.movers()
        .filter_map(|model| {
            let (origin, rotation, solid) = map.mover_pose(model)?;
            let handle = rapier.insert_body(
                RigidBodyBuilder::kinematic_position_based()
                    .pose(Pose::from_parts(origin, rotation))
                    .user_data(u128::from(u32::MAX)),
            );
            let shapes: Vec<(Pose, SharedShape)> = map
                .model_solids(model, PROP_SOLID | FULL_CLIP)
                .iter()
                .filter_map(|corners| SharedShape::convex_hull(corners))
                .map(|shape| (Pose::IDENTITY, shape))
                .collect();
            let collider = (!shapes.is_empty()).then(|| {
                rapier.insert_collider(
                    ColliderBuilder::new(SharedShape::compound(shapes))
                        .collision_groups(groups)
                        .enabled(solid),
                    Some(handle),
                )
            });
            Some(MoverBody {
                model,
                handle,
                collider,
            })
        })
        .collect()
}
