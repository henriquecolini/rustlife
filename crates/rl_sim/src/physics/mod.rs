//! Rigid-body physics: props that fall, tumble, get pushed and get thrown.
//!
//! [`PhysicsWorld`] wraps a `rapier3d` world kept in Source units (inches,
//! Z up, kilograms). It holds:
//!
//! - **The map**, as fixed colliders: one convex hull per solid brush (the
//!   same brushes players collide with) and one triangle mesh per
//!   displacement (`statics`).
//! - **Props**, one body each whose collider is the convex pieces of the
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
//! same inputs on the same build it still is: bodies are only ever visited in
//! insertion order.

use glam::{Quat, Vec3};
use rapier3d::parry::bounding_volume::BoundingVolume;
use rapier3d::prelude::{
    ActiveEvents, ActiveHooks, ColliderBuilder, ColliderHandle, Group, InteractionGroups,
    InteractionTestMode, PhysicsWorld as RapierWorld, Pose, RigidBodyBuilder, RigidBodyHandle,
    RigidBodyType, SharedShape,
};
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

/// The map's static geometry.
const GROUP_WORLD: Group = Group::GROUP_1;
/// Props players collide with.
const GROUP_PROP: Group = Group::GROUP_2;
/// Props players pass through.
const GROUP_DEBRIS: Group = Group::GROUP_3;
/// Players' kinematic boxes.
const GROUP_PLAYER: Group = Group::GROUP_4;

/// Squared distance below which a body has not moved since its clip planes
/// were last built.
const MOVED_EPSILON: f32 = 1e-8;

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

/// What the world knows about a prop beyond rapier's body.
#[derive(Debug, Clone)]
struct PropBody {
    pieces: Vec<Piece>,
    mass: f32,
    /// Distance from the centre of mass to the farthest corner.
    radius: f32,
    grabbable: bool,
    free_on_grab: bool,
    /// The pose the pieces' planes were built for.
    planes_pose: Pose,
    /// Broken: gone from the world, though its id stays taken.
    broken: bool,
    /// What it is made of.
    surface: String,
    /// Where it was added, for bringing it back.
    home: Pose,
}

#[derive(Debug, Clone)]
enum BodyKind {
    Prop(PropBody),
    Player { hull: Hull, velocity: Vec3 },
}

#[derive(Debug, Clone)]
struct Body {
    handle: RigidBodyHandle,
    collider: ColliderHandle,
    kind: BodyKind,
}

/// The map's rigid-body world. See the [module docs](self).
pub struct PhysicsWorld {
    rapier: RapierWorld,
    bodies: Vec<Body>,
    /// `(player, prop)` pairs where the player holds the prop: they do not
    /// collide.
    held: Vec<(BodyId, BodyId)>,
    /// `(player, prop)` pairs where the player let go of the prop while
    /// they still overlapped: they keep ignoring each other until they come
    /// apart, so a prop thrown from inside its holder's box neither shoves
    /// nor hits them.
    released: Vec<(BodyId, BodyId)>,
    /// `(player, prop)` pairs in contact after the last step, so an impact
    /// is counted once when contact begins.
    touching: Vec<(BodyId, BodyId)>,
    /// The map's movers' kinematic bodies.
    movers: Vec<statics::MoverBody>,
    /// The fixed body joints hold props to the world by, once one does.
    anchor: Option<RigidBodyHandle>,
    /// Props that struck props or the map during the last step.
    contacts: Vec<ContactImpact>,
    config: PhysicsConfig,
}

impl std::fmt::Debug for PhysicsWorld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicsWorld")
            .field("bodies", &self.bodies.len())
            .field("colliders", &self.rapier.colliders.len())
            .finish_non_exhaustive()
    }
}

