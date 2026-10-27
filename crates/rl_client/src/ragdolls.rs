//! Player death ragdolls: the body of a player who died, going limp and
//! falling where they stood.
//!
//! A ragdoll reuses the player's model root and skeleton. When its player is
//! killed, the root stops being a player view
//! (the server draws a fresh, hidden model for the dead player and shows it
//! again on respawn) and becomes a small rigid-body rig: the pelvis, spine,
//! chest, neck, head and the limb bones are simulated by `Box3D`, with capsule
//! colliders along the bones, mass from the bone volume, and spherical or
//! revolute joints tuned like the engine's human ragdoll sample. Fingers,
//! toes, clavicles and other detail bones follow the simulated bone they hang
//! from, so the skin bends with the skeleton instead of stretching, and the
//! last animation pose is where the body starts.
//!
//! The rig lives in the same Source-unit frame as the map's collision and the
//! server's physics: Source's +Z is up. Only the visual root transform is
//! converted to Bevy space when the skeleton is posed.
//!
//! It is visual only: the server does not know or care where a corpse lies.
//! Corpses are removed after 25 seconds, and at most eight are kept, oldest
//! first.

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
use rl_assets::{Skeleton, coords};
use rl_data::Shot;
use rl_sim::physics::UNITS_PER_METRE;
use rl_sim::simulation::{ExplosionEvent, PlayerId, ShotEvent, Simulation};

use crate::AppState;
use crate::actors::PlayerView;
use crate::animation::{Animated, SequencePlayer};
use crate::net::Game;
use crate::player::Ticked;
use crate::visual_world::{RAGDOLL_GROUP, WORLD_GROUP, build_static_world, filter};

/// How long a corpse lies before it is removed, seconds.
const RAGDOLL_LIFE: f32 = 25.0;
/// Seconds a body the energy ball dissolved lasts.
const DISSOLVE_LIFE: f32 = 2.0;
/// Most corpses alive at once.
const MAX_RAGDOLLS: usize = 8;
/// Radius of the smallest bone collider, Source units.
const MIN_BONE_RADIUS: f32 = 0.025 * UNITS_PER_METRE;
/// Seconds of physics per fixed step.
const STEP: f32 = 1.0 / 60.0;
/// Most steps one frame may run, so a hitch does not turn into a time bomb.
const MAX_STEPS: usize = 4;
/// `Box3D` sub-steps per fixed step, as in the engine's ragdoll samples.
const SUB_STEPS: i32 = 4;
/// How much of a killing hitscan's prop impulse reaches the ragdoll.
const SHOT_IMPULSE_SCALE: f32 = 0.25;
/// Most impulse one bullet may put into a body, kg·Source-units/s.
const MAX_SHOT_IMPULSE: f32 = 25.0 * UNITS_PER_METRE;
/// Most speed one bullet may give a single bone, Source units/s. A light
/// hand takes the same impulse as a heavy pelvis, so impulses are limited by
/// the speed they would cause and not only by their size.
const MAX_SHOT_SPEED: f32 = 3.5 * UNITS_PER_METRE;
/// Extra share of a bullet's impulse applied at the body's root, so the
/// whole corpse is shoved and not just the struck limb.
const SHOT_BODY_SHOVE: f32 = 0.35;
/// Fastest any bone of a corpse may move, Source units/s: the world's speed
/// limit. A hard blast still throws a body, but nothing is fired like a
/// bullet and no bone can cross a thin wall in one step.
const MAX_BONE_SPEED: f32 = 16.0 * UNITS_PER_METRE;
/// How fast overlapping shapes are pushed apart, Source units/s. Bones are
/// spawned in the pose of the living player, often touching the map; a gentle
/// push lets them ease out instead of popping.
const PUSH_OUT_SPEED: f32 = 1.0 * UNITS_PER_METRE;
/// Contact stiffness and damping: `Box3D`'s defaults, kept explicit because
/// the push-out speed is set beside them.
const CONTACT_HERTZ: f32 = 30.0;
const CONTACT_DAMPING: f32 = 10.0;
/// Fastest any bone may spin, radians/s. A hit off a light bone's centre
/// would otherwise spin it faster than the solver can follow.
const MAX_BONE_SPIN: f32 = 14.0;
/// How near two bones of a body may be in the pose of death, Source units,
/// and still collide afterwards. Bones closer than this are left alone, so a
/// pose with limbs pressed against the body does not start by shoving them
/// apart.
const SELF_COLLISION_MARGIN: f32 = 0.02 * UNITS_PER_METRE;
/// Farthest a bone may stray from where its player died before the corpse
/// is judged broken and frozen, Source units. A corpse falling down a deep
/// shaft is fine; one that fell through the map is not.
const MAX_STRAY: f32 = 150.0 * UNITS_PER_METRE;
/// Farthest a bone may be from the pelvis before the body is judged torn
/// apart, Source units. A person is under two metres tall.
const MAX_STRETCH: f32 = 2.5 * UNITS_PER_METRE;
/// How far from a shot a corpse bone is still pushed, Source units.
const CORPSE_SHOT_RADIUS: f32 = 0.28 * UNITS_PER_METRE;
/// How fast a blast throws a body at its centre, Source units/s.
const BLAST_SPEED: f32 = 11.0 * UNITS_PER_METRE;
/// How long a shot that hit a player is remembered for their death, seconds.
/// The fatal shot may be predicted locally while the kill arrives from the
/// server a little later.
const HIT_MEMORY: f32 = 1.0;
/// Density of a body in `Box3D`'s (Source-unit) space, kg/unit³: human flesh
/// is close enough to water.
const BONE_DENSITY: f32 = 1000.0 / (UNITS_PER_METRE * UNITS_PER_METRE * UNITS_PER_METRE);
/// Joint friction torque in `Box3D`'s units: `Box3D` lengths are Source inches,
/// while the engine samples use N·m, so one N·m is scaled by L². The ragdoll
/// samples use 5 to 10 N·m, enough that a body holds together without
/// being rigid.
const JOINT_TORQUE: f32 = 6.0 * UNITS_PER_METRE * UNITS_PER_METRE;
/// Stiffness of the pull of each joint back to the pose of death, cycles per
/// second. Like the ragdoll samples' it is soft, but it is what keeps a
/// corpse from folding up like a bag; without it limbs are limp.
const JOINT_HERTZ: f32 = 1.0;
/// Damping of that pull, non-dimensional. Below one it rings.
const JOINT_DAMPING: f32 = 0.7;

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

/// The shared `Box3D` world all corpses are simulated in.
#[derive(Resource, Default)]
struct RagdollPhysics {
    world: Option<Mutex<World>>,
    /// Leftover time from the last frame, so physics always advances in
    /// fixed 1/60 s steps.
    accumulator: f32,
}

