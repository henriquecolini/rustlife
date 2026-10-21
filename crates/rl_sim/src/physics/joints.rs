//! Constraints: props a map hangs, hinges or ties to the world or to each
//! other.
//!
//! Source maps join physics objects with constraint entities. The lamps of
//! `dm_steamlab` and `dm_powerhouse` hang from the ceiling on a
//! `phys_ballsocket`; without it they fall the moment the map starts. Each
//! constraint names the two things it joins (`attach1`, `attach2`); a name
//! that is empty, or names something that is not a simulated prop (the
//! constraint itself, an `info_target`), stands for the world.
//!
//! | Entity | Joint |
//! |--------|-------|
//! | `phys_ballsocket` | the two turn freely about `origin` |
//! | `phys_hinge` | they turn about the line from `origin` to `hingeaxis` only |
//! | `phys_lengthconstraint` | `origin` on the first and `attachpoint` on the second stay at most their distance plus `addlength` apart |
//! | `phys_constraint` | welded as they were placed |
//!
//! Every joint holds the bodies as the map placed them: its frame on each
//! body is where the joint is at spawn, so a hinge's angle and a weld's
//! offset are measured from there. Constraints that start off (`Start
//! inactive`), breaking under force (`forcelimit`, `torquelimit`) and
//! motors (`phys_motor`) are not simulated; `No collision until break`
//! keeps the joined bodies from colliding with each other.

use glam::{Quat, Vec3};
use rapier3d::prelude::{GenericJointBuilder, JointAxesMask, Pose, RopeJointBuilder};
use source_formats::entities::MapEntity;

use super::{BodyId, PhysicsWorld};

/// Constraint `spawnflags`.
mod flags {
    /// The joined bodies do not collide with each other.
    pub(super) const NO_COLLISION: u32 = 1;
    /// Off until turned on by map logic.
    pub(super) const START_INACTIVE: u32 = 4;
}

/// How a joint holds its bodies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JointKind {
    /// Turning freely about the pivot.
    Ball,
    /// Turning about `axis` (a direction, Source space) only.
    Hinge {
        /// The axis through the pivot.
        axis: Vec3,
    },
    /// The anchors at most `length` units apart.
    Rope {
        /// Most distance between the anchors, units.
        length: f32,
    },
    /// Held as placed.
    Weld,
}

/// A joint between two bodies, or a body and the world, as the map placed
/// it (Source space).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointDesc {
    /// How it holds them.
    pub kind: JointKind,
    /// The first body; `None` for the world.
    pub body1: Option<BodyId>,
    /// The second body; `None` for the world.
    pub body2: Option<BodyId>,
    /// Where the joint holds the first body: the pivot, or a rope's first
    /// end.
    pub anchor1: Vec3,
    /// Where it holds the second: the pivot again, or a rope's other end.
    pub anchor2: Vec3,
    /// Whether the two bodies collide with each other.
    pub collide: bool,
}

/// What became of a map's constraint entities.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapJoints {
    /// The joints to add.
    pub joints: Vec<JointDesc>,
    /// Why each constraint left out was: one line per entity.
    pub skipped: Vec<String>,
}

