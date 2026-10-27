//! Rigid-body physics: props that fall, tumble, get pushed and get thrown.
//!
//! [`PhysicsWorld`] wraps a [box3d-rust](https://github.com/larsbrubaker/box3d-rust)
//! world kept in Source units (inches, Z up, kilograms). It holds:
//!
//! - **The map**, as fixed colliders: one convex hull per solid brush (the
//!   same brushes players collide with) and one triangle mesh per
//!   displacement (`statics`).
//! - **Props**, one body each whose shapes are the convex pieces of the
//!   model's collision model ([`prop_desc`]): dynamic for physics props,
//!   fixed for static props and other prop entities.
//! - **Players**, as kinematic boxes the simulation moves to wherever
//!   movement put them, so props land on and bump into players rather than
//!   passing through them.
//! - **Joints** the map's constraints hold props by: lamps hanging from
//!   the ceiling, hinged props (`joints`).
//!
//! Players are not simulated by the physics engine: they move with
//! rustlife's own Source-like movement, which collides with props through
//! [`PhysicsWorld::trace_hull`] — the same exact box clip used for brushes,
//! against each piece's planes (`trace`). Walking into a prop pushes it
//! ([`PhysicsWorld::push`]); a prop hitting a player hard enough hurts
//! ([`Impact`], `contacts`).
//!
//! # Determinism
//!
//! Physics runs only on the server (clients interpolate what it sends), so
//! unlike movement it need not be bit-identical across machines. Given the
//! same inputs on the same build it still is: `Box3D` is deterministic, and
//! bodies are only ever visited in insertion order.

use std::sync::Once;

use b3::body::{
    body_apply_linear_impulse, body_compute_aabb, body_disable, body_enable,
    body_get_angular_velocity, body_get_linear_velocity, body_get_type, body_get_world_center,
    body_is_awake, body_is_enabled, body_set_angular_velocity, body_set_awake, body_set_bullet,
    body_set_gravity_scale, body_set_linear_velocity, body_set_target_transform,
    body_set_transform, body_set_type, create_body,
};
use b3::hull::{HullData, make_offset_box_hull};
use b3::math_functions::Vec3 as B3Vec3;
use b3::shape::create_hull_shape;
use b3::types::{BodyType, Filter, QueryFilter};
use b3::world::{
    World, world_set_contact_tuning, world_set_gravity, world_set_hit_event_threshold,
    world_set_maximum_linear_speed,
};
use box3d_rust as b3;
use glam::{Quat, Vec3};
use rl_data::PhysicsConfig;
use serde::{Deserialize, Serialize};

use crate::collision::clip::ClipPlane;
use crate::collision::{CollisionWorld, ConvexHull, Hull};

mod contacts;
mod impacts;
mod joints;
mod props;
mod statics;
mod trace;

pub use contacts::Impact;
pub use impacts::{ContactImpact, Scrape};
pub use joints::{JointDesc, JointKind, MapJoints, map_joints};
pub use props::{Breakable, GibPiece, Motion, PropDesc, prop_desc};

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Source units per metre. Both `Box3D` and the rest of the simulation work in
/// Source units, so this is also `Box3D`'s global unit scale.
pub const UNITS_PER_METRE: f32 = 1.0 / 0.0254;

/// The map's static geometry.
const GROUP_WORLD: u64 = 1 << 0;
/// Props players collide with.
const GROUP_PROP: u64 = 1 << 1;
/// Props players pass through.
const GROUP_DEBRIS: u64 = 1 << 2;
/// Players' kinematic boxes.
const GROUP_PLAYER: u64 = 1 << 3;

/// User data of the map's bodies and movers; they have no [`BodyId`].
const MAP_BODY: u64 = u64::MAX;

/// Squared distance below which a body has not moved since its clip planes
/// were last built.
const MOVED_EPSILON: f32 = 1e-8;

/// Set `Box3D`'s unit scale once, before any world is created.
///
/// `Box3D`'s tolerances and defaults are all scaled by this global value. It
/// has to be the same for every world in the process because the library
/// stores it in a global, so the client's ragdoll world uses Source units
/// too.
pub fn use_source_units() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| b3::core::set_length_units_per_meter(UNITS_PER_METRE));
}

/// A body in the [`PhysicsWorld`]: a prop or a player's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BodyId(u32);

impl BodyId {
    /// The body's index, in the order bodies were added.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// The id of the body at `index`; for tests and tools that refer to
    /// bodies by their order.
    #[must_use]
    pub fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

/// A prop's position and orientation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BodyPose {
    /// The prop's origin (its model's origin), Source space.
    pub position: Vec3,
    /// Its orientation.
    pub rotation: Quat,
}

/// One convex piece of a prop, with its clip planes where the prop was last
/// seen.
#[derive(Debug, Clone)]
struct Piece {
    hull: ConvexHull,
    planes: Vec<ClipPlane>,
    mins: Vec3,
    maxs: Vec3,
}