impl RagdollPhysics {
    /// The world, built from the map's collision when first needed.
    fn world(&mut self, sim: &Simulation) -> &mut World {
        self.world
            .get_or_insert_with(|| Mutex::new(corpse_world(sim)))
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

/// The world corpses are simulated in: the map, with limits that keep a
/// body from being thrown through it.
fn corpse_world(sim: &Simulation) -> World {
    let mut world = build_static_world(sim, RAGDOLL_GROUP);
    tune_world(&mut world);
    world
}

/// Applies the limits that keep corpses stable to a world.
fn tune_world(world: &mut World) {
    b3::world::world_set_maximum_linear_speed(world, MAX_BONE_SPEED);
    b3::world::world_set_contact_tuning(world, CONTACT_HERTZ, CONTACT_DAMPING, PUSH_OUT_SPEED);
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

/// A corpse: the rigid bodies of the simulated bones and the entities they
/// pose.
#[derive(Component)]
pub(crate) struct Ragdoll {
    /// The player whose body this is.
    pub(crate) player: PlayerId,
    /// Every skeleton joint, in skeleton order.
    joints: Vec<Entity>,
    /// Each skeleton joint's parent.
    parents: Vec<Option<usize>>,
    /// One `Box3D` body per simulated bone.
    bodies: Vec<b3::BodyId>,
    /// The simulated bone the whole rig hangs from.
    root: usize,
    /// For each skeleton joint, the simulated body that carries it.
    drivers: Vec<usize>,
    /// For each skeleton joint, its transform relative to its driver when
    /// the player died, in Bevy space. Simulated joints have identity here;
    /// fingers, toes and other detail bones keep their pose rigidly.
    rests: Vec<Transform>,
    /// World position of the model root; joints hang in world space around
    /// it.
    origin: Vec3,
    /// Where the body was when it died, Source units: the centre of its
    /// bones, for noticing one that has been thrown unreasonably far.
    anchor: Vec3,
    /// Set when the simulation of this corpse was stopped because it went
    /// wrong; it keeps the last pose it had.
    frozen: bool,
    pub(crate) born: f32,
    /// Seconds the corpse lasts: a dissolved one is gone in a couple.
    life: f32,
}

/// The part of the physical world a corpse owns.
struct PhysRig {
    /// The skeleton joint each body follows, in skeleton order.
    joints: Vec<usize>,
    /// Each body's spawn position, Source units.
    positions: Vec<Vec3>,
    bodies: Vec<b3::BodyId>,
    /// The body the rig hangs from.
    root: usize,
}

/// The anatomical role of a simulated bone. Parameters follow `Box3D`'s human
/// ragdoll sample and Valve's skeletons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoneKind {
    Pelvis,
    Spine,
    Chest,
    Neck,
    Head,
    UpperArm,
    Forearm,
    Hand,
    Thigh,
    Calf,
    Foot,
    Other,
}

impl BoneKind {
    /// The role a bone's name suggests, if any.
    fn of(name: &str) -> Self {
        let name = name.to_ascii_lowercase();
        if name.contains("pelvis") || name.contains("hips") {
            Self::Pelvis
        } else if name.contains("spine") || name.contains("chest") {
            Self::Spine
        } else if name.contains("neck") {
            Self::Neck
        } else if name.contains("head") {
            Self::Head
        } else if name.contains("upperarm") || name.contains("upper_arm") {
            Self::UpperArm
        } else if name.contains("forearm")
            || name.contains("lowerarm")
            || name.contains("lower_arm")
        {
            Self::Forearm
        } else if name.contains("hand") {
            Self::Hand
        } else if name.contains("thigh") || name.contains("upperleg") || name.contains("upper_leg")
        {
            Self::Thigh
        } else if name.contains("calf")
            || name.contains("calve")
            || name.contains("shin")
            || name.contains("lowerleg")
            || name.contains("lower_leg")
        {
            Self::Calf
        } else if name.contains("foot") {
            Self::Foot
        } else {
            Self::Other
        }
    }

    /// Elbows and knees are hinges, not ball joints.
    fn is_hinge(self) -> bool {
        matches!(self, Self::Forearm | Self::Calf)
    }

    /// The collider radius in metres, by body part.
    fn radius(self) -> f32 {
        match self {
            Self::Pelvis | Self::Chest => 0.13,
            Self::Spine | Self::Head => 0.11,
            Self::Thigh => 0.085,
            Self::Neck | Self::Calf | Self::Foot => 0.06,
            Self::UpperArm | Self::Hand => 0.055,
            Self::Other | Self::Forearm => 0.05,
        }
    }

    /// Contact friction of the bone against the world.
    fn contact_friction(self) -> f32 {
        match self {
            Self::Pelvis | Self::Thigh | Self::UpperArm => 0.6,
            Self::Spine | Self::Chest | Self::Other => 0.5,
            Self::Forearm | Self::Hand => 0.3,
            Self::Neck | Self::Head | Self::Foot => 0.2,
            Self::Calf => 0.1,
        }
    }

    /// Rolling resistance of the bone against the world.
    fn rolling_resistance(self) -> f32 {
        match self {
            Self::Pelvis
            | Self::Spine
            | Self::Chest
            | Self::Thigh
            | Self::UpperArm
            | Self::Forearm => 0.1,
            Self::Neck | Self::Head | Self::Hand => 0.05,
            Self::Calf | Self::Foot | Self::Other => 0.0,
        }
    }

    /// How much zero-velocity joint friction a bone gets. Elbows and wrists
    /// are nearly free; the rest are stiff enough for a body to hold
    /// together.
    fn joint_friction(self) -> f32 {
        match self {
            Self::Forearm | Self::Hand => 0.06,
            Self::UpperArm => 0.8,
            _ => 1.0,
        }
    }

    /// The cone (swing) and twist limits of a ball joint, radians.
    fn limits(self) -> (f32, f32) {
        let degrees = std::f32::consts::PI / 180.0;
        match self {
            Self::Pelvis | Self::Spine | Self::Chest => (35.0 * degrees, 17.5 * degrees),
            Self::Neck | Self::Head => (30.0 * degrees, 17.5 * degrees),
            Self::UpperArm => (45.0 * degrees, 20.0 * degrees),
            Self::Thigh | Self::Forearm | Self::Calf | Self::Hand | Self::Foot | Self::Other => {
                (30.0 * degrees, 10.0 * degrees)
            }
        }
    }

    /// The flexion range of a hinge, radians.
    fn hinge_limit(self) -> f32 {
        let degrees = std::f32::consts::PI / 180.0;
        match self {
            Self::Forearm => 35.0 * degrees,
            _ => 30.0 * degrees,
        }
    }
}

/// Spawns a corpse for every player killed in the ticks read this frame.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
                tracing::warn!(victim = ?kill.victim, "ragdoll: could not find every skeleton joint");
                continue;
            };
            let Some(captured) = capture_joints(&joints, &globals) else {
                tracing::warn!(victim = ?kill.victim, "ragdoll: could not read every joint transform");
                continue;
            };
            let positions: Vec<Vec3> = captured
                .iter()
                .map(|&(position, _)| source_position(position))
                .collect();
            let rotations: Vec<Quat> = captured
                .iter()
                .map(|&(_, rotation)| coords::rotation_to_source(rotation))
                .collect();
            let origin = root.translation();
            let root_rotation = root.rotation();
            let forward =
                (coords::rotation_to_source(root_rotation) * Vec3::X).normalize_or(Vec3::X);
            let parents: Vec<Option<usize>> = skeleton.joints.iter().map(|j| j.parent).collect();
            let Some(selection) = select_bones(skeleton) else {
                tracing::warn!(victim = ?kill.victim, "ragdoll: no bones selected");
                continue;
            };
            let rig = build_phys_rig(world, &positions, &rotations, &parents, &selection, forward);
            let drivers = drivers_for(&selection, &parents);
            let Some(rests) = driver_rests(&joints, &rig, &drivers, &globals) else {
                tracing::warn!(victim = ?kill.victim, "ragdoll: could not build driver rests");
                for body in &rig.bodies {
                    if b3::body::body_is_valid(world, *body) {
                        b3::body::destroy_body(world, *body);
                    }
                }
                continue;
            };
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
            let anchor = rig.positions.iter().copied().sum::<Vec3>() / rig.positions.len() as f32;
            apply_death_forces(world, &rig, &shots, &events.explosions, sim, kill.victim);

            commands
                .entity(entity)
                .remove::<(PlayerView, Animated, SequencePlayer, AnimationPlayer)>()
                .insert((
                    Ragdoll {
                        player: kill.victim,
                        joints,
                        parents,
                        bodies: rig.bodies,
                        root: rig.root,
                        drivers,
                        rests,
                        origin,
                        anchor,
                        frozen: false,
                        born: time.elapsed_secs(),
                        life: if kill.dissolved {
                            DISSOLVE_LIFE
                        } else {
                            RAGDOLL_LIFE
                        },
                    },
                    Visibility::Inherited,
                ));
        }
        hits.shots
            .extend(current.iter().map(|&(victim, shot)| (victim, shot, now)));
        hits.shots.retain(|(_, _, at)| now - *at <= HIT_MEMORY);
    }
}