impl PhysicsWorld {
    /// A world holding the static geometry of `map`, stepping `dt` seconds
    /// at a time.
    #[must_use]
    pub fn new(map: &CollisionWorld, config: &PhysicsConfig, dt: f32) -> Self {
        let mut rapier = RapierWorld::new();
        rapier.gravity = Vec3::new(0.0, 0.0, -config.gravity);
        let parameters = &mut rapier.integration_parameters;
        parameters.dt = dt;
        parameters.length_unit = statics::UNITS_PER_METRE;
        parameters.num_solver_iterations = config.solver_iterations as usize;
        parameters.num_internal_pgs_iterations = config.pgs_iterations as usize;
        parameters.max_ccd_substeps = config.ccd_substeps as usize;
        parameters.normalized_max_corrective_velocity =
            config.max_corrective_speed / statics::UNITS_PER_METRE;
        let colliders = statics::add_map(&mut rapier, map, interaction(GROUP_WORLD));
        let movers = statics::add_movers(&mut rapier, map, interaction(GROUP_WORLD));
        tracing::debug!(
            colliders,
            movers = movers.len(),
            "built the physics world's static geometry"
        );
        Self {
            rapier,
            bodies: Vec::new(),
            held: Vec::new(),
            released: Vec::new(),
            touching: Vec::new(),
            movers,
            anchor: None,
            contacts: Vec::new(),
            config: config.clone(),
        }
    }

    /// Moves the body of the mover of BSP model `model` to `origin` and
    /// `rotation` by the next step, pushing the props in its way, and
    /// makes it solid or passable. Unknown models are ignored.
    pub fn set_mover(&mut self, model: usize, origin: Vec3, rotation: Quat, solid: bool) {
        let Some(mover) = self.movers.iter().find(|mover| mover.model == model) else {
            return;
        };
        if let Some(rigid) = self.rapier.bodies.get_mut(mover.handle) {
            rigid.set_next_kinematic_position(Pose::from_parts(origin, rotation));
        }
        if let Some(collider) = mover
            .collider
            .and_then(|collider| self.rapier.colliders.get_mut(collider))
            && collider.is_enabled() != solid
        {
            collider.set_enabled(solid);
        }
    }