/// What the world knows about a prop beyond `Box3D`'s body.
#[derive(Debug, Clone)]
struct PropBody {
    pieces: Vec<Piece>,
    mass: f32,
    /// Distance from the centre of mass to the farthest corner.
    radius: f32,
    grabbable: bool,
    free_on_grab: bool,
    /// The pose the pieces' planes were built for.
    planes_pose: BodyPose,
    /// Broken: gone from the world, though its id stays taken.
    broken: bool,
    /// What it is made of.
    surface: String,
    /// Where it was added, for bringing it back.
    home: BodyPose,
}

#[derive(Debug, Clone)]
enum BodyKind {
    Prop(PropBody),
    Player { hull: Hull, velocity: Vec3 },
}

#[derive(Debug, Clone)]
struct Body {
    handle: b3::BodyId,
    shapes: Vec<b3::ShapeId>,
    kind: BodyKind,
}

/// A held (or recently released) prop and the filter joint that keeps it
/// from colliding with its holder.
#[derive(Debug, Clone, Copy)]
struct IgnoredPair {
    player: BodyId,
    prop: BodyId,
    joint: b3::JointId,
}

/// The map's rigid-body world. See the [module docs](self).
pub struct PhysicsWorld {
    world: Mutex<World>,
    bodies: Vec<Body>,
    /// `(player, prop)` pairs where the player holds the prop: they do not
    /// collide.
    held: Vec<IgnoredPair>,
    /// `(player, prop)` pairs where the player let go of the prop while
    /// they still overlapped: they keep ignoring each other until they come
    /// apart, so a prop thrown from inside its holder's box neither shoves
    /// nor hits them.
    released: Vec<IgnoredPair>,
    /// `(player, prop)` pairs in contact after the last step, so an impact
    /// is counted once when contact begins.
    touching: Vec<(BodyId, BodyId)>,
    /// The map's movers' kinematic bodies.
    movers: Vec<statics::MoverBody>,
    /// The fixed body joints hold props to the world by, once one does.
    anchor: Option<b3::BodyId>,
    /// Props that struck props or the map during the last step.
    contacts: Vec<ContactImpact>,
    config: PhysicsConfig,
    dt: f32,
}

impl std::fmt::Debug for PhysicsWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsWorld")
            .field("bodies", &self.bodies.len())
            .field(
                "shapes",
                &b3::world::world_get_counters(&self.b3_world()).shape_count,
            )
            .finish_non_exhaustive()
    }
}

impl PhysicsWorld {
    /// A world holding the static geometry of `map`, stepping `dt` seconds
    /// at a time.
    #[must_use]
    pub fn new(map: &CollisionWorld, config: &PhysicsConfig, dt: f32) -> Self {
        use_source_units();
        let mut def = b3::default_world_def();
        def.gravity = to_b3(Vec3::new(0.0, 0.0, -config.gravity));
        let mut world = World::new(&def);
        world_set_gravity(&mut world, def.gravity);
        world_set_hit_event_threshold(&mut world, config.breakables.impact.report_speed.max(0.0));
        world_set_maximum_linear_speed(&mut world, config.max_speed.max(0.0));
        world_set_contact_tuning(&mut world, 30.0, 10.0, config.max_corrective_speed.max(0.0));
        let shapes = statics::add_map(&mut world, map);
        let movers = statics::add_movers(&mut world, map);
        tracing::debug!(
            shapes,
            movers = movers.len(),
            "built the physics world's static geometry"
        );
        Self {
            world: Mutex::new(world),
            bodies: Vec::new(),
            held: Vec::new(),
            released: Vec::new(),
            touching: Vec::new(),
            movers,
            anchor: None,
            contacts: Vec::new(),
            config: config.clone(),
            dt,
        }
    }

    /// Moves the body of the mover of BSP model `model` to `origin` and
    /// `rotation` by the next step, pushing the props in its way, and
    /// makes it solid or passable. Unknown models are ignored.
    pub fn set_mover(&mut self, model: usize, origin: Vec3, rotation: Quat, solid: bool) {
        let Some(handle) = self
            .movers
            .iter()
            .find(|mover| mover.model == model)
            .map(|mover| mover.handle)
        else {
            return;
        };
        let dt = self.dt;
        let world = self.b3_world_mut();
        body_set_target_transform(
            world,
            handle,
            b3::Transform {
                p: to_b3(origin),
                q: to_b3_quat(rotation),
            },
            dt,
            true,
        );
        if body_is_enabled(world, handle) != solid {
            if solid {
                body_enable(world, handle);
            } else {
                body_disable(world, handle);
            }
        }
    }

