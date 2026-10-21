//! Turning a map's prop placements into physics bodies.
//!
//! A placement says where a model goes and how it collides; the model's
//! `.phy` gives its shape and mass. [`prop_desc`] combines the two (and the
//! data's surface materials) into a [`PropDesc`] ready for
//! [`super::PhysicsWorld::add_prop`]:
//!
//! - **Shape.** The convex pieces of the model's only solid, scaled with the
//!   model. A model without a usable `.phy` — none, unreadable, or a ragdoll
//!   with one solid per bone — collides as its bounding box, as does a
//!   placement that asks for a box.
//! - **Mass.** The `.phy`'s declared mass, times the placement's mass scale;
//!   for a bounding box, the material's density times the box's volume.
//! - **Motion.** Physics props are dynamic; static props and other prop
//!   entities are fixed. A physics prop with "motion disabled" starts fixed
//!   and, if the mapper allowed it, is freed when grabbed.

use glam::{Quat, Vec3};
use rl_data::{MaterialConfig, PhysicsConfig};
use source_formats::phy::PhysicsModel;
use source_formats::props::{PropKind, PropPlacement, PropSolid, spawnflags};

use crate::collision::{ConvexHull, rotation_from_angles};

/// Cubic metres per cubic Source unit (inch).
const CUBIC_METRES_PER_UNIT: f32 = 0.0254 * 0.0254 * 0.0254;
/// Lightest a body may be, kg: keeps the solver away from degenerate masses.
const MIN_MASS: f32 = 0.1;

/// How a body moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Simulated: falls, tumbles, can be pushed and thrown.
    Dynamic,
    /// Never moves (unless freed, see [`PropDesc::free_on_grab`]).
    Fixed,
}

/// Everything needed to add a prop to the physics world.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent behaviour flags of a plain description, mirroring spawnflags"
)]
pub struct PropDesc {
    /// The convex pieces, in the prop's frame (scaled).
    pub pieces: Vec<ConvexHull>,
    /// Position of the prop's origin, Source space.
    pub origin: Vec3,
    /// Orientation.
    pub rotation: Quat,
    /// Whether it is simulated.
    pub motion: Motion,
    /// Mass, kg.
    pub mass: f32,
    /// Surface material.
    pub material: MaterialConfig,
    /// Linear damping, fraction of speed lost per second.
    pub linear_damping: f32,
    /// Angular damping, fraction of spin lost per second.
    pub angular_damping: f32,
    /// Debris: collides with the world and props, not with players.
    pub debris: bool,
    /// Starts at rest until disturbed.
    pub start_asleep: bool,
    /// Players may pick it up with the Manipulator.
    pub grabbable: bool,
    /// A fixed prop that becomes dynamic when grabbed.
    pub free_on_grab: bool,
    /// What it takes to break it, if it can be broken.
    pub breakable: Option<Breakable>,
    /// The surface property of what it is made of, lower case: it names the
    /// sounds it makes. Empty if unknown.
    pub surface: String,
}

/// How a prop breaks: the damage it takes, what it does when it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakable {
    /// Damage the prop takes before breaking.
    pub health: f32,
    /// Multiplier on damage from bullets.
    pub bullets: f32,
    /// Multiplier on damage from melee weapons.
    pub club: f32,
    /// Multiplier on damage from explosions.
    pub explosive: f32,
    /// Breaking it sets off an explosion of this damage and radius (units).
    pub explosion: Option<(f32, f32)>,
    /// The models it breaks into, for the client to scatter.
    pub gibs: Vec<String>,
    /// How many gibs it breaks into.
    pub gib_count: u32,
    /// Seconds after breaking that it comes back where it was, if it does.
    pub respawn: Option<f32>,
}

