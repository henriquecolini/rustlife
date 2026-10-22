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
use b3::shape::{create_capsule_shape, create_sphere_shape};
use b3::types::BodyType;
use b3::world::World;
use bevy::animation::AnimationPlayer;
use bevy::prelude::*;
use box3d_rust as b3;
use rl_assets::coords;
use rl_data::Shot;
use rl_sim::physics::UNITS_PER_METRE;
use rl_sim::simulation::{PlayerId, ShotEvent, Simulation, TickEvents};

use crate::AppState;
use crate::actors::PlayerView;
use crate::animation::{Animated, SequencePlayer};
use crate::net::Game;
use crate::player::Ticked;
use crate::visual_world::{RAGDOLL_GROUP, WORLD_GROUP, build_static_world, filter};

/// How long a corpse lies before it is removed, seconds.
const RAGDOLL_LIFE: f32 = 25.0;
/// Most corpses alive at once.
const MAX_RAGDOLLS: usize = 8;
/// Radius of the smallest bone collider, Source units.
const MIN_BONE_RADIUS: f32 = 0.025 * UNITS_PER_METRE;
/// Seconds of physics per fixed step.
const STEP: f32 = 1.0 / 60.0;
/// Most steps one frame may run, so a hitch does not turn into a time bomb.
const MAX_STEPS: usize = 4;
/// Box3D sub-steps per fixed step, as in the engine's ragdoll samples.
const SUB_STEPS: i32 = 4;
/// How much of a killing hitscan's prop impulse reaches the ragdoll.
const SHOT_IMPULSE_SCALE: f32 = 0.25;
/// Most impulse one bullet may put into a body, kg·Source-units/s.
const MAX_SHOT_IMPULSE: f32 = 25.0 * UNITS_PER_METRE;
/// Extra share of a bullet's impulse applied at the body's root, so the
/// whole corpse is shoved and not just the struck limb.
const SHOT_BODY_SHOVE: f32 = 0.35;
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
/// Joint friction torque in Box3D's units: Box3D lengths are Source inches,
/// while the engine samples use N·m, so one N·m is scaled by L².
const JOINT_TORQUE: f32 = 5.0 * UNITS_PER_METRE * UNITS_PER_METRE;

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
    /// Leftover time from the last frame, so physics always advances in
    /// fixed 1/60 s steps.
    accumulator: f32,
    /// A negative collision group unique to the last corpse. All of a
    /// corpse's bones share it, which turns off self-collision (the usual
    /// source of ragdolls exploding) while leaving them colliding with the
    /// world and with other corpses.
    next_group: i32,
}