/// The joints of a map's constraint entities, finding the props they name
/// through `body_named` (a prop's `targetname` to its body).
#[must_use]
pub fn map_joints(
    entities: &[MapEntity],
    body_named: impl Fn(&str) -> Option<BodyId>,
) -> MapJoints {
    let mut result = MapJoints::default();
    for entity in entities {
        let class = entity.classname.as_str();
        let what = || {
            format!(
                "{class} {}",
                entity.property("targetname").unwrap_or("(unnamed)")
            )
        };
        if class == "phys_motor" {
            result
                .skipped
                .push(format!("{}: motors are not simulated", what()));
            continue;
        }
        let Some(kind) = joint_kind(entity) else {
            continue;
        };
        let spawnflags = entity.number("spawnflags").map_or(0, |flags| flags as u32);
        if spawnflags & flags::START_INACTIVE != 0 {
            result.skipped.push(format!("{}: starts inactive", what()));
            continue;
        }
        let attached = |key: &str| {
            entity
                .property(key)
                .filter(|name| !name.is_empty())
                .map(|name| (name, body_named(name)))
        };
        let (named1, named2) = (attached("attach1"), attached("attach2"));
        let body1 = named1.and_then(|(_, body)| body);
        let body2 = named2.and_then(|(_, body)| body);
        if body1.is_none() && body2.is_none() {
            let names: Vec<&str> = [named1, named2]
                .into_iter()
                .flatten()
                .map(|(name, _)| name)
                .collect();
            result.skipped.push(format!(
                "{}: joins no simulated prop (attaches {names:?})",
                what()
            ));
            continue;
        }
        let origin = Vec3::from(entity.origin);
        let anchor2 = match kind {
            JointKind::Rope { .. } => entity.vector("attachpoint").map_or(origin, Vec3::from),
            _ => origin,
        };
        let kind = match kind {
            JointKind::Rope { length } => JointKind::Rope {
                length: origin.distance(anchor2) + length,
            },
            other => other,
        };
        result.joints.push(JointDesc {
            kind,
            body1,
            body2,
            anchor1: origin,
            anchor2,
            collide: spawnflags & flags::NO_COLLISION == 0,
        });
    }
    result
}

/// The kind of joint a constraint entity makes (a rope's `length` is still
/// only its `addlength`), or `None` if it is not one.
fn joint_kind(entity: &MapEntity) -> Option<JointKind> {
    Some(match entity.classname.as_str() {
        "phys_ballsocket" => JointKind::Ball,
        "phys_hinge" => {
            let origin = Vec3::from(entity.origin);
            let axis = entity
                .vector("hingeaxis")
                .and_then(|point| (Vec3::from(point) - origin).try_normalize())
                .unwrap_or(Vec3::Z);
            JointKind::Hinge { axis }
        }
        "phys_lengthconstraint" => JointKind::Rope {
            length: entity.number("addlength").unwrap_or(0.0).max(0.0),
        },
        "phys_constraint" => JointKind::Weld,
        _ => return None,
    })
}

impl PhysicsWorld {
    /// Joins bodies as `joint` says. Returns `false` (and adds nothing) if
    /// it names a body that does not exist.
    pub fn add_joint(&mut self, joint: &JointDesc) -> bool {
        let handle = |world: &mut Self, body: Option<BodyId>| match body {
            Some(body) => world.handle(body),
            None => Some(world.anchor()),
        };
        let (Some(handle1), Some(handle2)) = (handle(self, joint.body1), handle(self, joint.body2))
        else {
            return false;
        };
        let pose = |world: &Self, handle| {
            world
                .rapier
                .bodies
                .get(handle)
                .map_or(Pose::IDENTITY, |rigid| *rigid.position())
        };
        let (pose1, pose2) = (pose(self, handle1), pose(self, handle2));
        // Each body's frame is the joint as placed, in the body's own
        // space: the joint's X axis along a hinge's axis.
        let turn = match joint.kind {
            JointKind::Hinge { axis } => Quat::from_rotation_arc(Vec3::X, axis),
            _ => Quat::IDENTITY,
        };
        let frame = |pose: &Pose, anchor: Vec3| {
            Pose::from_parts(
                pose.inverse_transform_point(anchor),
                pose.rotation.inverse() * turn,
            )
        };
        let frame1 = frame(&pose1, joint.anchor1);
        let frame2 = frame(&pose2, joint.anchor2);
        let data = match joint.kind {
            JointKind::Rope { length } => {
                RopeJointBuilder::new(length)
                    .local_anchor1(frame1.translation)
                    .local_anchor2(frame2.translation)
                    .contacts_enabled(joint.collide)
                    .build()
                    .data
            }
            kind => {
                let locked = match kind {
                    JointKind::Ball => JointAxesMask::LOCKED_SPHERICAL_AXES,
                    JointKind::Hinge { .. } => JointAxesMask::LOCKED_REVOLUTE_AXES,
                    _ => JointAxesMask::LOCKED_FIXED_AXES,
                };
                GenericJointBuilder::new(locked)
                    .local_frame1(frame1)
                    .local_frame2(frame2)
                    .contacts_enabled(joint.collide)
                    .build()
            }
        };
        self.rapier
            .impulse_joints
            .insert(handle1, handle2, data, false);
        true
    }