    /// Adds a prop.
    pub fn add_prop(&mut self, desc: &PropDesc) -> BodyId {
        let id = self.next_id();
        let pose = BodyPose {
            position: desc.origin,
            rotation: desc.rotation,
        };
        let (hulls, volumes) = prepared_hulls(&desc.pieces);
        let density = if volumes > 0.0 {
            desc.mass / volumes
        } else {
            desc.mass
        };

        let mut body_def = b3::default_body_def();
        body_def.type_ = match desc.motion {
            Motion::Dynamic => BodyType::Dynamic,
            Motion::Fixed => BodyType::Static,
        };
        body_def.position = to_b3(desc.origin);
        body_def.rotation = to_b3_quat(desc.rotation);
        body_def.linear_damping = desc.linear_damping.max(0.0);
        body_def.angular_damping = desc.angular_damping.max(0.0);
        body_def.is_awake = !desc.start_asleep;
        body_def.user_data = u64::from(id.0);
        let handle = create_body(self.b3_world_mut(), &body_def);

        let group = if desc.debris {
            GROUP_DEBRIS
        } else {
            GROUP_PROP
        };
        let mut shape_def = b3::default_shape_def();
        shape_def.density = density.max(f32::EPSILON);
        shape_def.base_material.friction = desc.material.friction;
        shape_def.base_material.restitution = desc.material.restitution;
        shape_def.filter = collision_filter(group);
        shape_def.enable_hit_events = true;
        let shapes = hulls
            .iter()
            .map(|hull| create_hull_shape(self.b3_world_mut(), handle, &shape_def, hull))
            .filter(|shape| shape.index1 != 0)
            .collect();
        if desc.start_asleep {
            body_set_awake(self.b3_world_mut(), handle, false);
        }

        let pieces = desc
            .pieces
            .iter()
            .map(|hull| piece_at(hull.clone(), &pose))
            .collect();
        let center = from_b3(body_get_world_center(&self.b3_world(), handle));
        let radius = desc
            .pieces
            .iter()
            .flat_map(ConvexHull::vertices)
            .map(|&vertex| (desc.rotation * vertex + desc.origin).distance(center))
            .fold(0.0, f32::max);
        self.bodies.push(Body {
            handle,
            shapes,
            kind: BodyKind::Prop(PropBody {
                pieces,
                mass: desc.mass,
                radius,
                grabbable: desc.grabbable,
                free_on_grab: desc.free_on_grab,
                planes_pose: pose,
                broken: false,
                surface: desc.surface.clone(),
                home: pose,
            }),
        });
        id
    }

    /// Adds a player's box at `origin` (the feet).
    pub fn add_player(&mut self, origin: Vec3, hull: &Hull) -> BodyId {
        let id = self.next_id();
        let mut body_def = b3::default_body_def();
        body_def.type_ = BodyType::Kinematic;
        body_def.position = to_b3(origin + hull.center());
        body_def.user_data = u64::from(id.0);
        let handle = create_body(self.b3_world_mut(), &body_def);
        let shape = add_player_shape(self.b3_world_mut(), handle, hull);
        self.bodies.push(Body {
            handle,
            shapes: vec![shape],
            kind: BodyKind::Player {
                hull: *hull,
                velocity: Vec3::ZERO,
            },
        });
        id
    }

    /// Moves a player's box to `origin` with `hull` by the next step;
    /// `velocity` is the player's, for judging impacts.
    pub fn set_player(&mut self, player: BodyId, origin: Vec3, velocity: Vec3, hull: &Hull) {
        let Some(body) = self.bodies.get_mut(player.index()) else {
            return;
        };
        let BodyKind::Player {
            hull: current,
            velocity: current_velocity,
        } = &mut body.kind
        else {
            return;
        };
        *current_velocity = velocity;
        let changed = current != hull;
        if changed {
            *current = *hull;
        }
        let handle = body.handle;
        if changed {
            let old: Vec<_> = std::mem::take(&mut self.bodies[player.index()].shapes);
            for shape in old {
                b3::shape::destroy_shape(self.b3_world_mut(), shape, false);
            }
            let shape = add_player_shape(self.b3_world_mut(), handle, hull);
            self.bodies[player.index()].shapes.push(shape);
        }
        let center = origin + hull.center();
        let dt = self.dt;
        body_set_target_transform(
            self.b3_world_mut(),
            handle,
            b3::Transform {
                p: to_b3(center),
                q: b3::QUAT_IDENTITY,
            },
            dt,
            true,
        );
    }

    /// Takes a player's box out of the world (while they are dead) or puts
    /// it back.
    pub fn set_player_enabled(&mut self, player: BodyId, enabled: bool) {
        let Some(body) = self.bodies.get(player.index()) else {
            return;
        };
        if !matches!(body.kind, BodyKind::Player { .. }) {
            return;
        }
        let handle = body.handle;
        if enabled {
            body_enable(self.b3_world_mut(), handle);
        } else {
            body_disable(self.b3_world_mut(), handle);
        }
    }

