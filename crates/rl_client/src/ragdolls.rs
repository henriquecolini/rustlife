//! Player death ragdolls: the body of a player who died, going limp and
//! falling where they stood.
//!
//! A ragdoll reuses the player's model root and skeleton. When its player is
//! killed, the root stops being a [`PlayerView`](crate::actors::PlayerView)
//! (the server draws a fresh, hidden model for the dead player and shows it
//! again on respawn) and becomes a small rigid-body body: one bone per
//! particle is simulated by Box3D, with capsule colliders along the bones,
//! mass from the bone volume, angular limits at every joint and self
//! collision between bones that are not directly joined. The joints' turns
//! come from the rigid bodies, so the skin bends with the skeleton instead of
//! stretching, and the last animation pose is where the body starts.
//!
//! It is visual only: the server does not know or care where a corpse lies.
//! Corpses are removed after [`RAGDOLL_LIFE`] seconds, and at most
//! [`MAX_RAGDOLLS`] are kept, oldest first.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use b3::body::{
    body_apply_linear_impulse, body_apply_linear_impulse_to_center, body_get_mass,
    body_get_transform, body_get_world_center, create_body,
};
use b3::geometry::{Capsule, Sphere};
use b3::math_functions::Vec3 as B3Vec3;
use b3::shape::{create_capsule_shape, create_hull_shape, create_mesh_shape, create_sphere_shape};
use b3::types::BodyType;
use b3::world::World;
use bevy::animation::AnimationPlayer;
use bevy::prelude::*;
use box3d_rust as b3;
use rl_assets::coords;
use rl_data::Shot;
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::physics::{UNITS_PER_METRE, use_source_units};
use rl_sim::simulation::{PlayerId, ShotEvent, Simulation, TickEvents};

use crate::AppState;
use crate::actors::PlayerView;
use crate::animation::{Animated, SequencePlayer};
use crate::net::Game;
use crate::player::Ticked;

/// How long a corpse lies before it is removed, seconds.
const RAGDOLL_LIFE: f32 = 25.0;
/// Most corpses alive at once.
const MAX_RAGDOLLS: usize = 8;
/// Downward acceleration, Source units/s²: the same gravity as movement.
const GRAVITY: f32 = -9.81 * UNITS_PER_METRE;
/// Radius of the smallest bone collider, Source units.
const MIN_BONE_RADIUS: f32 = 0.03 * UNITS_PER_METRE;
/// Radius of the largest bone collider, Source units.
const MAX_BONE_RADIUS: f32 = 0.075 * UNITS_PER_METRE;
/// Seconds of physics per fixed step.
const STEP: f32 = 1.0 / 120.0;
/// Most steps one frame may run, so a hitch does not turn into a time bomb.
const MAX_STEPS: usize = 4;
/// How much of a killing hitscan's prop impulse reaches the ragdoll.
const SHOT_IMPULSE_SCALE: f32 = 0.6;
/// Most impulse one bullet may put into a body, kg·Source-units/s.
const MAX_SHOT_IMPULSE: f32 = 60.0 * UNITS_PER_METRE;
/// Extra share of a bullet's impulse applied at the body's root, so the
/// whole corpse is shoved and not just the struck limb.
const SHOT_BODY_SHOVE: f32 = 0.65;
/// How far from a shot a corpse bone is still pushed, Source units.
const CORPSE_SHOT_RADIUS: f32 = 0.28 * UNITS_PER_METRE;
/// How fast a blast throws a body at its centre, Source units/s.
const BLAST_SPEED: f32 = 11.0 * UNITS_PER_METRE;
/// How long a shot that hit a player is remembered for their death, seconds.
/// The fatal shot may be predicted locally while the kill arrives from the
/// server a little later.
const HIT_MEMORY: f32 = 1.0;
/// Density of a body in Box3D's (Source-unit) space, kg/unit³: human flesh
/// is close enough to water.
const BONE_DENSITY: f32 = 1000.0 / (UNITS_PER_METRE * UNITS_PER_METRE * UNITS_PER_METRE);
/// User data of the map's static body in the ragdoll world.
const MAP_BODY: u64 = u64::MAX;