/// The joints to simulate, by skeleton index, in skeleton order. A Valve
/// skeleton gets a compact sample-like rig: pelvis, two spine bones, neck,
/// head, upper arms, forearms, hands, thighs, calves and feet. Detail bones
/// (fingers, toes, clavicles and the middle spine bones) are carried by the
/// nearest simulated ancestor.
fn select_bones(skeleton: &Skeleton) -> Option<Vec<(usize, BoneKind)>> {
    let mut selected = Vec::new();
    let mut spine = Vec::new();
    for (index, joint) in skeleton.joints.iter().enumerate() {
        match BoneKind::of(&joint.name) {
            BoneKind::Spine => spine.push(index),
            BoneKind::Other => {}
            kind => selected.push((index, kind)),
        }
    }
    if spine.len() == 1 {
        selected.push((spine[0], BoneKind::Spine));
    } else if let (Some(first), Some(last)) = (spine.first(), spine.last()) {
        selected.push((*first, BoneKind::Spine));
        selected.push((*last, BoneKind::Chest));
    }
    // The rig needs one body every skeleton joint can hang from. Valve
    // skeletons root at the pelvis, which is selected above; other
    // skeletons may have a dummy root bone.
    for (root, joint) in skeleton.joints.iter().enumerate() {
        if joint.parent.is_none() && !selected.iter().any(|(index, _)| *index == root) {
            selected.push((root, BoneKind::of(&joint.name)));
        }
    }
    selected.sort_by_key(|(index, _)| *index);
    selected.dedup_by_key(|(index, _)| *index);
    if selected.len() < 4 {
        // An unfamiliar skeleton: simulate every bone rather than none.
        selected = skeleton
            .joints
            .iter()
            .enumerate()
            .map(|(index, joint)| (index, BoneKind::of(&joint.name)))
            .collect();
    }
    (!selected.is_empty()).then_some(selected)
}

/// The physical bone each skeleton joint hangs from: itself when simulated,
/// else its nearest simulated ancestor.
fn drivers_for(selection: &[(usize, BoneKind)], parents: &[Option<usize>]) -> Vec<usize> {
    let mut phys_of: Vec<Option<usize>> = vec![None; parents.len()];
    for (phys, &(joint, _)) in selection.iter().enumerate() {
        if let Some(slot) = phys_of.get_mut(joint) {
            *slot = Some(phys);
        }
    }
    (0..parents.len())
        .map(|index| {
            let mut at = Some(index);
            while let Some(joint) = at {
                if let Some(phys) = phys_of.get(joint).copied().flatten() {
                    return phys;
                }
                at = parents.get(joint).copied().flatten();
            }
            0
        })
        .collect()
}

/// Builds one body and collider per simulated bone, then joins them with the
/// same spherical and revolute joints as `Box3D`'s human ragdoll.
#[allow(clippy::too_many_lines)]
fn build_phys_rig(
    world: &mut World,
    positions: &[Vec3],
    rotations: &[Quat],
    parents: &[Option<usize>],
    selection: &[(usize, BoneKind)],
    forward: Vec3,
) -> PhysRig {
    let mut phys_of: Vec<Option<usize>> = vec![None; parents.len()];
    for (phys, &(joint, _)) in selection.iter().enumerate() {
        phys_of[joint] = Some(phys);
    }
    let phys_parents: Vec<Option<usize>> = selection
        .iter()
        .map(|&(joint, _)| nearest_selected_ancestor(joint, parents, &phys_of))
        .collect();
    let kind_at: Vec<BoneKind> = (0..parents.len())
        .map(|index| phys_of[index].map_or(BoneKind::Other, |phys| selection[phys].1))
        .collect();
    let tails: Vec<Vec3> = selection
        .iter()
        .map(|&(joint, kind)| bone_tail(joint, kind, &kind_at, &phys_of, positions, parents))
        .collect();

    let mut bodies = Vec::with_capacity(selection.len());
    for (phys, &(joint, kind)) in selection.iter().enumerate() {
        let position = positions[joint];
        let rotation = rotations[joint];
        let mut body_def = b3::default_body_def();
        body_def.type_ = BodyType::Dynamic;
        body_def.position = b3v(position);
        body_def.rotation = b3q(rotation);
        // A little damping settles the limbs without making the corpse
        // float; the joints' springs are damped too.
        body_def.linear_damping = 0.05;
        body_def.angular_damping = 0.5;
        // The samples use no CCD; the map is thick enough for a corpse and
        // bullets on every bone only add solver noise.
        body_def.is_bullet = false;
        body_def.user_data = u64::try_from(phys).unwrap_or(u64::MAX);
        let handle = create_body(world, &body_def);
        bodies.push(handle);

        let mut shape_def = b3::default_shape_def();
        shape_def.density = BONE_DENSITY;
        shape_def.base_material.friction = kind.contact_friction();
        shape_def.base_material.rolling_resistance = kind.rolling_resistance();
        shape_def.base_material.restitution = 0.02;
        shape_def.filter = filter(RAGDOLL_GROUP, WORLD_GROUP | RAGDOLL_GROUP);

        let tail = tails[phys];
        let length = tail.distance(position);
        let radius = (kind.radius() * UNITS_PER_METRE).max(MIN_BONE_RADIUS);
        // The collider is in the body's local frame, so the bone's world
        // direction is taken back through the body's rotation.
        let direction = rotation
            .inverse()
            .mul_vec3(tail - position)
            .normalize_or(Vec3::X);
        let center = direction * (length * 0.5);
        let half_height = length * 0.5 - radius;
        if half_height > 0.01 * UNITS_PER_METRE {
            let capsule = Capsule {
                center1: b3v(center - direction * half_height),
                center2: b3v(center + direction * half_height),
                radius,
            };
            create_capsule_shape(world, handle, &shape_def, &capsule);
        } else {
            // A bone shorter than it is thick (a head, a hand, a pelvis) is a
            // ball in the middle of its segment; squashing a fat capsule into
            // it would make a light body with little inertia.
            create_sphere_shape(
                world,
                handle,
                &shape_def,
                &Sphere {
                    center: b3v(center),
                    radius,
                },
            );
        }
    }

    for (phys, &(joint, kind)) in selection.iter().enumerate() {
        let Some(parent) = phys_parents[phys] else {
            continue;
        };
        let parent_joint = selection[parent].0;
        let position = positions[joint];
        let parent_position = positions[parent_joint];
        let upper = position - parent_position;
        let lower = tails[phys] - position;
        let axis = if kind.is_hinge() {
            hinge_axis(upper, lower, forward)
        } else {
            lower.try_normalize().unwrap_or(forward)
        };
        let turn = Quat::from_rotation_arc(Vec3::Z, axis);
        let turn_a = rotations[parent_joint].inverse() * turn;
        let turn_b = rotations[joint].inverse() * turn;
        let frame_a = b3::Transform {
            p: b3v(rotations[parent_joint].inverse().mul_vec3(upper)),
            q: b3q(turn_a.normalize()),
        };
        let frame_b = b3::Transform {
            p: B3Vec3::default(),
            q: b3q(turn_b.normalize()),
        };
        if kind.is_hinge() {
            let limit = kind.hinge_limit();
            let mut def = b3::default_revolute_joint_def();
            def.base.body_id_a = bodies[parent];
            def.base.body_id_b = bodies[phys];
            def.base.local_frame_a = frame_a;
            def.base.local_frame_b = frame_b;
            def.base.collide_connected = false;
            def.enable_limit = true;
            def.lower_angle = -limit;
            def.upper_angle = limit;
            def.enable_spring = true;
            def.hertz = JOINT_HERTZ;
            def.damping_ratio = JOINT_DAMPING;
            def.enable_motor = true;
            def.max_motor_torque = kind.joint_friction() * JOINT_TORQUE;
            b3::joint::create_revolute_joint(world, &def);
        } else {
            let (cone, twist) = kind.limits();
            let mut def = b3::default_spherical_joint_def();
            def.base.body_id_a = bodies[parent];
            def.base.body_id_b = bodies[phys];
            def.base.local_frame_a = frame_a;
            def.base.local_frame_b = frame_b;
            def.base.collide_connected = false;
            def.enable_cone_limit = true;
            def.cone_angle = cone;
            def.enable_twist_limit = true;
            def.lower_twist_angle = -twist;
            def.upper_twist_angle = twist;
            // A zero-velocity motor acts as joint friction, and a soft spring
            // toward the pose of death keeps the body from going limp, as in
            // the engine's ragdoll samples.
            def.enable_spring = true;
            def.hertz = JOINT_HERTZ;
            def.damping_ratio = JOINT_DAMPING;
            def.enable_motor = true;
            def.max_motor_torque = kind.joint_friction() * JOINT_TORQUE;
            b3::joint::create_spherical_joint(world, &def);
        }
    }

    // Bones collide with the rest of their own body, except the ones that
    // touch it by design: neighbours, and anything that already overlaps in
    // the pose of death, which would otherwise be pushed apart against the
    // joints' springs.
    let segments: Vec<(Vec3, Vec3, f32)> = selection
        .iter()
        .enumerate()
        .map(|(phys, &(joint, kind))| {
            (
                positions[joint],
                tails[phys],
                kind.radius() * UNITS_PER_METRE,
            )
        })
        .collect();
    for (a, b) in self_collision_exclusions(&segments, &phys_parents) {
        let mut def = b3::default_filter_joint_def();
        def.base.body_id_a = bodies[a];
        def.base.body_id_b = bodies[b];
        b3::joint::create_filter_joint(world, &def);
    }

    let root = phys_parents.iter().position(Option::is_none).unwrap_or(0);
    PhysRig {
        joints: selection.iter().map(|&(joint, _)| joint).collect(),
        positions: selection
            .iter()
            .map(|&(joint, _)| positions[joint])
            .collect(),
        bodies,
        root,
    }
}