    /// Takes a prop out of the simulation, keeping its pose, or puts it
    /// back: how a pickup leaves the world while it is taken. A disabled
    /// prop neither falls, collides nor can be held.
    pub fn set_prop_enabled(&mut self, prop: BodyId, enabled: bool) {
        let Some(body) = self.bodies.get(prop.index()) else {
            return;
        };
        if !matches!(body.kind, BodyKind::Prop(_)) {
            return;
        }
        let handle = body.handle;
        if enabled {
            let world = self.b3_world_mut();
            body_enable(world, handle);
            body_set_gravity_scale(world, handle, 1.0);
        } else {
            body_disable(self.b3_world_mut(), handle);
            self.release_ignored(prop);
        }
    }

    /// Whether a prop is in the world (see [`PhysicsWorld::set_prop_enabled`]).
    #[must_use]
    pub fn prop_enabled(&self, prop: BodyId) -> bool {
        self.bodies
            .get(prop.index())
            .is_some_and(|body| body_is_enabled(&self.b3_world(), body.handle))
            && self.prop_any(prop).is_some()
    }

    /// Puts a prop back where it was added, at rest and enabled: how a
    /// pickup returns when it respawns.
    pub fn reset_prop(&mut self, prop: BodyId) {
        let Some(body) = self.bodies.get(prop.index()) else {
            return;
        };
        let BodyKind::Prop(prop_body) = &body.kind else {
            return;
        };
        if prop_body.broken {
            return;
        }
        let (handle, home) = (body.handle, prop_body.home);
        self.release_ignored(prop);
        {
            let world = self.b3_world_mut();
            body_enable(world, handle);
            body_set_transform(
                world,
                handle,
                to_b3(home.position),
                to_b3_quat(home.rotation),
            );
            body_set_linear_velocity(world, handle, B3Vec3::default());
            body_set_angular_velocity(world, handle, B3Vec3::default());
            body_set_gravity_scale(world, handle, 1.0);
            body_set_awake(world, handle, true);
        }
        self.refresh_planes();
    }

    /// Advances the world by one tick and returns the impacts of props on
    /// players that began during it.
    pub fn step(&mut self) -> Vec<Impact> {
        let before = self.prop_velocities();
        self.sweep_fast_props(&before);
        let sub_steps = i32::try_from(self.config.ccd_substeps.max(1)).unwrap_or(1);
        let dt = self.dt;
        self.b3_world_mut().step(dt, sub_steps);
        self.contacts = self.contact_impacts(&before, self.config.breakables.impact.report_speed);
        self.clamp_velocities();
        self.refresh_planes();
        self.forget_separated();
        self.detect_impacts(&before)
    }

    /// The props' impacts on props and the map during the last
    /// [`step`](PhysicsWorld::step).
    pub fn take_contacts(&mut self) -> Vec<ContactImpact> {
        std::mem::take(&mut self.contacts)
    }

    /// Where a prop is.
    #[must_use]
    pub fn pose(&self, body: BodyId) -> Option<BodyPose> {
        let handle = self.handle(body)?;
        let transform = b3::body::body_get_transform(&self.b3_world(), handle);
        Some(BodyPose {
            position: from_b3(transform.p),
            rotation: from_b3_quat(transform.q),
        })
    }

    /// A body's centre of mass, Source space.
    #[must_use]
    pub fn center_of_mass(&self, body: BodyId) -> Option<Vec3> {
        let handle = self.handle(body)?;
        Some(from_b3(body_get_world_center(&self.b3_world(), handle)))
    }

    /// A body's linear and angular (radians/s) velocity.
    #[must_use]
    pub fn velocity(&self, body: BodyId) -> Option<(Vec3, Vec3)> {
        let handle = self.handle(body)?;
        let world = self.b3_world();
        Some((
            from_b3(body_get_linear_velocity(&world, handle)),
            from_b3(body_get_angular_velocity(&world, handle)),
        ))
    }

    /// A prop's mass, kg.
    #[must_use]
    pub fn mass(&self, body: BodyId) -> Option<f32> {
        self.prop(body).map(|prop| prop.mass)
    }

    /// A prop's radius around its centre of mass.
    #[must_use]
    pub fn radius(&self, body: BodyId) -> Option<f32> {
        self.prop(body).map(|prop| prop.radius)
    }

    /// Whether a prop is simulated (rather than fixed in place).
    #[must_use]
    pub fn is_dynamic(&self, body: BodyId) -> bool {
        self.handle(body)
            .is_some_and(|handle| body_get_type(&self.b3_world(), handle) == BodyType::Dynamic)
    }

    /// Whether players may pick a prop up: simulated (or freed by being
    /// grabbed) and not flagged against it.
    #[must_use]
    pub fn is_grabbable(&self, body: BodyId) -> bool {
        self.prop(body)
            .is_some_and(|prop| prop.grabbable && (prop.free_on_grab || self.is_dynamic(body)))
    }