impl RagdollPhysics {
    /// The world, built from the map's collision when first needed.
    fn world(&mut self, sim: &Simulation) -> &mut World {
        self.world
            .get_or_insert_with(|| Mutex::new(build_static_world(sim, RAGDOLL_GROUP)))
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The world, if one has been built.
    fn current(&mut self) -> Option<&mut World> {
        self.world
            .as_mut()
            .map(|world| world.get_mut().unwrap_or_else(PoisonError::into_inner))
    }

    /// A fresh negative collision group for a corpse.
    fn corpse_group(&mut self) -> i32 {
        self.next_group = self.next_group.saturating_sub(1);
        if self.next_group == 0 {
            self.next_group = -1;
        }
        self.next_group
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
        let group = physics.corpse_group();
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
                group,
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
    group_index: i32,
) -> Vec<b3::BodyId> {
    // The renderer and animation live in metres; Box3D's world is in
    // Source units, like the server's physics.
    let positions: Vec<Vec3> = positions
        .iter()
        .map(|position| *position * UNITS_PER_METRE)
        .collect();
    let rotations: Vec<Quat> = rotations
        .iter()
        .map(|rotation| coords::rotation_to_source(*rotation))
        .collect();
    let mut bodies = Vec::with_capacity(positions.len());
    let mut shape_def = b3::default_shape_def();
    shape_def.density = BONE_DENSITY;
    shape_def.base_material.friction = 0.65;
    shape_def.base_material.restitution = 0.02;
    shape_def.filter = filter(RAGDOLL_GROUP, WORLD_GROUP | RAGDOLL_GROUP);
    shape_def.filter.group_index = group_index;
    for (index, (&position, &rotation)) in positions.iter().zip(&rotations).enumerate() {
        let mut body_def = b3::default_body_def();
        body_def.type_ = BodyType::Dynamic;
        body_def.position = b3v(position);
        body_def.rotation = b3q(rotation);
        body_def.linear_damping = 0.05;
        body_def.angular_damping = 0.2;
        body_def.is_bullet = true;
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
            let radius = bone_radius(&skeleton.joints[index].name, length);
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
        let bone = skeleton.joints[child].name.to_ascii_lowercase();
        let friction = joint_friction(&bone);
        let hinge = is_hinge(&bone);
        let turn = if hinge {
            hinge_axis(child, &positions, parents, &rotations).map_or(Quat::IDENTITY, |axis| {
                Quat::from_rotation_arc(Vec3::Z, axis)
            })
        } else {
            Quat::IDENTITY
        };
        let frame1_q = if hinge {
            rotations[parent].inverse() * turn
        } else {
            rotations[parent].inverse() * rotations[child]
        };
        let frame2_q = if hinge {
            rotations[child].inverse() * turn
        } else {
            Quat::IDENTITY
        };
        let frame1 = b3::Transform {
            p: b3v(rotations[parent]
                .inverse()
                .mul_vec3(positions[child] - positions[parent])),
            q: b3q(frame1_q.normalize()),
        };
        let frame2 = b3::Transform {
            p: B3Vec3::default(),
            q: b3q(frame2_q.normalize()),
        };
        if hinge {
            let limit = 35.0 * std::f32::consts::PI / 180.0;
            let mut def = b3::default_revolute_joint_def();
            def.base.body_id_a = bodies[parent];
            def.base.body_id_b = bodies[child];
            def.base.local_frame_a = frame1;
            def.base.local_frame_b = frame2;
            def.base.collide_connected = false;
            def.enable_limit = true;
            def.lower_angle = -limit;
            def.upper_angle = limit;
            def.enable_spring = true;
            def.hertz = 1.0;
            def.damping_ratio = 0.7;
            def.enable_motor = true;
            def.max_motor_torque = friction * JOINT_TORQUE;
            b3::joint::create_revolute_joint(world, &def);
        } else {
            let (cone, twist) = joint_limits(&bone);
            let mut def = b3::default_spherical_joint_def();
            def.base.body_id_a = bodies[parent];
            def.base.body_id_b = bodies[child];
            def.base.local_frame_a = frame1;
            def.base.local_frame_b = frame2;
            def.base.collide_connected = false;
            def.enable_cone_limit = true;
            def.cone_angle = cone;
            def.enable_twist_limit = true;
            def.lower_twist_angle = -twist;
            def.upper_twist_angle = twist;
            // The engine's human ragdoll: a soft spring back to the rest pose
            // plus a zero-velocity motor acting as joint friction.
            def.enable_spring = true;
            def.hertz = 1.0;
            def.damping_ratio = 0.7;
            def.enable_motor = true;
            def.max_motor_torque = friction * JOINT_TORQUE;
            b3::joint::create_spherical_joint(world, &def);
        }
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

/// Elbows and knees are hinges, not ball joints; everything else bends
/// freely within its cone and twist limits.
fn is_hinge(name: &str) -> bool {
    name.contains("forearm") || name.contains("calve") || name.contains("calf")
}

/// The natural hinge axis of a joint: the normal of the plane through the
/// parent, child and grandchild bones. Falls back to the child's local X
/// when the limb is perfectly straight.
fn hinge_axis(
    child: usize,
    positions: &[Vec3],
    parents: &[Option<usize>],
    rotations: &[Quat],
) -> Option<Vec3> {
    let parent = parents[child]?;
    let grandchild = parents
        .iter()
        .enumerate()
        .find_map(|(index, parent)| (*parent == Some(child)).then_some(index))?;
    let upper = positions[child] - positions[parent];
    let lower = positions[grandchild] - positions[child];
    upper
        .cross(lower)
        .try_normalize()
        .or_else(|| Some(rotations[child] * Vec3::X))
}

/// Angular limits for a joint, by the bone it drives: a cone around the
/// joint frame's axis and a twist range, radians. Based on Box3D's human
/// ragdoll samples, loosened a little for Valve's skeletons.
fn joint_limits(name: &str) -> (f32, f32) {
    let name = name.to_ascii_lowercase();
    let degrees = (std::f32::consts::PI / 180.0) as f32;
    if name.contains("head") {
        (30.0 * degrees, 17.5 * degrees)
    } else if name.contains("neck") {
        (30.0 * degrees, 17.5 * degrees)
    } else if name.contains("spine") || name.contains("pelvis") || name.contains("chest") {
        (35.0 * degrees, 17.5 * degrees)
    } else if name.contains("thigh") {
        (40.0 * degrees, 10.0 * degrees)
    } else if name.contains("calve") || name.contains("calf") {
        (35.0 * degrees, 10.0 * degrees)
    } else if name.contains("upperarm") {
        (50.0 * degrees, 20.0 * degrees)
    } else if name.contains("forearm") {
        (40.0 * degrees, 10.0 * degrees)
    } else if name.contains("hand") || name.contains("foot") || name.contains("clavicle") {
        (30.0 * degrees, 10.0 * degrees)
    } else {
        (30.0 * degrees, 10.0 * degrees)
    }
}

/// How much zero-velocity joint friction a bone gets. Elbows are nearly
/// free; the rest are stiff enough for a body to hold together.
fn joint_friction(name: &str) -> f32 {
    let name = name.to_ascii_lowercase();
    if name.contains("forearm") || name.contains("hand") {
        0.08
    } else if name.contains("upperarm") {
        0.8
    } else {
        1.0
    }
}

/// The collision radius of a bone, Source units: thicker for the torso and
/// head, thinner for limbs, as in Box3D's human ragdoll.
fn bone_radius(name: &str, length: f32) -> f32 {
    let name = name.to_ascii_lowercase();
    let metres = if name.contains("head") {
        0.11
    } else if name.contains("neck") {
        0.06
    } else if name.contains("pelvis") || name.contains("spine") || name.contains("chest") {
        0.13
    } else if name.contains("thigh") {
        0.085
    } else if name.contains("calve") || name.contains("calf") {
        0.07
    } else if name.contains("foot") {
        0.05
    } else if name.contains("upperarm") {
        0.055
    } else if name.contains("forearm") || name.contains("hand") {
        0.045
    } else {
        0.05
    };
    let maximum = (length * 0.45).max(MIN_BONE_RADIUS);
    (metres * UNITS_PER_METRE).clamp(MIN_BONE_RADIUS, maximum)
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
    let frame = time.delta_secs().min(STEP * MAX_STEPS as f32);
    physics.accumulator += frame;
    let steps = ((physics.accumulator / STEP).floor() as usize).min(MAX_STEPS);
    physics.accumulator -= STEP * steps as f32;
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
    for _ in 0..steps {
        world.step(STEP, SUB_STEPS);
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
        let rotation = coords::rotation_from_source(b3q_to_glam(transform_body.q));
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
        let (head, _) = joint_limits("ValveBiped.Bip01_Head1");
        let (forearm, _) = joint_limits("ValveBiped.Bip01_R_Forearm");
        let (spine, _) = joint_limits("ValveBiped.Bip01_Spine");
        let (thigh, _) = joint_limits("ValveBiped.Bip01_R_Thigh");
        assert!(head < forearm);
        assert!(spine < thigh);
        assert!(joint_limits("anything").0 > 0.0);
    }
}