/// Plays player deaths as ragdolls.
pub struct RagdollsPlugin;

impl Plugin for RagdollsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RagdollPhysics>()
            .init_resource::<RecentHits>()
            .add_systems(OnExit(AppState::InGame), reset)
            .add_systems(
                Update,
                (spawn_ragdolls, simulate_ragdolls)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The shared Box3D world all corpses are simulated in.
#[derive(Resource, Default)]
struct RagdollPhysics {
    world: Option<Mutex<World>>,
}

impl RagdollPhysics {
    /// The world, built from the map's collision when first needed.
    fn world(&mut self, sim: &Simulation) -> &mut World {
        self.world
            .get_or_insert_with(|| Mutex::new(build_world(sim)))
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The world, if one has been built.
    fn current(&mut self) -> Option<&mut World> {
        self.world
            .as_mut()
            .map(|world| world.get_mut().unwrap_or_else(PoisonError::into_inner))
    }
}

fn reset(mut physics: ResMut<RagdollPhysics>, mut hits: ResMut<RecentHits>) {
    *physics = RagdollPhysics::default();
    *hits = RecentHits::default();
}

/// Shots that recently struck players, so the corpse can be pushed by the
/// fatal one even when it was predicted locally before the server's kill.
#[derive(Resource, Default)]
struct RecentHits {
    shots: Vec<(PlayerId, ShotEvent, f32)>,
}

/// Builds the static map colliders for the ragdoll world.
fn build_world(sim: &Simulation) -> World {
    use_source_units();
    let mut def = b3::default_world_def();
    def.gravity = b3::Vec3 {
        x: 0.0,
        y: 0.0,
        z: GRAVITY,
    };
    let mut world = World::new(&def);
    b3::world::world_set_gravity(&mut world, def.gravity);

    let mut body_def = b3::default_body_def();
    body_def.type_ = BodyType::Static;
    body_def.user_data = MAP_BODY;
    let body = create_body(&mut world, &body_def);

    let mut shape_def = b3::default_shape_def();
    shape_def.density = 0.0;
    shape_def.update_body_mass = false;
    shape_def.base_material.friction = 0.8;
    shape_def.base_material.restitution = 0.05;

    for solid in sim.world().convex_solids(MASK_PLAYER_SOLID) {
        let points: Vec<B3Vec3> = solid.iter().map(|point| b3v(Vec3::from(*point))).collect();
        if let Some(hull) = b3::hull::create_hull(&points, b3::constants::MAX_HULL_VERTICES) {
            create_hull_shape(&mut world, body, &shape_def, &hull);
        }
    }

    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for triangles in sim.world().displacement_triangles(MASK_PLAYER_SOLID) {
        for triangle in triangles {
            let first = i32::try_from(vertices.len()).unwrap_or(i32::MAX);
            for point in triangle {
                vertices.push(b3v(Vec3::from(*point)));
            }
            indices.extend([first, first + 1, first + 2]);
        }
    }
    if !vertices.is_empty() {
        let mesh_def = b3::mesh::MeshDef {
            vertices,
            indices,
            ..b3::mesh::MeshDef::default()
        };
        if let Some(mesh) = b3::mesh::create_mesh(&mesh_def, None) {
            shape_def.base_material.restitution = 0.02;
            create_mesh_shape(&mut world, body, &shape_def, &mesh, b3::VEC3_ONE);
        }
    }
    world
}

/// A corpse: the rigid body of each joint and the entities they pose.
#[derive(Component)]
pub(crate) struct Ragdoll {
    /// The player whose body this is.
    pub(crate) player: PlayerId,
    joints: Vec<Entity>,
    parents: Vec<Option<usize>>,
    bodies: Vec<b3::BodyId>,
    /// World position of the model root; joints hang in world space around
    /// it.
    origin: Vec3,
    pub(crate) born: f32,
}

/// Spawns a corpse for every player killed in the ticks read this frame.
#[allow(clippy::too_many_arguments)]
fn spawn_ragdolls(
    mut commands: Commands,
    mut ticks: MessageReader<Ticked>,
    time: Res<Time>,
    game: Game,
    mut physics: ResMut<RagdollPhysics>,
    mut hits: ResMut<RecentHits>,
    roots: Query<(Entity, &PlayerView, &Animated, &GlobalTransform)>,
    existing: Query<(Entity, &Ragdoll)>,
    children: Query<&Children>,
    names: Query<&Name>,
    globals: Query<&GlobalTransform>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    for Ticked(events) in ticks.read() {
        let now = time.elapsed_secs();
        let current: Vec<(PlayerId, ShotEvent)> = events
            .shots
            .iter()
            .filter_map(|shot| match shot.trace.struck {
                rl_sim::combat::hitscan::Struck::Player(victim) => Some((victim, *shot)),
                _ => None,
            })
            .collect();
        if events.kills.is_empty() {
            hits.shots
                .extend(current.iter().map(|&(victim, shot)| (victim, shot, now)));
            hits.shots.retain(|(_, _, at)| now - *at <= HIT_MEMORY);
            continue;
        }
        let world = physics.world(sim);
        // The oldest corpses make room for new ones.
        let mut corpses: Vec<(f32, Entity, Vec<b3::BodyId>)> = existing
            .iter()
            .map(|(entity, ragdoll)| (ragdoll.born, entity, ragdoll.bodies.clone()))
            .collect();
        corpses.sort_by(|a, b| a.0.total_cmp(&b.0));
        while corpses.len() >= MAX_RAGDOLLS {
            let (_, entity, bodies) = corpses.remove(0);
            for body in bodies {
                if b3::body::body_is_valid(world, body) {
                    b3::body::destroy_body(world, body);
                }
            }
            commands.entity(entity).try_despawn();
        }

        for kill in &events.kills {
            let Some((entity, _view, animated, root)) =
                roots.iter().find(|(_, view, ..)| view.0 == kill.victim)
            else {
                continue;
            };
            let skeleton = &animated.animation.skeleton;
            let Some(joints) = collect_joints(entity, skeleton, &children, &names) else {
                continue;
            };
            let Some(positions) = capture_positions(&joints, &globals) else {
                continue;
            };
            let rotations = capture_rotations(&joints, &globals);
            let origin = root.translation();
            let parents: Vec<Option<usize>> = skeleton.joints.iter().map(|j| j.parent).collect();
            let mut shots: Vec<ShotEvent> = hits
                .shots
                .iter()
                .filter(|(victim, ..)| *victim == kill.victim)
                .map(|(_, shot, _)| *shot)
                .collect();
            shots.extend(
                current
                    .iter()
                    .filter(|(victim, _)| *victim == kill.victim)
                    .map(|(_, shot)| *shot),
            );
            let bodies = add_bodies(
                world,
                skeleton,
                &positions,
                &rotations,
                &parents,
                sim,
                &shots,
                events,
                kill.victim,
            );
            if bodies.is_empty() {
                continue;
            }

            commands
                .entity(entity)
                .remove::<(PlayerView, Animated, SequencePlayer, AnimationPlayer)>()
                .insert((
                    Ragdoll {
                        player: kill.victim,
                        joints,
                        parents,
                        bodies,
                        origin,
                        born: time.elapsed_secs(),
                    },
                    Visibility::Inherited,
                ));
        }
        hits.shots
            .extend(current.iter().map(|&(victim, shot)| (victim, shot, now)));
        hits.shots.retain(|(_, _, at)| now - *at <= HIT_MEMORY);
    }
}

/// Inserts each joint's rigid body and its bone capsule, joins the bones,
/// and applies the death blow where it landed.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn add_bodies(
    world: &mut World,
    skeleton: &rl_assets::Skeleton,
    positions: &[Vec3],
    rotations: &[Quat],
    parents: &[Option<usize>],
    sim: &Simulation,
    shots: &[ShotEvent],
    events: &TickEvents,
    victim: PlayerId,
) -> Vec<b3::BodyId> {
    // The renderer and animation live in metres; Box3D's world is in
    // Source units, like the server's physics.
    let positions: Vec<Vec3> = positions
        .iter()
        .map(|position| *position * UNITS_PER_METRE)
        .collect();
    let mut bodies = Vec::with_capacity(positions.len());
    let mut shape_def = b3::default_shape_def();
    shape_def.density = BONE_DENSITY;
    shape_def.base_material.friction = 0.8;
    shape_def.base_material.restitution = 0.02;
    for (index, (&position, &rotation)) in positions.iter().zip(rotations).enumerate() {
        let mut body_def = b3::default_body_def();
        body_def.type_ = BodyType::Dynamic;
        body_def.position = b3v(position);
        body_def.rotation = b3q(rotation);
        body_def.linear_damping = 0.15;
        body_def.angular_damping = 0.65;
        body_def.user_data = u64::try_from(index).unwrap_or(u64::MAX);
        let handle = create_body(world, &body_def);
        bodies.push(handle);
        let child = skeleton
            .joints
            .iter()
            .enumerate()
            .find_map(|(child, joint)| (joint.parent == Some(index)).then_some(child));
        let length = child.map(|child| positions[child].distance(position));
        if let (Some(child), Some(length)) = (
            child,
            length.filter(|length| *length > 0.02 * UNITS_PER_METRE),
        ) {
            let radius = (length * 0.20).clamp(MIN_BONE_RADIUS, MAX_BONE_RADIUS);
            let half_height = (length * 0.5 - radius).max(0.005 * UNITS_PER_METRE);
            let direction = (positions[child] - position).normalize_or(Vec3::Y);
            let center = direction * (length * 0.5);
            let capsule = Capsule {
                center1: b3v(center - direction * half_height),
                center2: b3v(center + direction * half_height),
                radius,
            };
            create_capsule_shape(world, handle, &shape_def, &capsule);
        } else {
            create_sphere_shape(
                world,
                handle,
                &shape_def,
                &Sphere {
                    center: B3Vec3::default(),
                    radius: MIN_BONE_RADIUS * 1.4,
                },
            );
        }
    }
    let root = parents.iter().position(Option::is_none).unwrap_or(0);
    for (child, parent) in parents.iter().enumerate() {
        let Some(parent) = *parent else {
            continue;
        };
        let joint_rotation = rotations[child];
        let frame1 = b3::Transform {
            p: b3v(rotations[parent]
                .inverse()
                .mul_vec3(positions[child] - positions[parent])),
            q: b3q(rotations[parent].inverse() * joint_rotation),
        };
        let frame2 = b3::Transform {
            p: B3Vec3::default(),
            q: b3::QUAT_IDENTITY,
        };
        let bone = skeleton.joints[child].name.to_ascii_lowercase();
        let limit = joint_limit(&bone);
        let (hertz, damping) = if bone.contains("spine")
            || bone.contains("pelvis")
            || bone.contains("chest")
            || bone.contains("head")
            || bone.contains("neck")
        {
            (2.5, 1.5)
        } else {
            (1.2, 1.0)
        };
        let mut def = b3::default_spherical_joint_def();
        def.base.body_id_a = bodies[parent];
        def.base.body_id_b = bodies[child];
        def.base.local_frame_a = frame1;
        def.base.local_frame_b = frame2;
        def.base.collide_connected = false;
        def.enable_cone_limit = true;
        def.cone_angle = limit.clamp(0.0, std::f32::consts::FRAC_PI_2 * 0.99);
        def.enable_twist_limit = true;
        def.lower_twist_angle = -limit.clamp(0.0, std::f32::consts::PI * 0.99);
        def.upper_twist_angle = limit.clamp(0.0, std::f32::consts::PI * 0.99);
        def.enable_spring = true;
        def.hertz = hertz;
        def.damping_ratio = damping;
        b3::joint::create_spherical_joint(world, &def);
    }

    // Fatal hits push the bone they struck, at the point they struck it.
    for shot in shots {
        if shot.trace.struck != rl_sim::combat::hitscan::Struck::Player(victim) {
            continue;
        }
        let point = Vec3::from(shot.trace.end);
        let direction = (Vec3::from(shot.trace.end) - Vec3::from(shot.start)).normalize_or(Vec3::Y);
        let force = shot_force(sim, shot.weapon);
        let reach = 0.45 * UNITS_PER_METRE;
        let mut weighted: Vec<(usize, f32)> = positions
            .iter()
            .enumerate()
            .filter_map(|(index, position)| {
                let distance = position.distance(point);
                (distance < reach).then(|| {
                    let weight = (1.0 - distance / reach).powi(2);
                    (index, weight)
                })
            })
            .collect();
        if weighted.is_empty()
            && let Some(index) = nearest_body(&positions, point)
        {
            weighted.push((index, 1.0));
        }
        let total: f32 = weighted.iter().map(|(_, weight)| weight).sum();
        if total <= 0.0 {
            continue;
        }
        for (index, weight) in weighted {
            let impulse = direction * force * weight / total;
            if b3::body::body_is_valid(world, bodies[index]) {
                body_apply_linear_impulse(world, bodies[index], b3v(impulse), b3v(point), true);
            }
        }
        if let Some(root) = bodies.get(root)
            && b3::body::body_is_valid(world, *root)
        {
            body_apply_linear_impulse_to_center(
                world,
                *root,
                b3v(direction * force * SHOT_BODY_SHOVE),
                true,
            );
        }
    }
    // A blast throws every part of a body near it outwards.
    for blast in &events.explosions {
        let center = Vec3::from(blast.position);
        let radius = (blast.radius * UNITS_PER_METRE).max(0.1);
        for handle in &bodies {
            if !b3::body::body_is_valid(world, *handle) {
                continue;
            }
            let position = b3v_to_glam(body_get_world_center(world, *handle));
            let offset = position - center;
            let distance = offset.length();
            if distance >= radius {
                continue;
            }
            let falloff = (1.0 - distance / radius).clamp(0.0, 1.0).powi(2);
            let direction = (offset.normalize_or_zero() + Vec3::Y * 0.35).normalize_or(Vec3::Y);
            let mass = body_get_mass(world, *handle).max(1.0);
            let impulse = direction * mass * BLAST_SPEED * falloff;
            body_apply_linear_impulse_to_center(world, *handle, b3v(impulse), true);
        }
    }
    bodies
}

/// A hitscan or melee weapon's impulse, converted to the ragdoll scale.
fn shot_force(sim: &Simulation, weapon: rl_sim::weapons::WeaponId) -> f32 {
    let force = sim
        .arsenal()
        .weapon(weapon)
        .and_then(|weapon| {
            weapon.fire_modes().find_map(|fire| match fire.shot {
                Shot::Hitscan(ref hitscan) => Some(hitscan.force),
                Shot::Melee(ref melee) => Some(melee.force),
                _ => None,
            })
        })
        .unwrap_or(1000.0);
    (force * SHOT_IMPULSE_SCALE).min(MAX_SHOT_IMPULSE)
}

fn nearest_body(positions: &[Vec3], point: Vec3) -> Option<usize> {
    positions
        .iter()
        .enumerate()
        .min_by(|a, b| {
            a.1.distance_squared(point)
                .total_cmp(&b.1.distance_squared(point))
        })
        .map(|(index, _)| index)
}

/// Angular limits for a joint, by the bone it drives. They keep a corpse
/// from folding through itself while still letting it settle naturally.
fn joint_limit(name: &str) -> f32 {
    let name = name.to_ascii_lowercase();
    if name.contains("head") || name.contains("neck") {
        0.32
    } else if name.contains("spine") || name.contains("pelvis") || name.contains("chest") {
        0.16
    } else if name.contains("forearm") || name.contains("calve") || name.contains("calf") {
        0.72
    } else if name.contains("thigh") || name.contains("leg") || name.contains("arm") {
        0.66
    } else if name.contains("hand") || name.contains("foot") || name.contains("clavicle") {
        0.38
    } else {
        0.45
    }
}

/// The entities of `skeleton`'s joints under `root`, in skeleton order.
fn collect_joints(
    root: Entity,
    skeleton: &rl_assets::Skeleton,
    children: &Query<&Children>,
    names: &Query<&Name>,
) -> Option<Vec<Entity>> {
    let mut by_name: HashMap<String, Entity> = HashMap::new();
    let mut stack = vec![root];
    for _ in 0..4096 {
        let Some(entity) = stack.pop() else {
            break;
        };
        if let Ok(name) = names.get(entity) {
            by_name
                .entry(name.as_str().to_ascii_lowercase())
                .or_insert(entity);
        }
        if let Ok(below) = children.get(entity) {
            stack.extend(below.iter());
        }
    }
    skeleton
        .joints
        .iter()
        .map(|joint| by_name.get(&joint.name.to_ascii_lowercase()).copied())
        .collect()
}

/// The joints' world positions, if every joint can be found.
fn capture_positions(joints: &[Entity], globals: &Query<&GlobalTransform>) -> Option<Vec<Vec3>> {
    joints
        .iter()
        .map(|joint| Some(globals.get(*joint).ok()?.translation()))
        .collect()
}

fn capture_rotations(joints: &[Entity], globals: &Query<&GlobalTransform>) -> Vec<Quat> {
    joints
        .iter()
        .map(|joint| {
            globals.get(*joint).map_or(Quat::IDENTITY, |global| {
                global.to_scale_rotation_translation().1
            })
        })
        .collect()
}

/// Advances every corpse, poses its joints, and removes old bodies.
fn simulate_ragdolls(
    mut commands: Commands,
    mut ticks: MessageReader<Ticked>,
    time: Res<Time>,
    game: Game,
    mut physics: ResMut<RagdollPhysics>,
    mut corpses: Query<(Entity, &mut Ragdoll, &mut Transform)>,
    mut joints: Query<&mut Transform, Without<Ragdoll>>,
) {
    if game.sim().is_none() {
        return;
    }
    let now = time.elapsed_secs();
    let Some(world) = physics.current() else {
        return;
    };
    let mut expired = Vec::new();
    for (entity, corpse, _) in &mut corpses {
        if now - corpse.born >= RAGDOLL_LIFE {
            expired.push((entity, corpse.bodies.clone()));
        }
    }
    for (entity, bodies) in expired {
        for body in bodies {
            if b3::body::body_is_valid(world, body) {
                b3::body::destroy_body(world, body);
            }
        }
        commands.entity(entity).try_despawn();
    }
    if corpses.is_empty() {
        return;
    }
    let Some(sim) = game.sim() else {
        return;
    };
    for Ticked(events) in ticks.read() {
        for shot in &events.shots {
            if matches!(
                shot.trace.struck,
                rl_sim::combat::hitscan::Struck::Player(_)
            ) {
                // The fatal shot was already applied when the corpse formed.
                continue;
            }
            let force = shot_force(sim, shot.weapon);
            if force <= 0.0 {
                continue;
            }
            let start = Vec3::from(shot.start);
            let end = Vec3::from(shot.trace.end);
            let direction = (end - start).normalize_or_zero();
            let length = start.distance(end);
            for (_, corpse, _) in &mut corpses {
                let mut best: Option<(usize, f32, Vec3)> = None;
                for (index, handle) in corpse.bodies.iter().enumerate() {
                    if !b3::body::body_is_valid(world, *handle) {
                        continue;
                    }
                    let position = b3v_to_glam(body_get_world_center(world, *handle));
                    let along = (position - start).dot(direction).clamp(0.0, length);
                    let closest = start + direction * along;
                    let distance = position.distance(closest);
                    if distance <= CORPSE_SHOT_RADIUS
                        && best.is_none_or(|(_, best_distance, _)| distance < best_distance)
                    {
                        best = Some((index, distance, closest));
                    }
                }
                if let Some((index, _, closest)) = best
                    && b3::body::body_is_valid(world, corpse.bodies[index])
                {
                    body_apply_linear_impulse(
                        world,
                        corpse.bodies[index],
                        b3v(direction * force),
                        b3v(closest),
                        true,
                    );
                }
                if let Some(root) = corpse.parents.iter().position(Option::is_none)
                    && b3::body::body_is_valid(world, corpse.bodies[root])
                {
                    body_apply_linear_impulse_to_center(
                        world,
                        corpse.bodies[root],
                        b3v(direction * force * SHOT_BODY_SHOVE),
                        true,
                    );
                }
            }
        }
    }
    let dt = time.delta_secs().min(STEP * MAX_STEPS as f32);
    let steps = ((dt / STEP).ceil() as usize).clamp(1, MAX_STEPS);
    for _ in 0..steps {
        world.step(dt / steps as f32, 1);
    }
    for (_, corpse, mut transform) in &mut corpses {
        pose(&corpse, world, &mut joints);
        transform.translation = corpse.origin;
        transform.rotation = Quat::IDENTITY;
        transform.scale = Vec3::ONE;
    }
}

/// Sets every joint's local transform from its rigid body and its parent's.
fn pose(ragdoll: &Ragdoll, world: &World, joints: &mut Query<&mut Transform, Without<Ragdoll>>) {
    let mut rotations: Vec<Quat> = Vec::with_capacity(ragdoll.joints.len());
    for (index, joint) in ragdoll.joints.iter().enumerate() {
        let handle = ragdoll.bodies[index];
        if !b3::body::body_is_valid(world, handle) {
            continue;
        }
        let transform_body = body_get_transform(world, handle);
        let rotation = b3q_to_glam(transform_body.q);
        let position = b3v_to_glam(transform_body.p) * coords::METERS_PER_UNIT;
        let parent = ragdoll.parents[index];
        let parent_rotation = parent
            .and_then(|parent| rotations.get(parent).copied())
            .unwrap_or(Quat::IDENTITY);
        let parent_position = parent
            .and_then(|parent| {
                let handle = ragdoll.bodies[parent];
                b3::body::body_is_valid(world, handle).then(|| {
                    b3v_to_glam(body_get_transform(world, handle).p) * coords::METERS_PER_UNIT
                })
            })
            .unwrap_or(ragdoll.origin);
        let local_rotation = parent_rotation.inverse() * rotation;
        let local_position = parent_rotation
            .inverse()
            .mul_vec3(position - parent_position);
        if let Ok(mut transform) = joints.get_mut(*joint) {
            transform.translation = local_position;
            transform.rotation = local_rotation;
            transform.scale = Vec3::ONE;
        }
        rotations.push(rotation);
    }
}

fn b3v(value: Vec3) -> B3Vec3 {
    B3Vec3 {
        x: value.x,
        y: value.y,
        z: value.z,
    }
}

fn b3v_to_glam(value: B3Vec3) -> Vec3 {
    Vec3::new(value.x, value.y, value.z)
}

fn b3q(value: Quat) -> b3::Quat {
    b3::Quat {
        v: b3v(value.xyz()),
        s: value.w,
    }
}

fn b3q_to_glam(value: b3::Quat) -> Quat {
    Quat::from_xyzw(value.v.x, value.v.y, value.v.z, value.s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_and_spine_turn_far_less_than_limbs() {
        assert!(joint_limit("ValveBiped.Bip01_Head1") < joint_limit("ValveBiped.Bip01_R_Forearm"));
        assert!(joint_limit("ValveBiped.Bip01_Spine") < joint_limit("ValveBiped.Bip01_R_Thigh"));
        assert!(joint_limit("anything") > 0.0);
    }
}