    /// Whether a prop is asleep (at rest and not simulated until touched).
    #[must_use]
    pub fn is_sleeping(&self, body: BodyId) -> bool {
        let Some(handle) = self.handle(body) else {
            return false;
        };
        let world = self.b3_world();
        body_get_type(&world, handle) == BodyType::Dynamic && !body_is_awake(&world, handle)
    }

    /// Every prop, in the order they were added.
    pub fn props(&self) -> impl Iterator<Item = BodyId> + '_ {
        self.bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| matches!(&body.kind, BodyKind::Prop(prop) if !prop.broken))
            .map(|(index, _)| BodyId(index as u32))
    }

    /// Takes a prop out of the world: it stops colliding, being hit and being
    /// held, and is no longer listed by [`PhysicsWorld::props`]. Its id stays
    /// taken, so every other body keeps its own. Returns whether it was
    /// there to break.
    pub fn break_prop(&mut self, body: BodyId) -> bool {
        let Some(BodyKind::Prop(prop)) = self.bodies.get_mut(body.index()).map(|b| &mut b.kind)
        else {
            return false;
        };
        if std::mem::replace(&mut prop.broken, true) {
            return false;
        }
        let handle = self.bodies[body.index()].handle;
        self.release_ignored(body);
        body_disable(self.b3_world_mut(), handle);
        true
    }

    /// Where a prop was added: its home.
    #[must_use]
    pub fn home_position(&self, body: BodyId) -> Option<Vec3> {
        self.prop_any(body).map(|prop| prop.home.position)
    }

    /// Brings a broken prop back where it was added, at rest. Returns
    /// whether it was broken.
    pub fn restore_prop(&mut self, body: BodyId) -> bool {
        let Some(BodyKind::Prop(prop)) = self.bodies.get_mut(body.index()).map(|b| &mut b.kind)
        else {
            return false;
        };
        if !std::mem::replace(&mut prop.broken, false) {
            return false;
        }
        let home = prop.home;
        prop.planes_pose = home;
        let handle = self.bodies[body.index()].handle;
        {
            let world = self.b3_world_mut();
            body_enable(world, handle);
            body_set_transform(
                world,
                handle,
                to_b3(home.position),
                to_b3_quat(home.rotation),
            );
            body_set_linear_velocity(world, handle, B3Vec3::default());
            body_set_angular_velocity(world, handle, B3Vec3::default());
            body_set_awake(world, handle, true);
        }
        self.refresh_planes();
        true
    }

    /// The surface property a prop is made of, lower case (empty if unknown).
    #[must_use]
    pub fn surface(&self, body: BodyId) -> &str {
        self.prop_any(body).map_or("", |prop| prop.surface.as_str())
    }

    /// Whether a prop has been broken.
    #[must_use]
    pub fn is_broken(&self, body: BodyId) -> bool {
        self.prop_any(body).is_some_and(|prop| prop.broken)
    }

    /// Every prop that has been broken.
    pub fn broken(&self) -> impl Iterator<Item = BodyId> + '_ {
        self.bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| matches!(&body.kind, BodyKind::Prop(prop) if prop.broken))
            .map(|(index, _)| BodyId(index as u32))
    }

    /// Places a prop at `pose` at once, without simulating the move: how a
    /// client's replica follows the poses the server sends. Collision
    /// queries see the new pose after [`PhysicsWorld::refresh`].
    pub fn set_pose(&mut self, body: BodyId, pose: BodyPose) {
        if self.prop(body).is_none() {
            return;
        }
        let Some(handle) = self.handle(body) else {
            return;
        };
        body_set_transform(
            self.b3_world_mut(),
            handle,
            to_b3(pose.position),
            to_b3_quat(pose.rotation),
        );
    }

    /// Brings collision queries up to date after props were placed with
    /// [`PhysicsWorld::set_pose`], without advancing time.
    pub fn refresh(&mut self) {
        self.refresh_planes();
    }

    /// Sets a dynamic body's velocities (angular in radians/s), waking it.
    pub fn set_velocity(&mut self, body: BodyId, linear: Vec3, angular: Vec3) {
        let Some(handle) = self.handle(body) else {
            return;
        };
        let world = self.b3_world_mut();
        if body_get_type(world, handle) != BodyType::Dynamic {
            return;
        }
        body_set_linear_velocity(world, handle, to_b3(linear));
        body_set_angular_velocity(world, handle, to_b3(angular));
    }

    /// Applies `impulse` (kg·units/s) at `point`, waking the body.
    pub fn apply_impulse(&mut self, body: BodyId, impulse: Vec3, point: Vec3) {
        let Some(handle) = self.handle(body) else {
            return;
        };
        let world = self.b3_world_mut();
        if body_get_type(world, handle) == BodyType::Dynamic {
            body_apply_linear_impulse(world, handle, to_b3(impulse), to_b3(point), true);
        }
    }

    /// Marks `prop` as held by `player` (or released, with `None`): a held
    /// prop ignores gravity and does not collide with its holder. A fixed
    /// prop that may be freed by grabbing becomes dynamic.
    pub fn set_held(&mut self, prop: BodyId, player: Option<BodyId>) {
        let mut moved = Vec::new();
        self.held.retain(|pair| {
            if pair.prop != prop {
                return true;
            }
            moved.push(*pair);
            false
        });
        let mut old_released = Vec::new();
        self.released.retain(|pair| {
            if pair.prop != prop {
                return true;
            }
            old_released.push(*pair);
            false
        });
        let mut to_destroy = old_released;
        if player.is_none() {
            self.released.extend(moved);
        } else {
            to_destroy.extend(moved);
        }
        if !to_destroy.is_empty() {
            let world = self.b3_world_mut();
            for pair in to_destroy {
                b3::joint::destroy_joint(world, pair.joint, true);
            }
        }

        let free = self.prop(prop).is_some_and(|body| body.free_on_grab);
        let Some(prop_handle) = self.handle(prop) else {
            return;
        };
        {
            let world = self.b3_world_mut();
            if player.is_some() && free && body_get_type(world, prop_handle) != BodyType::Dynamic {
                body_set_type(world, prop_handle, BodyType::Dynamic);
            }
            body_set_gravity_scale(world, prop_handle, if player.is_some() { 0.0 } else { 1.0 });
        }

        let Some(player) = player else {
            return;
        };
        let Some(player_handle) = self.handle(player) else {
            return;
        };
        let mut def = b3::default_filter_joint_def();
        def.base.body_id_a = player_handle;
        def.base.body_id_b = prop_handle;
        def.base.collide_connected = false;
        let joint = b3::joint::create_filter_joint(self.b3_world_mut(), &def);
        if joint.index1 != 0 {
            self.held.push(IgnoredPair {
                player,
                prop,
                joint,
            });
        }
    }

    /// Whether `player` and `prop` ignore each other: the player holds the
    /// prop, or let go of it and has not come apart from it yet.
    #[must_use]
    pub fn ignores(&self, player: BodyId, prop: BodyId) -> bool {
        self.held
            .iter()
            .any(|pair| pair.player == player && pair.prop == prop)
            || self
                .released
                .iter()
                .any(|pair| pair.player == player && pair.prop == prop)
    }

    /// Stops ignoring released pairs whose bounds no longer overlap.
    fn forget_separated(&mut self) {
        let separated: Vec<IgnoredPair> = self
            .released
            .iter()
            .copied()
            .filter(|pair| {
                let (Some(player), Some(prop)) = (self.handle(pair.player), self.handle(pair.prop))
                else {
                    return true;
                };
                let a = body_compute_aabb(&self.b3_world(), player);
                let b = body_compute_aabb(&self.b3_world(), prop);
                !aabb_overlap(a, b)
            })
            .collect();
        for pair in &separated {
            b3::joint::destroy_joint(self.b3_world_mut(), pair.joint, true);
        }
        self.released
            .retain(|pair| !separated.iter().any(|gone| gone.joint == pair.joint));
    }

    /// The player holding `prop`, if any.
    #[must_use]
    pub fn holder(&self, prop: BodyId) -> Option<BodyId> {
        self.held
            .iter()
            .find(|pair| pair.prop == prop)
            .map(|pair| pair.player)
    }

    /// Pushes `prop` away from a player who walked into it: the player,
    /// moving at `speed` into the prop's surface with outward `normal`,
    /// meets it as a body of the configured mass in an inelastic collision.
    pub fn push(&mut self, prop: BodyId, normal: Vec3, speed: f32) {
        let push = &self.config.push;
        let (Some(mass), Some(center)) = (self.mass(prop), self.center_of_mass(prop)) else {
            return;
        };
        if speed <= 0.0 || !self.is_dynamic(prop) {
            return;
        }
        let change =
            (speed * push.player_mass / (push.player_mass + mass)).min(push.max_speed_change);
        self.apply_impulse(prop, -normal * change * mass, center);
    }

    /// The `Box3D` world, for reads.
    fn b3_world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The `Box3D` world, for writes. `&mut self` already guarantees exclusive
    /// access, so no runtime lock is needed.
    fn b3_world_mut(&mut self) -> &mut World {
        self.world.get_mut().unwrap_or_else(PoisonError::into_inner)
    }

    fn next_id(&self) -> BodyId {
        BodyId(u32::try_from(self.bodies.len()).unwrap_or(u32::MAX))
    }

    fn handle(&self, body: BodyId) -> Option<b3::BodyId> {
        self.bodies.get(body.index()).map(|body| body.handle)
    }

    fn body_id(&self, handle: b3::BodyId) -> Option<BodyId> {
        self.bodies
            .iter()
            .position(|body| body.handle == handle)
            .map(|index| BodyId(index as u32))
    }

    /// Destroys the filter joints that made `prop` ignore players.
    fn release_ignored(&mut self, prop: BodyId) {
        let mut joints = Vec::new();
        self.held.retain(|pair| {
            if pair.prop != prop {
                return true;
            }
            joints.push(pair.joint);
            false
        });
        self.released.retain(|pair| {
            if pair.prop != prop {
                return true;
            }
            joints.push(pair.joint);
            false
        });
        for joint in joints {
            b3::joint::destroy_joint(self.b3_world_mut(), joint, true);
        }
    }

    /// A prop that is still in the world.
    fn prop(&self, body: BodyId) -> Option<&PropBody> {
        self.prop_any(body).filter(|prop| !prop.broken)
    }

    /// A prop, broken or not.
    fn prop_any(&self, body: BodyId) -> Option<&PropBody> {
        match &self.bodies.get(body.index())?.kind {
            BodyKind::Prop(prop) => Some(prop),
            BodyKind::Player { .. } => None,
        }
    }

    /// The linear velocity of every awake dynamic prop, before a step.
    fn prop_velocities(&self) -> Vec<(BodyId, Vec3)> {
        let world = self.b3_world();
        self.props()
            .filter_map(|id| {
                let handle = self.handle(id)?;
                let dynamic = body_get_type(&world, handle) == BodyType::Dynamic;
                (dynamic && body_is_awake(&world, handle))
                    .then(|| (id, from_b3(body_get_linear_velocity(&world, handle))))
            })
            .collect()
    }

    /// Makes the props moving faster than `ccd_min_speed` bullets for this
    /// step: `Box3D` sweeps those against the other props in their way, which
    /// stops a hard throw from passing through a crate, while slow props
    /// (almost all of them, almost all of the time) cost nothing extra.
    fn sweep_fast_props(&mut self, velocities: &[(BodyId, Vec3)]) {
        let threshold = self.config.ccd_min_speed;
        for &(id, velocity) in velocities {
            let fast = velocity.length() > threshold;
            let Some(handle) = self.handle(id) else {
                continue;
            };
            body_set_bullet(self.b3_world_mut(), handle, fast);
        }
    }

    /// Enforces the configured speed limits.
    fn clamp_velocities(&mut self) {
        let max_speed = self.config.max_speed;
        let max_spin = self.config.max_angular_speed.to_radians();
        let world = self.world.get_mut().unwrap_or_else(PoisonError::into_inner);
        let bodies = &self.bodies;
        for body in bodies {
            let handle = body.handle;
            if body_get_type(world, handle) != BodyType::Dynamic || !body_is_awake(world, handle) {
                continue;
            }
            let linear = from_b3(body_get_linear_velocity(world, handle));
            if linear.length() > max_speed {
                body_set_linear_velocity(world, handle, to_b3(linear.clamp_length_max(max_speed)));
            }
            let angular = from_b3(body_get_angular_velocity(world, handle));
            if angular.length() > max_spin {
                body_set_angular_velocity(world, handle, to_b3(angular.clamp_length_max(max_spin)));
            }
        }
    }

    /// Rebuilds the clip planes of every prop that moved.
    fn refresh_planes(&mut self) {
        let world = self.world.get_mut().unwrap_or_else(PoisonError::into_inner);
        for body in &mut self.bodies {
            let BodyKind::Prop(prop) = &mut body.kind else {
                continue;
            };
            let transform = b3::body::body_get_transform(world, body.handle);
            let pose = BodyPose {
                position: from_b3(transform.p),
                rotation: from_b3_quat(transform.q),
            };
            if !moved(&prop.planes_pose, &pose) {
                continue;
            }
            for piece in &mut prop.pieces {
                (piece.mins, piece.maxs) =
                    piece
                        .hull
                        .world_planes(pose.rotation, pose.position, &mut piece.planes);
            }
            prop.planes_pose = pose;
        }
    }
}