/// The physics description of `placement`, given its model's collision
/// model (if any), bounds (Source space, model frame) and `$surfaceprop`.
/// `None` for props that collide with nothing.
#[must_use]
pub fn prop_desc(
    placement: &PropPlacement,
    physics: Option<&PhysicsModel>,
    bounds: ([f32; 3], [f32; 3]),
    surface_prop: &str,
    config: &PhysicsConfig,
) -> Option<PropDesc> {
    if placement.solid == PropSolid::None {
        return None;
    }
    let solid = physics
        .filter(|_| placement.solid == PropSolid::Hull)
        .filter(|model| model.solids.len() == 1)
        .and_then(|model| model.solids.first());
    let surface_prop = solid
        .map(|solid| solid.params.surface_prop.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(surface_prop);
    let material = *config.materials.get(surface_prop);

    let scale = placement.scale;
    let hull_pieces: Vec<ConvexHull> = solid
        .map(|solid| {
            solid
                .pieces
                .iter()
                .filter_map(|piece| {
                    let vertices = piece.vertices.iter().map(|&v| Vec3::from(v)).collect();
                    ConvexHull::from_triangles(vertices, &piece.triangles)
                })
                .map(|hull| hull.scaled(scale))
                .collect()
        })
        .unwrap_or_default();
    let (pieces, mass) = if hull_pieces.is_empty() {
        let (mins, maxs) = (Vec3::from(bounds.0) * scale, Vec3::from(bounds.1) * scale);
        let size = (maxs - mins).max(Vec3::ZERO);
        if size.min_element() <= 0.0 {
            return None;
        }
        let volume = size.x * size.y * size.z * CUBIC_METRES_PER_UNIT;
        (
            vec![ConvexHull::from_box(mins, maxs)],
            material.density * volume,
        )
    } else {
        let mass = physics.map_or(1.0, PhysicsModel::mass);
        (hull_pieces, mass)
    };

    let flags = placement.spawnflags;
    let is_physics = placement.kind == PropKind::Physics;
    let motion_disabled = flags & spawnflags::MOTION_DISABLED != 0;
    let (linear_damping, angular_damping) = solid.map_or((0.0, 0.0), |solid| {
        (solid.params.damping, solid.params.rot_damping)
    });
    Some(PropDesc {
        pieces,
        origin: Vec3::from(placement.origin),
        rotation: Quat::from_mat3(&rotation_from_angles(placement.angles)),
        motion: if is_physics && !motion_disabled {
            Motion::Dynamic
        } else {
            Motion::Fixed
        },
        mass: (mass * placement.mass_scale).max(MIN_MASS),
        material,
        linear_damping,
        angular_damping,
        debris: is_physics && flags & spawnflags::DEBRIS != 0,
        start_asleep: flags & spawnflags::START_ASLEEP != 0,
        grabbable: is_physics && flags & spawnflags::PREVENT_PICKUP == 0,
        free_on_grab: is_physics
            && motion_disabled
            && flags & spawnflags::ENABLE_MOTION_ON_GRAB != 0,
        breakable: None,
        surface: surface_prop.to_ascii_lowercase(),
    })
}

#[cfg(test)]
mod tests {
    use source_formats::phy::{self, testing::boxes_phy};

    use super::*;
    use crate::physics::testing::physics_config;

    const BOUNDS: ([f32; 3], [f32; 3]) = ([-10.0, -10.0, 0.0], [10.0, 10.0, 40.0]);

    fn barrel() -> PhysicsModel {
        phy::decode(&boxes_phy(
            &[([-12.0, -12.0, 0.0], [12.0, 12.0, 44.0])],
            30.0,
            "metal_barrel",
        ))
        .expect("decode")
    }

    fn placement(kind: PropKind) -> PropPlacement {
        PropPlacement::new("models/barrel.mdl", [1.0, 2.0, 3.0], [0.0, 90.0, 0.0], kind)
    }

    #[test]
    fn physics_props_use_their_hull_and_declared_mass() {
        let config = physics_config();
        let mut placement = placement(PropKind::Physics);
        placement.mass_scale = 2.0;
        let desc =
            prop_desc(&placement, Some(&barrel()), BOUNDS, "wood", &config).expect("solid prop");
        assert_eq!(desc.motion, Motion::Dynamic);
        assert_eq!(desc.mass, 60.0);
        assert_eq!(desc.pieces.len(), 1);
        assert!(
            desc.pieces[0]
                .bounds()
                .1
                .abs_diff_eq(Vec3::new(12.0, 12.0, 44.0), 1e-3)
        );
        assert_eq!(desc.material, *config.materials.get("metal_barrel"));
        assert!(desc.grabbable && !desc.debris);
        assert!((desc.rotation * Vec3::X).abs_diff_eq(Vec3::Y, 1e-6));
    }

    #[test]
    fn models_without_a_hull_collide_as_their_box() {
        let config = physics_config();
        let desc = prop_desc(&placement(PropKind::Static), None, BOUNDS, "wood", &config)
            .expect("solid prop");
        assert_eq!(desc.motion, Motion::Fixed);
        assert!(!desc.grabbable);
        assert_eq!(
            desc.pieces[0].bounds(),
            (Vec3::new(-10.0, -10.0, 0.0), Vec3::new(10.0, 10.0, 40.0))
        );
        // 20 × 20 × 40 cubic inches of wood.
        let expected = config.materials.get("wood").density * 16_000.0 * CUBIC_METRES_PER_UNIT;
        assert!((desc.mass - expected).abs() < 1e-3);

        let mut boxed = placement(PropKind::Physics);
        boxed.solid = PropSolid::Box;
        let desc = prop_desc(&boxed, Some(&barrel()), BOUNDS, "wood", &config).expect("solid");
        assert_eq!(desc.pieces[0].bounds().1, Vec3::new(10.0, 10.0, 40.0));
    }

    #[test]
    fn spawnflags_shape_behaviour() {
        let config = physics_config();
        let mut flagged = placement(PropKind::Physics);
        flagged.spawnflags = spawnflags::MOTION_DISABLED
            | spawnflags::ENABLE_MOTION_ON_GRAB
            | spawnflags::DEBRIS
            | spawnflags::START_ASLEEP;
        let desc = prop_desc(&flagged, Some(&barrel()), BOUNDS, "", &config).expect("solid");
        assert_eq!(desc.motion, Motion::Fixed);
        assert!(desc.free_on_grab && desc.debris && desc.start_asleep);

        flagged.spawnflags = spawnflags::PREVENT_PICKUP;
        let desc = prop_desc(&flagged, Some(&barrel()), BOUNDS, "", &config).expect("solid");
        assert!(!desc.grabbable);

        flagged.solid = PropSolid::None;
        assert!(prop_desc(&flagged, Some(&barrel()), BOUNDS, "", &config).is_none());
    }

    #[test]
    fn empty_bounds_without_a_hull_do_not_collide() {
        let config = physics_config();
        let flat = ([0.0; 3], [10.0, 10.0, 0.0]);
        assert!(prop_desc(&placement(PropKind::Static), None, flat, "", &config).is_none());
    }
}