    /// The fixed body joints hold props to the world by, made on first use.
    fn anchor(&mut self) -> rapier3d::prelude::RigidBodyHandle {
        if let Some(anchor) = self.anchor {
            return anchor;
        }
        let anchor = self
            .rapier
            .bodies
            .insert(rapier3d::prelude::RigidBodyBuilder::fixed().build());
        self.anchor = Some(anchor);
        anchor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Option<BodyId> {
        match name {
            "lamp" => Some(BodyId(3)),
            "crate" => Some(BodyId(5)),
            _ => None,
        }
    }

    #[test]
    fn a_lamp_hangs_from_the_world_on_a_ball_socket() {
        // As dm_steamlab hangs them: attach2 left empty.
        let socket = MapEntity::new("phys_ballsocket", [10.0, 20.0, 100.0]).with("attach1", "lamp");
        // As dm_powerhouse does: attach1 names the constraint itself.
        let own = MapEntity::new("phys_ballsocket", [0.0, 0.0, 50.0])
            .with("targetname", "ball")
            .with("attach1", "ball")
            .with("attach2", "lamp");
        let joints = map_joints(&[socket, own], named);
        assert!(joints.skipped.is_empty());
        let [first, second] = joints.joints.as_slice() else {
            panic!("two joints: {joints:?}");
        };
        assert_eq!(first.kind, JointKind::Ball);
        assert_eq!((first.body1, first.body2), (Some(BodyId(3)), None));
        assert_eq!(first.anchor1, Vec3::new(10.0, 20.0, 100.0));
        assert!(first.collide);
        assert_eq!((second.body1, second.body2), (None, Some(BodyId(3))));
    }

    #[test]
    fn hinges_turn_about_the_line_to_their_axis_point() {
        let hinge = MapEntity::new("phys_hinge", [0.0, 0.0, 0.0])
            .with("attach1", "crate")
            .with("hingeaxis", "0 32 0")
            .with("spawnflags", "1");
        let joints = map_joints(&[hinge], named);
        assert_eq!(joints.joints[0].kind, JointKind::Hinge { axis: Vec3::Y });
        assert!(!joints.joints[0].collide);
    }

    #[test]
    fn length_constraints_reach_their_distance_plus_the_slack() {
        let rope = MapEntity::new("phys_lengthconstraint", [0.0, 0.0, 0.0])
            .with("attach1", "crate")
            .with("attachpoint", "30 40 0")
            .with("addlength", "16");
        let joints = map_joints(&[rope], named);
        let joint = joints.joints[0];
        assert_eq!(joint.kind, JointKind::Rope { length: 66.0 });
        assert_eq!(joint.anchor2, Vec3::new(30.0, 40.0, 0.0));
    }

    #[test]
    fn what_cannot_be_joined_is_said() {
        let entities = [
            MapEntity::new("phys_ballsocket", [0.0; 3]).with("attach1", "physbox"),
            MapEntity::new("phys_ballsocket", [0.0; 3])
                .with("attach1", "lamp")
                .with("spawnflags", "4"),
            MapEntity::new("phys_motor", [0.0; 3]).with("attach1", "crate"),
            MapEntity::new("prop_physics", [0.0; 3]).with("targetname", "lamp"),
        ];
        let joints = map_joints(&entities, named);
        assert!(joints.joints.is_empty());
        assert_eq!(joints.skipped.len(), 3, "{:?}", joints.skipped);
    }
}