/// Adds a player's box shape to `handle`.
fn add_player_shape(world: &mut World, handle: b3::BodyId, hull: &Hull) -> b3::ShapeId {
    let extents = hull.extents();
    let box_hull = b3::hull::make_box_hull(extents.x, extents.y, extents.z);
    let mut shape_def = b3::default_shape_def();
    shape_def.density = 0.0;
    shape_def.update_body_mass = false;
    shape_def.filter = collision_filter(GROUP_PLAYER);
    shape_def.enable_contact_events = true;
    create_hull_shape(world, handle, &shape_def, &box_hull.base)
}

/// Converts a piece of our collision hull to `Box3D`'s hull, falling back to a
/// box if the point set is degenerate. Returns the hulls and their total
/// volume in cubic Source units.
fn prepared_hulls(pieces: &[ConvexHull]) -> (Vec<HullData>, f32) {
    let mut hulls = Vec::with_capacity(pieces.len());
    let mut volume = 0.0;
    for piece in pieces {
        let points: Vec<B3Vec3> = piece.vertices().iter().map(|&point| to_b3(point)).collect();
        let hull =
            b3::hull::create_hull(&points, b3::constants::MAX_HULL_VERTICES).unwrap_or_else(|| {
                let (mins, maxs) = piece.bounds();
                let half = ((maxs - mins) * 0.5).max(Vec3::splat(0.05));
                let center = (mins + maxs) * 0.5;
                make_offset_box_hull(half.x, half.y, half.z, to_b3(center)).base
            });
        volume += hull.volume.max(0.0);
        hulls.push(hull);
    }
    (hulls, volume)
}