    /// Adds a prop.
    pub fn add_prop(&mut self, desc: &PropDesc) -> BodyId {
        let id = self.next_id();
        let builder = match desc.motion {
            Motion::Dynamic => RigidBodyBuilder::dynamic(),
            Motion::Fixed => RigidBodyBuilder::fixed(),
        };
        let handle = self.rapier.insert_body(
            builder
                .pose(Pose::from_parts(desc.origin, desc.rotation))
                .linear_damping(desc.linear_damping)
                .angular_damping(desc.angular_damping)
                .sleeping(desc.start_asleep)
                .user_data(u128::from(id.0)),
        );

        let group = if desc.debris {
            GROUP_DEBRIS
        } else {
            GROUP_PROP
        };
        let shapes: Vec<(Pose, SharedShape)> = desc
            .pieces
            .iter()
            .filter_map(|piece| SharedShape::convex_hull(piece.vertices()))
            .map(|shape| (Pose::IDENTITY, shape))
            .collect();
        let shape = if shapes.len() == 1 {
            shapes.into_iter().next().map(|(_, shape)| shape)
        } else {
            (!shapes.is_empty()).then(|| SharedShape::compound(shapes))
        };
        // A hull too degenerate for rapier still gets a sliver of a box, so
        // the body has mass and a collider handle.
        let shape = shape.unwrap_or_else(|| SharedShape::cuboid(0.5, 0.5, 0.5));
        let collider = self.rapier.insert_collider(
            ColliderBuilder::new(shape)
                .mass(desc.mass)
                .friction(desc.material.friction)
                .restitution(desc.material.restitution)
                .collision_groups(interaction(group))
                .active_events(ActiveEvents::COLLISION_EVENTS)
                .user_data(u128::from(id.0)),
            Some(handle),
        );

        let pose = Pose::from_parts(desc.origin, desc.rotation);
        let pieces = desc
            .pieces
            .iter()
            .map(|hull| piece_at(hull.clone(), &pose))
            .collect();
        let center = self.rapier.bodies[handle].center_of_mass();
        let radius = desc
            .pieces
            .iter()
            .flat_map(ConvexHull::vertices)
            .map(|&vertex| pose.transform_point(vertex).distance(center))
            .fold(0.0, f32::max);
        self.bodies.push(Body {
            handle,
            collider,
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
        let handle = self.rapier.insert_body(
            RigidBodyBuilder::kinematic_position_based()
                .translation(origin)
                .user_data(u128::from(id.0)),
        );
        let collider = self.rapier.insert_collider(
            player_collider(hull).user_data(u128::from(id.0)),
            Some(handle),
        );
        self.bodies.push(Body {
            handle,
            collider,
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
        if current != hull {
            *current = *hull;
            if let Some(collider) = self.rapier.colliders.get_mut(body.collider) {
                collider.set_shape(SharedShape::cuboid(
                    hull.extents().x,
                    hull.extents().y,
                    hull.extents().z,
                ));
                collider.set_translation_wrt_parent(hull.center());
            }
        }
        if let Some(rigid) = self.rapier.bodies.get_mut(body.handle) {
            rigid.set_next_kinematic_translation(origin);
        }
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
        if let Some(collider) = self.rapier.colliders.get_mut(body.collider) {
            collider.set_enabled(enabled);
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
        let (handle, collider) = (body.handle, body.collider);
        if let Some(collider) = self.rapier.colliders.get_mut(collider) {
            collider.set_enabled(enabled);
        }
        if let Some(rigid) = self.rapier.bodies.get_mut(handle) {
            rigid.set_enabled(enabled);
            if enabled {
                rigid.set_gravity_scale(1.0, true);
            }
        }
        if !enabled {
            self.held.retain(|&(_, held)| held != prop);
            self.released.retain(|&(_, released)| released != prop);
            self.touching.retain(|&(_, touched)| touched != prop);
        }
    }

    /// Whether a prop is in the world (see [`PhysicsWorld::set_prop_enabled`]).
    #[must_use]
    pub fn prop_enabled(&self, prop: BodyId) -> bool {
        let enabled = match self.rigid(prop) {
            Some(rigid) => rigid.is_enabled(),
            None => false,
        };
        self.prop_any(prop).is_some() && enabled
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
        let (handle, collider, home) = (body.handle, body.collider, prop_body.home);
        self.held.retain(|&(_, held)| held != prop);
        self.released.retain(|&(_, released)| released != prop);
        self.touching.retain(|&(_, touched)| touched != prop);
        if let Some(rigid) = self.rapier.bodies.get_mut(handle) {
            rigid.set_enabled(true);
            rigid.set_position(home, true);
            rigid.set_linvel(Vec3::ZERO, true);
            rigid.set_angvel(Vec3::ZERO, true);
            rigid.set_gravity_scale(1.0, true);
        }
        if let Some(collider) = self.rapier.colliders.get_mut(collider) {
            collider.set_enabled(true);
        }
        self.refresh_planes();
    }

    /// Advances the world by one tick and returns the impacts of props on
    /// players that began during it.
    pub fn step(&mut self) -> Vec<Impact> {
        let before = self.prop_velocities();
        self.sweep_fast_props(&before);
        let hooks = contacts::HeldFilter {
            pairs: self
                .held
                .iter()
                .chain(&self.released)
                .filter_map(|&(player, prop)| Some((self.handle(player)?, self.handle(prop)?)))
                .collect(),
        };
        let collector = impacts::Collector::default();
        self.rapier.step_with_events(&hooks, &collector);
        self.contacts = self.contact_impacts(
            &collector,
            &before,
            self.config.breakables.impact.report_speed,
        );
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
        let rigid = self.rigid(body)?;
        Some(BodyPose {
            position: rigid.translation(),
            rotation: *rigid.rotation(),
        })
    }

    /// A body's centre of mass, Source space.
    #[must_use]
    pub fn center_of_mass(&self, body: BodyId) -> Option<Vec3> {
        self.rigid(body)
            .map(rapier3d::prelude::RigidBody::center_of_mass)
    }

    /// A body's linear and angular (radians/s) velocity.
    #[must_use]
    pub fn velocity(&self, body: BodyId) -> Option<(Vec3, Vec3)> {
        self.rigid(body)
            .map(|rigid| (rigid.linvel(), rigid.angvel()))
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
        self.rigid(body)
            .is_some_and(rapier3d::prelude::RigidBody::is_dynamic)
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
        self.rigid(body)
            .is_some_and(rapier3d::prelude::RigidBody::is_sleeping)
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
        self.held.retain(|&(_, held)| held != body);
        self.released.retain(|&(_, released)| released != body);
        self.touching.retain(|&(_, touched)| touched != body);
        if let Some(rigid) = self.rigid_mut(body) {
            rigid.set_enabled(false);
        }
        true
    }

    /// Where a prop was added: its home.
    #[must_use]
    pub fn home_position(&self, body: BodyId) -> Option<Vec3> {
        self.prop_any(body).map(|prop| prop.home.translation)
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
        if let Some(rigid) = self.rigid_mut(body) {
            rigid.set_enabled(true);
            rigid.set_position(home, true);
            rigid.set_linvel(Vec3::ZERO, true);
            rigid.set_angvel(Vec3::ZERO, true);
        }
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
        if let Some(rigid) = self.rigid_mut(body) {
            rigid.set_position(Pose::from_parts(pose.position, pose.rotation), false);
        }
    }

    /// Brings collision queries up to date after props were placed with
    /// [`PhysicsWorld::set_pose`], without advancing time.
    pub fn refresh(&mut self) {
        self.rapier.detect_collisions(&(), &());
        self.refresh_planes();
    }

    /// Sets a dynamic body's velocities (angular in radians/s), waking it.
    pub fn set_velocity(&mut self, body: BodyId, linear: Vec3, angular: Vec3) {
        if let Some(rigid) = self.rigid_mut(body)
            && rigid.is_dynamic()
        {
            rigid.set_linvel(linear, true);
            rigid.set_angvel(angular, true);
        }
    }

    /// Applies `impulse` (kg·units/s) at `point`, waking the body.
    pub fn apply_impulse(&mut self, body: BodyId, impulse: Vec3, point: Vec3) {
        if let Some(rigid) = self.rigid_mut(body)
            && rigid.is_dynamic()
        {
            rigid.apply_impulse_at_point(impulse, point, true);
        }
    }

    /// Marks `prop` as held by `player` (or released, with `None`): a held
    /// prop ignores gravity and does not collide with its holder. A fixed
    /// prop that may be freed by grabbing becomes dynamic.
    pub fn set_held(&mut self, prop: BodyId, player: Option<BodyId>) {
        let mut released = Vec::new();
        self.held.retain(|&pair| {
            let keep = pair.1 != prop;
            if !keep {
                released.push(pair);
            }
            keep
        });
        self.released.retain(|&(_, other)| other != prop);
        if player.is_none() {
            self.released.extend(released);
        }
        let free = self.prop(prop).is_some_and(|body| body.free_on_grab);
        if let Some(player) = player {
            self.held.push((player, prop));
        }
        if let Some(rigid) = self.rigid_mut(prop) {
            if player.is_some() && free && !rigid.is_dynamic() {
                rigid.set_body_type(RigidBodyType::Dynamic, true);
            }
            rigid.set_gravity_scale(if player.is_some() { 0.0 } else { 1.0 }, true);
        }
    }

    /// Whether `player` and `prop` ignore each other: the player holds the
    /// prop, or let go of it and has not come apart from it yet.
    #[must_use]
    pub fn ignores(&self, player: BodyId, prop: BodyId) -> bool {
        self.held.contains(&(player, prop)) || self.released.contains(&(player, prop))
    }

    /// Stops ignoring released pairs whose bounds no longer overlap.
    fn forget_separated(&mut self) {
        let bounds = |body: BodyId| {
            let collider = self.bodies.get(body.index())?.collider;
            Some(self.rapier.colliders.get(collider)?.compute_aabb())
        };
        let apart: Vec<_> = self
            .released
            .iter()
            .copied()
            .filter(|&(player, prop)| match (bounds(player), bounds(prop)) {
                (Some(a), Some(b)) => !a.intersects(&b),
                _ => true,
            })
            .collect();
        self.released.retain(|pair| !apart.contains(pair));
    }

    /// The player holding `prop`, if any.
    #[must_use]
    pub fn holder(&self, prop: BodyId) -> Option<BodyId> {
        self.held
            .iter()
            .find(|&&(_, held)| held == prop)
            .map(|&(player, _)| player)
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

    fn next_id(&self) -> BodyId {
        BodyId(u32::try_from(self.bodies.len()).unwrap_or(u32::MAX))
    }

    fn handle(&self, body: BodyId) -> Option<RigidBodyHandle> {
        self.bodies.get(body.index()).map(|body| body.handle)
    }

    fn rigid(&self, body: BodyId) -> Option<&rapier3d::prelude::RigidBody> {
        self.rapier.bodies.get(self.handle(body)?)
    }

    fn rigid_mut(&mut self, body: BodyId) -> Option<&mut rapier3d::prelude::RigidBody> {
        let handle = self.handle(body)?;
        self.rapier.bodies.get_mut(handle)
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
        self.props()
            .filter_map(|id| {
                let rigid = self.rigid(id)?;
                (rigid.is_dynamic() && !rigid.is_sleeping()).then(|| (id, rigid.linvel()))
            })
            .collect()
    }

    /// Makes the props moving faster than `ccd_min_speed` bullets for this
    /// step: rapier sweeps those against the other props in their way, which
    /// stops a hard throw from passing through a crate, while slow props
    /// (almost all of them, almost all of the time) cost nothing extra.
    /// Rapier never sweeps one bullet against another, which is why
    /// bullets are not made of every prop.
    fn sweep_fast_props(&mut self, velocities: &[(BodyId, Vec3)]) {
        let threshold = self.config.ccd_min_speed;
        for &(id, velocity) in velocities {
            let fast = velocity.length() > threshold;
            if let Some(rigid) = self.rigid_mut(id)
                && rigid.is_ccd_enabled() != fast
            {
                rigid.enable_ccd(fast);
            }
        }
    }

    /// Enforces the configured speed limits.
    fn clamp_velocities(&mut self) {
        let max_speed = self.config.max_speed;
        let max_spin = self.config.max_angular_speed.to_radians();
        for body in &self.bodies {
            let Some(rigid) = self.rapier.bodies.get_mut(body.handle) else {
                continue;
            };
            if !rigid.is_dynamic() || rigid.is_sleeping() {
                continue;
            }
            let linear = rigid.linvel();
            if linear.length() > max_speed {
                rigid.set_linvel(linear.clamp_length_max(max_speed), false);
            }
            let angular = rigid.angvel();
            if angular.length() > max_spin {
                rigid.set_angvel(angular.clamp_length_max(max_spin), false);
            }
        }
    }

    /// Rebuilds the clip planes of every prop that moved.
    fn refresh_planes(&mut self) {
        for body in &mut self.bodies {
            let BodyKind::Prop(prop) = &mut body.kind else {
                continue;
            };
            let Some(rigid) = self.rapier.bodies.get(body.handle) else {
                continue;
            };
            let pose = *rigid.position();
            if !moved(&prop.planes_pose, &pose) {
                continue;
            }
            for piece in &mut prop.pieces {
                (piece.mins, piece.maxs) =
                    piece
                        .hull
                        .world_planes(pose.rotation, pose.translation, &mut piece.planes);
            }
            prop.planes_pose = pose;
        }
    }
}

/// A piece with its planes built for `pose`.
fn piece_at(hull: ConvexHull, pose: &Pose) -> Piece {
    let mut planes = Vec::new();
    let (mins, maxs) = hull.world_planes(pose.rotation, pose.translation, &mut planes);
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
fn moved(a: &Pose, b: &Pose) -> bool {
    a.translation.distance_squared(b.translation) > MOVED_EPSILON || a.rotation != b.rotation
}

/// The interaction groups of a collider in `group`: who it collides with.
fn interaction(group: Group) -> InteractionGroups {
    let filter = if group == GROUP_WORLD {
        GROUP_PROP | GROUP_DEBRIS
    } else if group == GROUP_PROP {
        GROUP_WORLD | GROUP_PROP | GROUP_DEBRIS | GROUP_PLAYER
    } else if group == GROUP_DEBRIS {
        GROUP_WORLD | GROUP_PROP
    } else {
        GROUP_PROP
    };
    InteractionGroups::new(group, filter, InteractionTestMode::And)
}

/// A player's kinematic box.
fn player_collider(hull: &Hull) -> ColliderBuilder {
    let extents = hull.extents();
    ColliderBuilder::cuboid(extents.x, extents.y, extents.z)
        .translation(hull.center())
        .collision_groups(interaction(GROUP_PLAYER))
        .active_hooks(ActiveHooks::FILTER_CONTACT_PAIRS)
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
        let pose = Pose::from_parts(Vec3::new(10.0, -4.0, 2.0), rotation);
        assert!(!moved(&pose, &pose));
    }

    #[test]
    fn moving_or_turning_at_all_is_moving() {
        let pose = Pose::from_parts(Vec3::ZERO, Quat::IDENTITY);
        let slid = Pose::from_parts(Vec3::new(0.01, 0.0, 0.0), Quat::IDENTITY);
        let turned = Pose::from_parts(Vec3::ZERO, Quat::from_rotation_z(1e-3));
        let crept = Pose::from_parts(Vec3::new(1e-5, 0.0, 0.0), Quat::IDENTITY);
        assert!(moved(&pose, &slid));
        assert!(moved(&pose, &turned));
        assert!(!moved(&pose, &crept));
    }
}