/// The pairs of bones of one body that must not collide: a bone and its
/// parent (a forearm may still fold against the chest), and any two bones whose capsules (`segments`: the
/// two ends and the radius) overlap or all but touch in the pose of death.
fn self_collision_exclusions(
    segments: &[(Vec3, Vec3, f32)],
    phys_parents: &[Option<usize>],
) -> Vec<(usize, usize)> {
    let parent = |bone: usize| phys_parents.get(bone).copied().flatten();
    let mut excluded = Vec::new();
    for a in 0..segments.len() {
        for b in (a + 1)..segments.len() {
            let related = parent(a) == Some(b) || parent(b) == Some(a);
            let (start_a, end_a, radius_a) = segments[a];
            let (start_b, end_b, radius_b) = segments[b];
            let gap = segment_distance(start_a, end_a, start_b, end_b) - radius_a - radius_b;
            if related || gap < SELF_COLLISION_MARGIN {
                excluded.push((a, b));
            }
        }
    }
    excluded
}

/// The shortest distance between the segments `start_a`-`end_a` and
/// `start_b`-`end_b`.
fn segment_distance(start_a: Vec3, end_a: Vec3, start_b: Vec3, end_b: Vec3) -> f32 {
    let along_a = end_a - start_a;
    let along_b = end_b - start_b;
    let offset = start_a - start_b;
    let length_a = along_a.length_squared();
    let length_b = along_b.length_squared();
    let b_on_offset = along_b.dot(offset);
    // The fractions along each segment of the closest points.
    let (on_a, on_b) = if length_a <= f32::EPSILON && length_b <= f32::EPSILON {
        (0.0, 0.0)
    } else if length_a <= f32::EPSILON {
        (0.0, (b_on_offset / length_b).clamp(0.0, 1.0))
    } else {
        let a_on_offset = along_a.dot(offset);
        if length_b <= f32::EPSILON {
            ((-a_on_offset / length_a).clamp(0.0, 1.0), 0.0)
        } else {
            let alignment = along_a.dot(along_b);
            let denominator = length_a * length_b - alignment * alignment;
            let first = if denominator > f32::EPSILON {
                ((alignment * b_on_offset - a_on_offset * length_b) / denominator).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let second = (alignment * first + b_on_offset) / length_b;
            if second < 0.0 {
                ((-a_on_offset / length_a).clamp(0.0, 1.0), 0.0)
            } else if second > 1.0 {
                (((alignment - a_on_offset) / length_a).clamp(0.0, 1.0), 1.0)
            } else {
                (first, second)
            }
        }
    };
    (start_a + along_a * on_a).distance(start_b + along_b * on_b)
}

/// The nearest simulated ancestor of `joint`, if any.
fn nearest_selected_ancestor(
    joint: usize,
    parents: &[Option<usize>],
    phys_of: &[Option<usize>],
) -> Option<usize> {
    let mut at = parents.get(joint).copied().flatten();
    while let Some(ancestor) = at {
        if let Some(phys) = phys_of.get(ancestor).copied().flatten() {
            return Some(phys);
        }
        at = parents.get(ancestor).copied().flatten();
    }
    None
}

/// The far end of a bone, Source units. A simulated child ends the segment;
/// a leaf limb reaches for its fingers or toes, and a leaf head extends
/// along the neck.
fn bone_tail(
    joint: usize,
    kind: BoneKind,
    kind_at: &[BoneKind],
    phys_of: &[Option<usize>],
    positions: &[Vec3],
    parents: &[Option<usize>],
) -> Vec3 {
    // A dummy root bone is not part of the body; it only anchors the rig.
    if kind == BoneKind::Other && parents.get(joint).copied().flatten().is_none() {
        return positions[joint] + Vec3::Z * (0.02 * UNITS_PER_METRE);
    }
    let preferred = match kind {
        BoneKind::Pelvis => Some(&[BoneKind::Spine, BoneKind::Chest][..]),
        BoneKind::Spine => Some(&[BoneKind::Chest][..]),
        BoneKind::Chest => Some(&[BoneKind::Neck, BoneKind::Head][..]),
        _ => None,
    };
    if let Some(preferred) = preferred
        && let Some(child) = nearest_selected_descendant(joint, parents, phys_of, kind_at, |kind| {
            preferred.contains(&kind)
        })
    {
        return positions[child];
    }
    if let Some(child) = nearest_selected_descendant(joint, parents, phys_of, kind_at, |_| true) {
        return positions[child];
    }
    if let Some(descendant) = farthest_descendant(joint, parents, positions, joint) {
        let distance = positions[joint].distance(positions[descendant]);
        if distance > 0.02 * UNITS_PER_METRE {
            return positions[descendant];
        }
    }
    let direction = parents
        .get(joint)
        .copied()
        .flatten()
        .and_then(|parent| (positions[joint] - positions[parent]).try_normalize())
        .unwrap_or(Vec3::Z);
    // A head with nothing above it still has a skull: its centre is about
    // a head's radius up from the bone.
    let reach = if kind == BoneKind::Head { 0.2 } else { 0.1 };
    positions[joint] + direction * (reach * UNITS_PER_METRE)
}

/// The closest simulated descendant of `joint` matching `wanted`.
fn nearest_selected_descendant(
    joint: usize,
    parents: &[Option<usize>],
    phys_of: &[Option<usize>],
    kind_at: &[BoneKind],
    wanted: impl Fn(BoneKind) -> bool,
) -> Option<usize> {
    let mut stack = vec![(joint, 0_usize)];
    let mut best: Option<(usize, usize, f32)> = None;
    while let Some((at, depth)) = stack.pop() {
        for (child, _) in parents
            .iter()
            .enumerate()
            .filter(|(_, parent)| **parent == Some(at))
        {
            if let Some(phys) = phys_of.get(child).copied().flatten()
                && wanted(kind_at[child])
            {
                let distance = rank(depth + 1, phys);
                if best.is_none_or(|(_, _, best)| distance < best) {
                    best = Some((child, depth + 1, distance));
                }
            }
            stack.push((child, depth + 1));
        }
    }
    best.map(|(child, _, _)| child)
}

/// A sortable depth for a candidate descendant.
fn rank(depth: usize, phys: usize) -> f32 {
    depth as f32 + phys as f32 * 1e-4
}

/// The farthest descendant of `joint`, including `joint` itself.
fn farthest_descendant(
    joint: usize,
    parents: &[Option<usize>],
    positions: &[Vec3],
    start: usize,
) -> Option<usize> {
    let mut best = (start, 0.0_f32);
    let mut stack = vec![joint];
    while let Some(at) = stack.pop() {
        for (child, _) in parents
            .iter()
            .enumerate()
            .filter(|(_, parent)| **parent == Some(at))
        {
            stack.push(child);
            let distance = positions[start].distance(positions[child]);
            if distance > best.1 {
                best = (child, distance);
            }
        }
    }
    (best.0 != start).then_some(best.0)
}

/// The natural hinge axis of a joint: the normal of the plane through the
/// upper and lower bones. A straight limb falls back to its lateral axis,
/// perpendicular to both the bone and the way the player faced.
fn hinge_axis(upper: Vec3, lower: Vec3, forward: Vec3) -> Vec3 {
    if let Some(axis) = upper.cross(lower).try_normalize() {
        return axis;
    }
    let direction = lower
        .try_normalize()
        .or_else(|| upper.try_normalize())
        .unwrap_or(Vec3::Z);
    direction
        .cross(forward)
        .try_normalize()
        .or_else(|| direction.cross(Vec3::Z).try_normalize())
        .or_else(|| direction.cross(Vec3::X).try_normalize())
        .unwrap_or(Vec3::X)
}

/// A Bevy-space world position as a Source-space (inch) position, the frame
/// of the ragdoll world. The renderer's up axis is Bevy's +Y, while Source
/// (and so the map's collision and gravity) is +Z up.
fn source_position(bevy: Vec3) -> Vec3 {
    Vec3::from(coords::to_source_position(bevy))
}

/// A joint's world position and rotation.
fn global_pose(global: &GlobalTransform) -> (Vec3, Quat) {
    (global.translation(), global.rotation())
}

/// The skeleton joints' world poses when the player died, in Bevy space.
fn capture_joints(
    joints: &[Entity],
    globals: &Query<&GlobalTransform>,
) -> Option<Vec<(Vec3, Quat)>> {
    joints
        .iter()
        .map(|joint| Some(global_pose(globals.get(*joint).ok()?)))
        .collect()
}

/// Each skeleton joint's pose relative to the simulated body that carries
/// it, in Bevy space.
fn driver_rests(
    joints: &[Entity],
    rig: &PhysRig,
    drivers: &[usize],
    globals: &Query<&GlobalTransform>,
) -> Option<Vec<Transform>> {
    joints
        .iter()
        .enumerate()
        .map(|(index, entity)| {
            let driver_joint = *rig.joints.get(drivers[index])?;
            let driver = globals.get(joints[driver_joint]).ok()?;
            let joint = globals.get(*entity).ok()?;
            Some(Transform {
                translation: driver.rotation().inverse()
                    * (joint.translation() - driver.translation()),
                rotation: driver.rotation().inverse() * joint.rotation(),
                scale: Vec3::ONE,
            })
        })
        .collect()
}

/// Applies the killing blow where it landed, and throws the body with any
/// blast that caught it.
fn apply_death_forces(
    world: &mut World,
    rig: &PhysRig,
    shots: &[ShotEvent],
    blasts: &[ExplosionEvent],
    sim: &Simulation,
    victim: PlayerId,
) {
    for shot in shots {
        if shot.trace.struck != rl_sim::combat::hitscan::Struck::Player(victim) {
            continue;
        }
        let point = shot.trace.end;
        let direction = (shot.trace.end - shot.start).normalize_or(Vec3::Z);
        let force = shot_force(sim, shot.weapon);
        let reach = 0.45 * UNITS_PER_METRE;
        let mut weighted: Vec<(usize, f32)> = rig
            .positions
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
            && let Some(index) = nearest_body(&rig.positions, point)
        {
            weighted.push((index, 1.0));
        }
        let total: f32 = weighted.iter().map(|(_, weight)| weight).sum();
        if total <= 0.0 {
            continue;
        }
        for (index, weight) in weighted {
            let impulse = direction * force * weight / total;
            if let Some(body) = rig.bodies.get(index) {
                shove(world, *body, impulse, Some(point));
            }
        }
        if let Some(root) = rig.bodies.get(rig.root) {
            shove(world, *root, direction * force * SHOT_BODY_SHOVE, None);
        }
    }
    // A blast throws every part of a body near it outwards.
    for blast in blasts {
        let center = blast.position;
        let radius = (blast.radius * UNITS_PER_METRE).max(0.1);
        for handle in &rig.bodies {
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
            let direction = (offset.normalize_or_zero() + Vec3::Z * 0.35).normalize_or(Vec3::Z);
            let mass = body_get_mass(world, *handle).max(1.0);
            let impulse = direction * mass * BLAST_SPEED * falloff;
            shove(world, *handle, impulse, None);
        }
    }
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

/// Gives `impulse` to `body`, at `point` if there is one, else at its centre.
/// The impulse is limited to what moves the body at [`MAX_SHOT_SPEED`], so
/// the same blow cannot fling a hand like it nudges a pelvis, and the spin it
/// causes is limited too.
fn shove(world: &mut World, body: b3::BodyId, impulse: Vec3, point: Option<Vec3>) {
    if !b3::body::body_is_valid(world, body) {
        return;
    }
    let impulse = limited_impulse(impulse, body_get_mass(world, body), MAX_SHOT_SPEED);
    match point {
        Some(point) => body_apply_linear_impulse(world, body, b3v(impulse), b3v(point), true),
        None => body_apply_linear_impulse_to_center(world, body, b3v(impulse), true),
    }
    tame(world, &[body]);
}

/// `impulse`, shortened so a body of `mass` is not sped up by more than
/// `max_speed`. A non-finite impulse is nothing.
fn limited_impulse(impulse: Vec3, mass: f32, max_speed: f32) -> Vec3 {
    if !impulse.is_finite() {
        return Vec3::ZERO;
    }
    impulse.clamp_length_max(mass.max(0.0) * max_speed)
}

/// Limits how fast the bodies spin; the world already limits how fast they
/// move.
fn tame(world: &mut World, bodies: &[b3::BodyId]) {
    for body in bodies {
        if !b3::body::body_is_valid(world, *body) {
            continue;
        }
        let spin = b3v_to_glam(b3::body::body_get_angular_velocity(world, *body));
        if spin.length_squared() > MAX_BONE_SPIN * MAX_BONE_SPIN || !spin.is_finite() {
            let spin = if spin.is_finite() {
                spin.clamp_length_max(MAX_BONE_SPIN)
            } else {
                Vec3::ZERO
            };
            b3::body::body_set_angular_velocity(world, *body, b3v(spin));
        }
    }
}

/// Whether any of `bodies` is not a number, is farther than [`MAX_STRAY`]
/// from `anchor`, or is farther than [`MAX_STRETCH`] from the `root` body:
/// a corpse that went wrong, not one that was thrown.
fn has_run_away(world: &World, bodies: &[b3::BodyId], root: usize, anchor: Vec3) -> bool {
    let centre = |body: &b3::BodyId| {
        b3::body::body_is_valid(world, *body)
            .then(|| b3v_to_glam(body_get_world_center(world, *body)))
    };
    let Some(hub) = bodies.get(root).and_then(centre) else {
        return false;
    };
    bodies.iter().filter_map(centre).any(|at| {
        !at.is_finite() || at.distance(anchor) > MAX_STRAY || at.distance(hub) > MAX_STRETCH
    })
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

/// The entities of `skeleton`'s joints under `root`, in skeleton order.
fn collect_joints(
    root: Entity,
    skeleton: &Skeleton,
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
        if now - corpse.born >= corpse.life {
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
            let start = shot.start;
            let end = shot.trace.end;
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
                    && let Some(body) = corpse.bodies.get(index)
                {
                    shove(world, *body, direction * force, Some(closest));
                    if let Some(root) = corpse.bodies.get(corpse.root) {
                        shove(world, *root, direction * force * SHOT_BODY_SHOVE, None);
                    }
                }
            }
        }
    }
    for _ in 0..steps {
        world.step(STEP, SUB_STEPS);
        for (_, corpse, _) in &corpses {
            tame(world, &corpse.bodies);
        }
    }
    for (_, mut corpse, mut transform) in &mut corpses {
        if corpse.frozen {
            continue;
        }
        if has_run_away(world, &corpse.bodies, corpse.root, corpse.anchor) {
            tracing::warn!(player = ?corpse.player, "ragdoll: simulation diverged; freezing the corpse");
            for body in &corpse.bodies {
                if b3::body::body_is_valid(world, *body) {
                    b3::body::destroy_body(world, *body);
                }
            }
            corpse.frozen = true;
            continue;
        }
        pose(&corpse, world, &mut joints);
        transform.translation = corpse.origin;
        transform.rotation = Quat::IDENTITY;
        transform.scale = Vec3::ONE;
    }
}

/// Sets every joint's local transform from the simulated body that carries
/// it. Detail bones keep their death pose relative to their driver.
fn pose(ragdoll: &Ragdoll, world: &World, joints: &mut Query<&mut Transform, Without<Ragdoll>>) {
    let mut worlds: Vec<Option<(Vec3, Quat)>> = vec![None; ragdoll.joints.len()];
    for (index, joint) in ragdoll.joints.iter().enumerate() {
        let Some(driver) = ragdoll.drivers.get(index).copied() else {
            continue;
        };
        let Some(handle) = ragdoll.bodies.get(driver).copied() else {
            continue;
        };
        if !b3::body::body_is_valid(world, handle) {
            continue;
        }
        let transform_body = body_get_transform(world, handle);
        let body_rotation = coords::rotation_from_source(b3q_to_glam(transform_body.q)).normalize();
        let body_position =
            coords::position([transform_body.p.x, transform_body.p.y, transform_body.p.z]);
        let rest = ragdoll.rests.get(index).copied().unwrap_or_default();
        let position = body_position + body_rotation * rest.translation;
        let rotation = (body_rotation * rest.rotation).normalize();
        let (parent_position, parent_rotation) = match ragdoll.parents.get(index).copied().flatten()
        {
            Some(parent) => worlds
                .get(parent)
                .copied()
                .flatten()
                .unwrap_or((ragdoll.origin, Quat::IDENTITY)),
            None => (ragdoll.origin, Quat::IDENTITY),
        };
        let local_position = parent_rotation.inverse() * (position - parent_position);
        let local_rotation = parent_rotation.inverse() * rotation;
        if let Ok(mut transform) = joints.get_mut(*joint) {
            transform.translation = local_position;
            transform.rotation = local_rotation;
            transform.scale = Vec3::ONE;
        }
        if let Some(slot) = worlds.get_mut(index) {
            *slot = Some((position, rotation));
        }
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
    use b3::hull::make_box_hull;
    use b3::shape::create_hull_shape;
    use bevy::math::Vec3;

    /// A ValveBiped-like skeleton in Source space: +Z up, +X forward.
    fn human_skeleton() -> (Skeleton, Vec<Vec3>) {
        let specs: &[(&str, Option<usize>, [f32; 3])] = &[
            ("ValveBiped.Bip01_Pelvis", None, [0.0, 0.0, 62.0]),
            ("ValveBiped.Bip01_Spine", Some(0), [0.0, 0.0, 70.0]),
            ("ValveBiped.Bip01_Spine1", Some(1), [0.0, 0.0, 78.0]),
            ("ValveBiped.Bip01_Spine2", Some(2), [0.0, 0.0, 85.0]),
            ("ValveBiped.Bip01_Spine4", Some(3), [0.0, 0.0, 92.0]),
            ("ValveBiped.Bip01_Neck1", Some(4), [0.0, 0.0, 100.0]),
            ("ValveBiped.Bip01_Head1", Some(5), [0.0, 0.0, 107.0]),
            ("ValveBiped.Bip01_L_Clavicle", Some(4), [0.0, 6.0, 96.0]),
            ("ValveBiped.Bip01_L_UpperArm", Some(7), [4.0, 12.0, 94.0]),
            ("ValveBiped.Bip01_L_Forearm", Some(8), [8.0, 20.0, 80.0]),
            ("ValveBiped.Bip01_L_Hand", Some(9), [10.0, 24.0, 70.0]),
            ("ValveBiped.Bip01_L_Finger0", Some(10), [12.0, 25.0, 64.0]),
            ("ValveBiped.Bip01_R_Clavicle", Some(4), [0.0, -6.0, 96.0]),
            ("ValveBiped.Bip01_R_UpperArm", Some(12), [4.0, -12.0, 94.0]),
            ("ValveBiped.Bip01_R_Forearm", Some(13), [8.0, -20.0, 80.0]),
            ("ValveBiped.Bip01_R_Hand", Some(14), [10.0, -24.0, 70.0]),
            ("ValveBiped.Bip01_R_Finger0", Some(15), [12.0, -25.0, 64.0]),
            ("ValveBiped.Bip01_L_Thigh", Some(0), [1.0, 7.0, 58.0]),
            ("ValveBiped.Bip01_L_Calf", Some(17), [2.0, 8.0, 34.0]),
            ("ValveBiped.Bip01_L_Foot", Some(18), [2.0, 8.0, 10.0]),
            ("ValveBiped.Bip01_L_Toe0", Some(19), [7.0, 8.0, 5.0]),
            ("ValveBiped.Bip01_R_Thigh", Some(0), [1.0, -7.0, 58.0]),
            ("ValveBiped.Bip01_R_Calf", Some(21), [2.0, -8.0, 34.0]),
            ("ValveBiped.Bip01_R_Foot", Some(22), [2.0, -8.0, 10.0]),
            ("ValveBiped.Bip01_R_Toe0", Some(23), [7.0, -8.0, 5.0]),
        ];
        let joints = specs
            .iter()
            .map(|&(name, parent, _)| rl_assets::Joint {
                name: name.to_owned(),
                parent,
                rest: Transform::IDENTITY,
            })
            .collect();
        let positions = specs
            .iter()
            .map(|&(_, _, position)| Vec3::from(position))
            .collect();
        (Skeleton { joints }, positions)
    }

    fn phys_index(selection: &[(usize, BoneKind)], skeleton: &Skeleton, name: &str) -> usize {
        selection
            .iter()
            .position(|&(index, _)| skeleton.joints[index].name.eq_ignore_ascii_case(name))
            .unwrap()
    }

    #[test]
    fn global_poses_are_position_and_rotation_not_scale() {
        let global =
            GlobalTransform::from(Transform::from_xyz(1.0, 2.0, 3.0).with_scale(Vec3::splat(2.0)));
        let (position, rotation) = global_pose(&global);
        assert_eq!(position, Vec3::new(1.0, 2.0, 3.0));
        assert!(rotation.abs_diff_eq(Quat::IDENTITY, 1e-6));
    }

    #[test]
    fn bevy_positions_map_back_into_source_space() {
        let source = Vec3::new(128.0, -64.0, 72.0);
        let back = source_position(coords::position(source.to_array()));
        assert!(back.abs_diff_eq(source, 1e-3), "{back:?} != {source:?}");
        let up = source_position(Vec3::Y);
        assert!(
            up.z > 0.0 && up.y.abs() < 1e-4,
            "Bevy up is Source +Z, not +Y: {up:?}"
        );
    }

    #[test]
    fn valve_skeletons_get_a_compact_anatomical_rig() {
        let (skeleton, _) = human_skeleton();
        let parents: Vec<Option<usize>> =
            skeleton.joints.iter().map(|joint| joint.parent).collect();
        let selection = select_bones(&skeleton).unwrap();
        assert_eq!(
            selection.len(),
            17,
            "pelvis, two spine, neck, head, 6 arm and 6 leg bones"
        );
        assert!(selection.iter().all(|(index, _)| {
            let name = skeleton.joints[*index].name.to_ascii_lowercase();
            !name.contains("finger")
                && !name.contains("toe")
                && !name.contains("clavicle")
                && !name.contains("spine1")
                && !name.contains("spine2")
        }));
        let drivers = drivers_for(&selection, &parents);
        assert_eq!(
            drivers[10],
            phys_index(&selection, &skeleton, "ValveBiped.Bip01_L_Hand"),
            "fingers follow the hand"
        );
        assert_eq!(
            drivers[7],
            phys_index(&selection, &skeleton, "ValveBiped.Bip01_Spine4"),
            "clavicles follow the chest"
        );
        assert_eq!(
            drivers[20],
            phys_index(&selection, &skeleton, "ValveBiped.Bip01_L_Foot"),
            "toes follow the foot"
        );
    }

    /// A tuned world with a thick floor whose top is at z = 0.
    fn floor_world() -> World {
        rl_sim::physics::use_source_units();
        let mut def = b3::default_world_def();
        def.gravity = b3v(Vec3::new(0.0, 0.0, -9.81 * UNITS_PER_METRE));
        let mut world = World::new(&def);
        tune_world(&mut world);
        let mut ground_def = b3::default_body_def();
        ground_def.type_ = BodyType::Static;
        ground_def.position = b3v(Vec3::new(0.0, 0.0, -10.0));
        let ground = create_body(&mut world, &ground_def);
        let hull = make_box_hull(500.0, 500.0, 10.0);
        create_hull_shape(&mut world, ground, &b3::default_shape_def(), &hull.base);
        world
    }

    /// A human at `lift` units above the floor, tilted as a body in an
    /// animation is.
    fn spawn_human(world: &mut World, lift: f32, tilt: f32) -> (PhysRig, Vec<(usize, BoneKind)>) {
        let (skeleton, positions) = human_skeleton();
        let positions: Vec<Vec3> = positions
            .iter()
            .map(|position| *position + Vec3::Z * lift)
            .collect();
        let parents: Vec<Option<usize>> =
            skeleton.joints.iter().map(|joint| joint.parent).collect();
        let rotation = Quat::from_rotation_z(0.7) * Quat::from_rotation_x(tilt);
        let rotations = vec![rotation; skeleton.joints.len()];
        let selection = select_bones(&skeleton).unwrap();
        let rig = build_phys_rig(world, &positions, &rotations, &parents, &selection, Vec3::X);
        (rig, selection)
    }

    /// What a run of the simulation showed of a corpse.
    #[derive(Debug, Default)]
    struct Observed {
        top_speed: f32,
        top_spin: f32,
        worst_stretch: f32,
        lowest: f32,
    }

    /// Steps `world` for `seconds`, taming bodies as the game does, and
    /// records the extremes of the rig's motion.
    fn run(world: &mut World, rig: &PhysRig, seconds: f32) -> Observed {
        let steps = (seconds / STEP) as usize;
        let mut seen = Observed {
            lowest: f32::MAX,
            ..Observed::default()
        };
        for _ in 0..steps {
            world.step(STEP, SUB_STEPS);
            tame(world, &rig.bodies);
            for (index, body) in rig.bodies.iter().enumerate() {
                let speed = b3v_to_glam(b3::body::body_get_linear_velocity(world, *body)).length();
                let spin = b3v_to_glam(b3::body::body_get_angular_velocity(world, *body)).length();
                let at = b3v_to_glam(body_get_world_center(world, *body));
                let hub = b3v_to_glam(body_get_world_center(world, rig.bodies[rig.root]));
                seen.top_speed = seen.top_speed.max(speed);
                seen.top_spin = seen.top_spin.max(spin);
                seen.worst_stretch = seen.worst_stretch.max(at.distance(hub));
                seen.lowest = seen.lowest.min(at.z);
                assert!(at.is_finite(), "bone {index} went non-finite");
            }
        }
        seen
    }

    fn hand_of(rig: &PhysRig, selection: &[(usize, BoneKind)]) -> b3::BodyId {
        let hand = selection
            .iter()
            .position(|(_, kind)| *kind == BoneKind::Hand)
            .unwrap();
        rig.bodies[hand]
    }

    fn awake_count(world: &World, rig: &PhysRig) -> usize {
        rig.bodies
            .iter()
            .filter(|body| b3::body::body_is_awake(world, **body))
            .count()
    }

    #[test]
    fn no_bone_is_a_featherweight_next_to_its_neighbours() {
        let mut world = floor_world();
        let (rig, selection) = spawn_human(&mut world, 5.0, 0.0);
        let masses: Vec<f32> = rig
            .bodies
            .iter()
            .map(|body| body_get_mass(&world, *body))
            .collect();
        let heaviest = masses.iter().copied().fold(0.0, f32::max);
        for (&(joint, kind), mass) in selection.iter().zip(&masses) {
            assert!(
                *mass > 0.5,
                "{kind:?} (joint {joint}) weighs {mass} kg, too little to be stable"
            );
            assert!(
                heaviest / mass < 25.0,
                "{kind:?} (joint {joint}) is {} times lighter than the heaviest bone",
                heaviest / mass
            );
        }
        let total: f32 = masses.iter().sum();
        assert!(
            (45.0..110.0).contains(&total),
            "a person should weigh about 70 kg, not {total}"
        );
    }

    #[test]
    fn a_hard_hit_on_a_light_hand_cannot_fling_it() {
        let mut world = floor_world();
        let (rig, selection) = spawn_human(&mut world, 2.0, 0.2);
        let hand = hand_of(&rig, &selection);
        // The most any weapon can ask for, at the hand's very edge.
        let at = b3v_to_glam(body_get_world_center(&world, hand)) + Vec3::Y * 2.0;
        shove(
            &mut world,
            hand,
            Vec3::X * MAX_SHOT_IMPULSE * 10.0,
            Some(at),
        );
        let speed = b3v_to_glam(b3::body::body_get_linear_velocity(&world, hand)).length();
        let spin = b3v_to_glam(b3::body::body_get_angular_velocity(&world, hand)).length();
        assert!(
            speed <= MAX_SHOT_SPEED * 1.01,
            "the hand was thrown at {speed} units/s"
        );
        assert!(
            spin <= MAX_BONE_SPIN * 1.01,
            "the hand spun at {spin} rad/s"
        );
        let seen = run(&mut world, &rig, 6.0);
        assert!(seen.top_speed < MAX_BONE_SPEED, "{seen:?}");
        assert!(
            seen.worst_stretch < 1.6 * UNITS_PER_METRE,
            "the body was torn apart: {seen:?}"
        );
        assert!(seen.lowest > -2.0, "fell through the floor: {seen:?}");
    }

    #[test]
    fn bones_collide_with_the_body_except_where_they_touch_by_design() {
        let (skeleton, positions) = human_skeleton();
        let parents: Vec<Option<usize>> =
            skeleton.joints.iter().map(|joint| joint.parent).collect();
        let selection = select_bones(&skeleton).unwrap();
        let mut phys_of: Vec<Option<usize>> = vec![None; parents.len()];
        for (phys, &(joint, _)) in selection.iter().enumerate() {
            phys_of[joint] = Some(phys);
        }
        let kind_at: Vec<BoneKind> = (0..parents.len())
            .map(|index| phys_of[index].map_or(BoneKind::Other, |phys| selection[phys].1))
            .collect();
        let phys_parents: Vec<Option<usize>> = selection
            .iter()
            .map(|&(joint, _)| nearest_selected_ancestor(joint, &parents, &phys_of))
            .collect();
        let segments: Vec<(Vec3, Vec3, f32)> = selection
            .iter()
            .map(|&(joint, kind)| {
                let tail = bone_tail(joint, kind, &kind_at, &phys_of, &positions, &parents);
                (positions[joint], tail, kind.radius() * UNITS_PER_METRE)
            })
            .collect();
        let excluded = self_collision_exclusions(&segments, &phys_parents);
        let skip = |a: &str, b: &str| {
            let (a, b) = (
                phys_index(&selection, &skeleton, a),
                phys_index(&selection, &skeleton, b),
            );
            excluded.contains(&(a.min(b), a.max(b)))
        };
        assert!(skip(
            "ValveBiped.Bip01_L_Forearm",
            "ValveBiped.Bip01_L_UpperArm"
        ));
        assert!(skip("ValveBiped.Bip01_Head1", "ValveBiped.Bip01_Neck1"));
        assert!(
            !skip("ValveBiped.Bip01_L_Hand", "ValveBiped.Bip01_R_Hand"),
            "the hands may meet"
        );
        assert!(
            !skip("ValveBiped.Bip01_L_Hand", "ValveBiped.Bip01_L_Thigh"),
            "a hand may lie on the thigh"
        );
        assert!(
            !skip("ValveBiped.Bip01_L_Forearm", "ValveBiped.Bip01_Spine4"),
            "an arm may fold over the chest"
        );
        assert!(
            excluded.len() < segments.len() * 3,
            "too much of the body was exempted: {excluded:?}"
        );
    }

    #[test]
    fn segments_know_how_far_apart_they_are() {
        let x = |x| Vec3::new(x, 0.0, 0.0);
        let distance = segment_distance(x(0.0), x(1.0), x(3.0), x(4.0));
        assert!((distance - 2.0).abs() < 1e-5);
        let crossing = segment_distance(
            x(0.0),
            x(2.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(1.0, 3.0, 0.0),
        );
        assert!((crossing - 1.0).abs() < 1e-5, "{crossing}");
        let point = segment_distance(
            x(1.0),
            x(1.0),
            Vec3::new(1.0, 0.0, 5.0),
            Vec3::new(1.0, 0.0, 5.0),
        );
        assert!((point - 5.0).abs() < 1e-5);
        let parallel = segment_distance(
            x(0.0),
            x(2.0),
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::new(2.0, 3.0, 0.0),
        );
        assert!((parallel - 3.0).abs() < 1e-5);
    }

    #[test]
    fn limited_impulses_depend_on_mass_not_only_size() {
        let impulse = Vec3::new(300.0, 0.0, 400.0);
        let light = limited_impulse(impulse, 1.0, 100.0);
        assert!((light.length() - 100.0).abs() < 1e-3, "{light:?}");
        assert!(light.normalize().abs_diff_eq(impulse.normalize(), 1e-5));
        let heavy = limited_impulse(impulse, 50.0, 100.0);
        assert_eq!(heavy, impulse, "a heavy body takes it whole");
        assert_eq!(limited_impulse(Vec3::NAN, 5.0, 100.0), Vec3::ZERO);
    }

    #[test]
    fn a_blast_throws_a_body_without_tearing_it() {
        let mut world = floor_world();
        let (rig, _) = spawn_human(&mut world, 2.0, 0.1);
        for body in &rig.bodies {
            let mass = body_get_mass(&world, *body);
            let direction = (Vec3::X + Vec3::Z * 0.35).normalize();
            shove(&mut world, *body, direction * mass * BLAST_SPEED, None);
        }
        let seen = run(&mut world, &rig, 8.0);
        assert!(seen.top_speed <= MAX_BONE_SPEED * 1.01, "{seen:?}");
        assert!(seen.top_spin <= MAX_BONE_SPIN * 1.05, "{seen:?}");
        assert!(
            seen.worst_stretch < 1.6 * UNITS_PER_METRE,
            "the body was torn apart: {seen:?}"
        );
        assert!(seen.lowest > -2.0, "fell through the floor: {seen:?}");
        assert_eq!(awake_count(&world, &rig), 0, "it should come to rest");
    }

    #[test]
    fn a_fall_from_height_lands_and_stays_put() {
        let mut world = floor_world();
        let (rig, _) = spawn_human(&mut world, 400.0, 0.3);
        let seen = run(&mut world, &rig, 10.0);
        assert!(seen.top_speed <= MAX_BONE_SPEED * 1.01, "{seen:?}");
        assert!(seen.worst_stretch < 1.8 * UNITS_PER_METRE, "{seen:?}");
        assert!(seen.lowest > -2.0, "fell through the floor: {seen:?}");
        assert_eq!(awake_count(&world, &rig), 0, "it should come to rest");
    }

    #[test]
    fn a_standing_corpse_does_not_collapse_into_a_heap_or_stay_stiff() {
        let mut world = floor_world();
        let (rig, selection) = spawn_human(&mut world, 1.0, 0.0);
        let head = selection
            .iter()
            .position(|(_, kind)| *kind == BoneKind::Head)
            .unwrap();
        let start = b3v_to_glam(body_get_world_center(&world, rig.bodies[head])).z;
        run(&mut world, &rig, 6.0);
        let end = b3v_to_glam(body_get_world_center(&world, rig.bodies[head])).z;
        // It must fall (it is dead) but a body is not a puddle: the head ends
        // above the floor by more than a bone radius.
        assert!(
            end < start - 10.0,
            "a dead body should go down: {start} -> {end}"
        );
        assert!(
            end > BoneKind::Head.radius() * UNITS_PER_METRE * 0.9,
            "the head went through the floor: {end}"
        );
    }

    #[test]
    fn the_same_death_always_plays_out_the_same_way() {
        let play = || {
            let mut world = floor_world();
            let (rig, selection) = spawn_human(&mut world, 30.0, 0.25);
            let hand = hand_of(&rig, &selection);
            shove(
                &mut world,
                hand,
                Vec3::new(500.0, 100.0, 50.0),
                Some(Vec3::new(10.0, 24.0, 100.0)),
            );
            run(&mut world, &rig, 4.0);
            rig.bodies
                .iter()
                .map(|body| b3v_to_glam(body_get_transform(&world, *body).p))
                .collect::<Vec<_>>()
        };
        assert_eq!(play(), play());
    }

    #[test]
    fn a_corpse_that_diverges_is_noticed() {
        let mut world = floor_world();
        let (rig, _) = spawn_human(&mut world, 2.0, 0.0);
        let anchor = rig.positions.iter().copied().sum::<Vec3>() / rig.positions.len() as f32;
        assert!(!has_run_away(&world, &rig.bodies, rig.root, anchor));
        // Tear a hand off the body.
        let far = rig.bodies[rig.bodies.len() - 1];
        b3::body::body_set_transform(
            &mut world,
            far,
            b3v(anchor + Vec3::Z * 5.0 * UNITS_PER_METRE),
            b3q(Quat::IDENTITY),
        );
        assert!(has_run_away(&world, &rig.bodies, rig.root, anchor));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_human_rig_falls_over_without_exploding() {
        rl_sim::physics::use_source_units();
        let mut def = b3::default_world_def();
        def.gravity = b3v(Vec3::new(0.0, 0.0, -9.81 * UNITS_PER_METRE));
        let mut world = World::new(&def);
        let mut ground_def = b3::default_body_def();
        ground_def.type_ = BodyType::Static;
        ground_def.position = b3v(Vec3::new(0.0, 0.0, -10.0));
        let ground = create_body(&mut world, &ground_def);
        let shape_def = b3::default_shape_def();
        let hull = make_box_hull(500.0, 500.0, 10.0);
        create_hull_shape(&mut world, ground, &shape_def, &hull.base);

        let (skeleton, positions) = human_skeleton();
        let parents: Vec<Option<usize>> =
            skeleton.joints.iter().map(|joint| joint.parent).collect();
        // Bodies in a real skeleton are not axis-aligned: exercise the
        // world-to-body-local work the capsule needs.
        let rotation = Quat::from_rotation_z(0.7) * Quat::from_rotation_x(0.2);
        let rotations = vec![rotation; skeleton.joints.len()];
        let selection = select_bones(&skeleton).unwrap();
        let rig = build_phys_rig(
            &mut world,
            &positions,
            &rotations,
            &parents,
            &selection,
            Vec3::X,
        );
        let mut phys_of: Vec<Option<usize>> = vec![None; parents.len()];
        for (phys, &(joint, _)) in selection.iter().enumerate() {
            phys_of[joint] = Some(phys);
        }
        // The collider must lie along the bone in the world, even though the
        // capsule is built in the body's local frame.
        let kind_at: Vec<BoneKind> = (0..parents.len())
            .map(|index| phys_of[index].map_or(BoneKind::Other, |phys| selection[phys].1))
            .collect();
        for (phys, &(joint, kind)) in selection.iter().enumerate() {
            let tail = bone_tail(joint, kind, &kind_at, &phys_of, &positions, &parents);
            if tail.distance(positions[joint]) < 0.05 * UNITS_PER_METRE {
                continue;
            }
            let center = b3v_to_glam(body_get_world_center(&world, rig.bodies[phys]));
            let along = (center - positions[joint]).normalize_or_zero();
            let bone = (tail - positions[joint]).normalize_or_zero();
            assert!(
                along.dot(bone) > 0.99,
                "body {phys}'s collider is not along its bone: {along:?} vs {bone:?}"
            );
        }
        let phys_parents: Vec<Option<usize>> = selection
            .iter()
            .map(|&(joint, _)| nearest_selected_ancestor(joint, &parents, &phys_of))
            .collect();
        let initial: Vec<f32> = phys_parents
            .iter()
            .enumerate()
            .map(|(phys, parent)| {
                parent.map_or(0.0, |parent| {
                    rig.positions[phys].distance(rig.positions[parent])
                })
            })
            .collect();
        for _ in 0..60 {
            world.step(STEP, SUB_STEPS);
        }
        // A killing hit must not tear the rig apart.
        if let Some(hand) = selection
            .iter()
            .position(|(_, kind)| *kind == BoneKind::Hand)
        {
            body_apply_linear_impulse(
                &mut world,
                rig.bodies[hand],
                b3v(Vec3::new(900.0, 300.0, 200.0)),
                b3v(rig.positions[hand]),
                true,
            );
        }
        for _ in 0..240 {
            world.step(STEP, SUB_STEPS);
        }
        let center =
            rig.positions.iter().fold(Vec3::ZERO, |sum, p| sum + *p) / rig.positions.len() as f32;
        let settled: Vec<Vec3> = rig
            .bodies
            .iter()
            .map(|body| b3v_to_glam(body_get_transform(&world, *body).p))
            .collect();
        for (index, position) in settled.iter().enumerate() {
            assert!(
                position.is_finite(),
                "bone {index} went non-finite: {position:?}"
            );
            assert!(
                position.z > -40.0,
                "bone {index} fell through the ground: {position:?}"
            );
            assert!(
                position.distance(center) < 400.0,
                "bone {index} flew away from the body: {position:?} (centre {center:?})"
            );
        }
        for (phys, parent) in phys_parents.iter().enumerate() {
            let Some(parent) = parent else {
                continue;
            };
            let distance = settled[phys].distance(settled[*parent]);
            assert!(
                (distance - initial[phys]).abs() < 5.0,
                "joint {phys} pulled apart: {distance} vs {}",
                initial[phys]
            );
        }
        let awake = rig
            .bodies
            .iter()
            .filter(|body| b3::body::body_is_awake(&world, **body))
            .count();
        assert_eq!(awake, 0, "the corpse should come to rest, not ring");
    }
}