/// A piece with its planes built for `pose`.
fn piece_at(hull: ConvexHull, pose: &BodyPose) -> Piece {
    let mut planes = Vec::new();
    let (mins, maxs) = hull.world_planes(pose.rotation, pose.position, &mut planes);
    Piece {
        hull,
        planes,
        mins,
        maxs,
    }
}

/// Whether two poses differ.
///
/// Rotations are compared exactly: a body at rest (fixed, or asleep) keeps
/// its pose bit for bit, while any tolerance on the dot product of two
/// rotations would drown in `f32` rounding (`1.0 - 1e-8` *is* `1.0`), so
/// every rotated prop whose rotation is not exactly unit length would seem
/// to move every tick and have its planes rebuilt for nothing.
fn moved(a: &BodyPose, b: &BodyPose) -> bool {
    a.position.distance_squared(b.position) > MOVED_EPSILON || a.rotation != b.rotation
}

/// The collision filter of a body's shapes in `group`: who it collides with.
fn collision_filter(group: u64) -> Filter {
    let filter = if group == GROUP_WORLD {
        GROUP_PROP | GROUP_DEBRIS
    } else if group == GROUP_PROP {
        GROUP_WORLD | GROUP_PROP | GROUP_DEBRIS | GROUP_PLAYER
    } else if group == GROUP_DEBRIS {
        GROUP_WORLD | GROUP_PROP
    } else {
        GROUP_PROP
    };
    Filter {
        category_bits: group,
        mask_bits: filter,
        group_index: 0,
    }
}

/// A query filter for the props in `groups`.
fn query_filter(groups: u64) -> QueryFilter {
    QueryFilter {
        category_bits: GROUP_PROP,
        mask_bits: groups,
        id: 0,
        name: String::new(),
    }
}

fn aabb_overlap(a: b3::Aabb, b: b3::Aabb) -> bool {
    a.lower_bound.x <= b.upper_bound.x
        && b.lower_bound.x <= a.upper_bound.x
        && a.lower_bound.y <= b.upper_bound.y
        && b.lower_bound.y <= a.upper_bound.y
        && a.lower_bound.z <= b.upper_bound.z
        && b.lower_bound.z <= a.upper_bound.z
}

pub(crate) fn to_b3(value: Vec3) -> B3Vec3 {
    B3Vec3 {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}

pub(crate) fn from_b3(value: B3Vec3) -> Vec3 {
    Vec3::new(value.x, value.y, value.z)
}

pub(crate) fn to_b3_quat(value: Quat) -> b3::Quat {
    b3::Quat {
        v: to_b3(value.xyz()),
        s: value.w,
    }
}

pub(crate) fn from_b3_quat(value: b3::Quat) -> Quat {
    Quat::from_xyzw(value.v.x, value.v.y, value.v.z, value.s)
}

/// Shared test fixtures.
#[cfg(test)]
pub(crate) mod testing {
    use rl_data::{GameData, PhysicsConfig};

    /// The built-in physics data.
    pub(crate) fn physics_config() -> PhysicsConfig {
        GameData::builtin().expect("builtin data").physics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pose_at_rest_has_not_moved() {
        // Not exactly unit length, as rotations built from a map's angles
        // often are: its dot product with itself rounds below 1.
        let rotation = Quat::from_xyzw(0.5, 0.5, 0.5, 0.499_999_9);
        assert!(rotation.dot(rotation) < 1.0);
        let pose = BodyPose {
            position: Vec3::new(10.0, -4.0, 2.0),
            rotation,
        };
        assert!(!moved(&pose, &pose));
    }

    #[test]
    fn moving_or_turning_at_all_is_moving() {
        let pose = BodyPose {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
        };
        let slid = BodyPose {
            position: Vec3::new(0.01, 0.0, 0.0),
            ..pose
        };
        let turned = BodyPose {
            rotation: Quat::from_rotation_z(1e-3),
            ..pose
        };
        let crept = BodyPose {
            position: Vec3::new(1e-5, 0.0, 0.0),
            ..pose
        };
        assert!(moved(&pose, &slid));
        assert!(moved(&pose, &turned));
        assert!(!moved(&pose, &crept));
    }
}
