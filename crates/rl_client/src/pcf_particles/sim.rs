//! The particle simulation: a system's operators run on a list of particles.
//!
//! This is the half of the PCF runtime that knows nothing of Bevy: it takes
//! [`ParticleSystem`] definitions, steps them in time and hands back what to
//! draw. Everything is in Source units (inches, Z up), like the files.
//!
//! Each step follows the engine's order: emitters create particles and the
//! initializers shape them, then forces push, operators move and fade, and
//! constraints (collision, planes) have the last word. Only the operators
//! HL2DM's own `.pcf` files use are implemented (see the module tests for the
//! list); one the files use but this does not know is skipped and logged once.
//!
//! Approximations, all of them visual: noise is value noise, not Perlin;
//! `Movement Basic` drag is applied per 1/30 s tick; operator fade windows
//! (`operator start fadein` and friends) scale emitters only; positions on
//! models and bones fall back to the control point.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::math::{Mat3, Vec3};
use rl_sim::rng::Rng;
use source_formats::pcf::{Operator, ParticleSystem, Value};

/// How many particles one system may hold when its file does not say.
const DEFAULT_MAX_PARTICLES: usize = 1000;
/// The frames a sequence is assumed to have without sheet data.
const DEFAULT_SEQUENCE_FRAMES: f32 = 30.0;
/// `Movement Basic`'s drag is a loss per tick of this length, seconds.
const DRAG_TICK: f32 = 1.0 / 30.0;
/// The longest step simulated at once, seconds; longer is split.
const MAX_STEP: f32 = 0.1;

// Attribute (field) numbers operators name their targets by.
const FIELD_XYZ: i32 = 0;
const FIELD_LIFE: i32 = 1;
const FIELD_RADIUS: i32 = 3;
const FIELD_ROTATION: i32 = 4;
const FIELD_ROTATION_SPEED: i32 = 5;
const FIELD_TINT: i32 = 6;
const FIELD_ALPHA: i32 = 7;
const FIELD_CREATION_TIME: i32 = 8;
const FIELD_SEQUENCE: i32 = 9;
const FIELD_TRAIL_LENGTH: i32 = 10;
const FIELD_YAW: i32 = 12;

/// A point the game gives a system to anchor to: where it is and which way
/// it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlPoint {
    /// Where, Source units.
    pub position: Vec3,
    /// Its forward axis (local X).
    pub forward: Vec3,
    /// Its right axis (local Y).
    pub right: Vec3,
    /// Its up axis (local Z).
    pub up: Vec3,
    /// Where it was last step, to find how far it moved.
    previous: Vec3,
    /// How fast it moves, units/s.
    velocity: Vec3,
}

impl ControlPoint {
    /// A control point at `position` facing along world X, Y, Z.
    #[must_use]
    pub fn at(position: Vec3) -> Self {
        Self {
            position,
            forward: Vec3::X,
            right: Vec3::Y,
            up: Vec3::Z,
            previous: position,
            velocity: Vec3::ZERO,
        }
    }

    /// A control point at `position` whose local Z axis is `up` (a surface
    /// normal, say); the other axes are any pair perpendicular to it.
    #[must_use]
    pub fn facing(position: Vec3, up: Vec3) -> Self {
        let up = up.try_normalize().unwrap_or(Vec3::Z);
        let helper = if up.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
        let right = up.cross(helper).normalize();
        let forward = right.cross(up);
        Self {
            forward,
            right,
            up,
            ..Self::at(position)
        }
    }

    /// Moves it to `position`, keeping its orientation.
    pub fn move_to(&mut self, position: Vec3) {
        self.position = position;
    }

    fn basis(self) -> Mat3 {
        Mat3::from_cols(self.forward, self.right, self.up)
    }

    fn to_world(self, local: Vec3) -> Vec3 {
        self.basis() * local
    }
}

/// Where a moving particle hit the map.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// The point of impact.
    pub point: Vec3,
    /// The surface's unit normal.
    pub normal: Vec3,
}

/// Answers whether a particle's step crossed solid map.
pub trait Collider {
    /// The first hit along `from` → `to`, in Source units.
    fn trace(&self, from: Vec3, to: Vec3) -> Option<Hit>;
}

/// A world with nothing in it.
#[derive(Debug, Clone, Copy, Default)]
pub struct EmptyWorld;

impl Collider for EmptyWorld {
    fn trace(&self, _from: Vec3, _to: Vec3) -> Option<Hit> {
        None
    }
}

/// A floor: everything below the plane `z = height` is solid.
#[derive(Debug, Clone, Copy)]
pub struct Floor(pub f32);

impl Collider for Floor {
    fn trace(&self, from: Vec3, to: Vec3) -> Option<Hit> {
        if from.z < self.0 || to.z >= self.0 {
            return None;
        }
        let t = (from.z - self.0) / (from.z - to.z);
        Some(Hit {
            point: from.lerp(to, t),
            normal: Vec3::Z,
        })
    }
}

// ---------------------------------------------------------------------------
// Definitions

/// How a system's particles are drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum Renderer {
    /// A camera-facing quad that steps through the sprite sheet.
    AnimatedSprite {
        /// Frames per second of the sheet, or per lifetime if `fit_lifetime`.
        rate: f32,
        /// Whether the animation spans the particle's life.
        fit_lifetime: bool,
        /// `0` faces the camera; others align to a control point's axes.
        orientation: i32,
    },
    /// A streak along the particle's motion.
    SpriteTrail {
        /// Shortest streak, units.
        min_length: f32,
        /// Longest streak, units.
        max_length: f32,
        /// Seconds of motion the streak spans, scaled by each particle's trail length.
        length_fade_in: f32,
    },
    /// A ribbon through the particles in order.
    Rope {
        /// Segments between two particles.
        subdivisions: u32,
        /// Units of ribbon per texture repeat.
        texel_size: f32,
        /// Texture scroll, repeats per second.
        scroll_rate: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emitter {
    Continuously,
    Instantaneously,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Init {
    RadiusRandom,
    AlphaRandom,
    ColorRandom,
    LifetimeRandom,
    RotationRandom,
    RotationYawRandom,
    RotationYawFlip,
    SequenceRandom,
    SequenceTwoRandom,
    TrailLengthRandom,
    LifetimeFromSequence,
    LifetimePreAgeNoise,
    PositionSphere,
    PositionBox,
    PositionOffset,
    PositionWarp,
    PositionAlongPath,
    PositionInHierarchy,
    PositionOnModel,
    VelocityNoise,
    VelocityInherit,
    RemapScalar,
    RemapNoise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Movement,
    LifespanDecay,
    AlphaFadeIn,
    AlphaFadeOut,
    AlphaFadeAndDecay,
    ColorFade,
    RadiusScale,
    SpinRoll,
    OscillateScalar,
    OscillateVector,
    LockToControlPoint,
    DampenToControlPoint,
    NoiseVector,
    RemapDistance,
    CullPlane,
    CullRandom,
    SetChildControlPoints,
    /// Needs a skeleton; the particle just keeps its own motion.
    Inert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Force {
    Random,
    Twist,
    PullToControlPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Constraint {
    Collision,
    Plane,
    DistanceToControlPoint,
}

#[derive(Debug, Clone)]
struct Step<K> {
    kind: K,
    op: Operator,
}

impl<K: Copy> Step<K> {
    fn float(&self, key: &str, default: f32) -> f32 {
        self.op.float(key, default)
    }

    fn int(&self, key: &str, default: i32) -> i32 {
        match self.op.attributes.get(key) {
            Some(Value::Int(n)) => *n,
            Some(other) => other.as_f32().map_or(default, |f| f as i32),
            None => default,
        }
    }

    fn flag(&self, key: &str, default: bool) -> bool {
        match self.op.attributes.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Int(n)) => *n != 0,
            _ => default,
        }
    }

    fn vec3(&self, key: &str, default: Vec3) -> Vec3 {
        match self.op.attributes.get(key) {
            Some(Value::Vec(v)) if v.len() >= 3 => Vec3::new(v[0], v[1], v[2]),
            _ => default,
        }
    }

    fn color(&self, key: &str, default: [f32; 3]) -> [f32; 3] {
        match self.op.attributes.get(key) {
            Some(Value::Color(c)) => [
                f32::from(c[0]) / 255.0,
                f32::from(c[1]) / 255.0,
                f32::from(c[2]) / 255.0,
            ],
            _ => default,
        }
    }

    /// The `[min, max]` pair of settings `min_key`/`max_key`.
    fn range(&self, min_key: &str, max_key: &str, min: f32, max: f32) -> (f32, f32) {
        (self.float(min_key, min), self.float(max_key, max))
    }

    /// How strongly the operator acts at system time `time`, `0..=1`,
    /// from its fade windows.
    fn strength(&self, time: f32) -> f32 {
        let start_in = self.float("operator start fadein", 0.0);
        let end_in = self.float("operator end fadein", 0.0);
        let start_out = self.float("operator start fadeout", 0.0);
        let end_out = self.float("operator end fadeout", 0.0);
        let mut s = 1.0;
        if end_in > start_in && time < end_in {
            s *= ramp(time, start_in, end_in);
        }
        if end_out > start_out && time > start_out {
            s *= 1.0 - ramp(time, start_out, end_out);
        }
        s
    }
}

/// A system ready to run: its operators decoded.
#[derive(Debug, Clone)]
pub struct SystemDef {
    name: String,
    material: String,
    max_particles: usize,
    initial_particles: usize,
    radius: f32,
    color: [f32; 4],
    sequence: i32,
    emitters: Vec<Step<Emitter>>,
    initializers: Vec<Step<Init>>,
    operators: Vec<Step<Op>>,
    forces: Vec<Step<Force>>,
    constraints: Vec<Step<Constraint>>,
    renderers: Vec<Renderer>,
    children: Vec<(String, f32)>,
}

impl SystemDef {
    /// Decodes `system`. Operators this runtime does not know are dropped
    /// and listed in the returned names.
    #[must_use]
    pub fn compile(system: &ParticleSystem) -> (Self, Vec<String>) {
        let mut unknown = Vec::new();
        let emitters = steps(&mut unknown, &system.emitters, emitter_kind);
        let initializers = steps(&mut unknown, &system.initializers, init_kind);
        let operators = steps(&mut unknown, &system.operators, op_kind);
        let forces = steps(&mut unknown, &system.forces, force_kind);
        let constraints = steps(&mut unknown, &system.constraints, constraint_kind);
        let renderers = system
            .renderers
            .iter()
            .filter_map(|op| {
                let step = Step {
                    kind: (),
                    op: op.clone(),
                };
                match op.function.as_str() {
                    "render_animated_sprites" => Some(Renderer::AnimatedSprite {
                        rate: step.float("animation rate", 1.0),
                        fit_lifetime: step.flag("animation_fit_lifetime", false),
                        orientation: step.int("orientation_type", 0),
                    }),
                    "render_sprite_trail" => Some(Renderer::SpriteTrail {
                        min_length: step.float("min length", 0.0),
                        max_length: step.float("max length", 2000.0),
                        length_fade_in: step.float("length fade in time", 0.0),
                    }),
                    "render_rope" => Some(Renderer::Rope {
                        subdivisions: step.int("subdivision_count", 3).max(1) as u32,
                        texel_size: step.float("texel_size", 4.0).max(0.01),
                        scroll_rate: step.float("texture_scroll_rate", 0.0),
                    }),
                    other => {
                        unknown.push(other.to_owned());
                        None
                    }
                }
            })
            .collect();
        let c = system.color;
        let def = Self {
            name: system.name.clone(),
            material: system.material.clone(),
            max_particles: if system.max_particles == 0 {
                DEFAULT_MAX_PARTICLES
            } else {
                system.max_particles as usize
            },
            initial_particles: system.initial_particles as usize,
            radius: system.radius,
            color: [
                f32::from(c[0]) / 255.0,
                f32::from(c[1]) / 255.0,
                f32::from(c[2]) / 255.0,
                f32::from(c[3]) / 255.0,
            ],
            sequence: system.sequence,
            emitters,
            initializers,
            operators,
            forces,
            constraints,
            renderers,
            children: system
                .children
                .iter()
                .map(|child| (child.name.clone(), child.delay))
                .collect(),
        };
        (def, unknown)
    }

    /// The system's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The material path its particles are drawn with.
    #[must_use]
    pub fn material(&self) -> &str {
        &self.material
    }

    /// How its particles are drawn.
    #[must_use]
    pub fn renderers(&self) -> &[Renderer] {
        &self.renderers
    }
}

/// Decodes `ops` with `kind`, noting the names it does not know.
fn steps<K>(
    unknown: &mut Vec<String>,
    ops: &[Operator],
    kind: fn(&str) -> Option<K>,
) -> Vec<Step<K>> {
    ops.iter()
        .filter_map(|op| {
            if let Some(k) = kind(&op.function) {
                Some(Step {
                    kind: k,
                    op: op.clone(),
                })
            } else {
                unknown.push(op.function.clone());
                None
            }
        })
        .collect()
}

fn emitter_kind(name: &str) -> Option<Emitter> {
    Some(match name {
        "emit_continuously" => Emitter::Continuously,
        "emit_instantaneously" => Emitter::Instantaneously,
        _ => return None,
    })
}

fn init_kind(name: &str) -> Option<Init> {
    Some(match name {
        "Radius Random" => Init::RadiusRandom,
        "Alpha Random" => Init::AlphaRandom,
        "Color Random" => Init::ColorRandom,
        "Lifetime Random" => Init::LifetimeRandom,
        "Rotation Random" => Init::RotationRandom,
        "Rotation Yaw Random" => Init::RotationYawRandom,
        "Rotation Yaw Flip Random" => Init::RotationYawFlip,
        "Sequence Random" => Init::SequenceRandom,
        "Sequence Two Random" => Init::SequenceTwoRandom,
        "Trail Length Random" => Init::TrailLengthRandom,
        "lifetime from sequence" => Init::LifetimeFromSequence,
        "Lifetime Pre-Age Noise" => Init::LifetimePreAgeNoise,
        "Position Within Sphere Random" => Init::PositionSphere,
        "Position Within Box Random" => Init::PositionBox,
        "Position Modify Offset Random" => Init::PositionOffset,
        "Position Modify Warp Random" => Init::PositionWarp,
        "Position Along Path Sequential" => Init::PositionAlongPath,
        "Position In CP Hierarchy" => Init::PositionInHierarchy,
        "Position on Model Random" => Init::PositionOnModel,
        "Velocity Noise" => Init::VelocityNoise,
        "Velocity Inherit from Control Point" => Init::VelocityInherit,
        "remap initial scalar" => Init::RemapScalar,
        "Remap Noise to Scalar" => Init::RemapNoise,
        _ => return None,
    })
}

fn op_kind(name: &str) -> Option<Op> {
    Some(match name {
        "Movement Basic" => Op::Movement,
        "Lifespan Decay" => Op::LifespanDecay,
        "Alpha Fade In Random" => Op::AlphaFadeIn,
        "Alpha Fade Out Random" => Op::AlphaFadeOut,
        "Alpha Fade and Decay" => Op::AlphaFadeAndDecay,
        "Color Fade" => Op::ColorFade,
        "Radius Scale" => Op::RadiusScale,
        "Rotation Spin Roll" => Op::SpinRoll,
        "Oscillate Scalar" => Op::OscillateScalar,
        "Oscillate Vector" => Op::OscillateVector,
        "Movement Lock to Control Point" => Op::LockToControlPoint,
        "Movement Dampen Relative to Control Point" => Op::DampenToControlPoint,
        "Noise Vector" => Op::NoiseVector,
        "Remap Distance to Control Point to Scalar" => Op::RemapDistance,
        "Cull when crossing plane" => Op::CullPlane,
        "Cull Random" => Op::CullRandom,
        "Set child control points from particle positions" => Op::SetChildControlPoints,
        "Movement Lock to Bone" => Op::Inert,
        _ => return None,
    })
}

fn force_kind(name: &str) -> Option<Force> {
    Some(match name {
        "random force" => Force::Random,
        "twist around axis" => Force::Twist,
        "Pull towards control point" => Force::PullToControlPoint,
        _ => return None,
    })
}

fn constraint_kind(name: &str) -> Option<Constraint> {
    Some(match name {
        "Collision via traces" => Constraint::Collision,
        "Prevent passing through a plane" => Constraint::Plane,
        "Constrain distance to control point" => Constraint::DistanceToControlPoint,
        _ => return None,
    })
}

/// Every system the game defines, by name.
#[derive(Debug, Clone, Default)]
pub struct Library {
    systems: HashMap<String, Arc<SystemDef>>,
}

impl Library {
    /// An empty library.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `systems`, replacing any of the same name. Returns the names of
    /// operators that were skipped because they are not implemented.
    pub fn add(&mut self, systems: &[ParticleSystem]) -> Vec<String> {
        let mut unknown = Vec::new();
        for system in systems {
            let (def, skipped) = SystemDef::compile(system);
            unknown.extend(skipped);
            self.systems.insert(def.name.clone(), Arc::new(def));
        }
        unknown.sort();
        unknown.dedup();
        unknown
    }

    /// The system called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<SystemDef>> {
        self.systems.get(name)
    }

    /// How many systems there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.systems.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.systems.is_empty()
    }

    /// Starts the system `name` with the given control points.
    #[must_use]
    pub fn spawn(&self, name: &str, points: &[ControlPoint], seed: u64) -> Option<Instance> {
        Instance::new(self, name, points, seed)
    }
}

// ---------------------------------------------------------------------------
// Running systems

/// One live particle.
#[derive(Debug, Clone)]
pub struct Particle {
    /// Where, Source units.
    pub position: Vec3,
    /// Velocity, units/s.
    pub velocity: Vec3,
    /// Seconds lived.
    pub age: f32,
    /// Seconds it lives.
    pub life: f32,
    /// Half-size, units.
    pub radius: f32,
    /// Roll, radians.
    pub rotation: f32,
    /// Roll speed, radians/s.
    pub rotation_speed: f32,
    /// Yaw, degrees (rope and oriented sprites).
    pub yaw: f32,
    /// Colour, `0..=1` (may exceed 1).
    pub color: [f32; 3],
    /// Opacity, `0..=1`.
    pub alpha: f32,
    /// The sprite-sheet sequence.
    pub sequence: i32,
    /// A second sequence blended in by some sheets.
    pub sequence2: i32,
    /// Trail length scale.
    pub trail_length: f32,
    /// Creation index, a stable identity.
    pub id: u32,
    radius0: f32,
    color0: [f32; 3],
    alpha0: f32,
    /// Where the control point it follows was last step.
    anchor: Vec3,
    dead: bool,
}

impl Particle {
    /// How far through its life, `0..=1`.
    #[must_use]
    pub fn life_fraction(&self) -> f32 {
        if self.life <= 0.0 {
            1.0
        } else {
            (self.age / self.life).clamp(0.0, 1.0)
        }
    }

    /// A number in `0..1` that is the same every time it is asked for
    /// `salt` for this particle.
    fn random(&self, salt: u32) -> f32 {
        hash01(self.id, salt)
    }
}

/// A running system: its particles, children and state.
#[derive(Debug)]
pub struct Instance {
    def: Arc<SystemDef>,
    /// Where the game has anchored it. Children share these.
    pub points: Vec<ControlPoint>,
    /// How many of `points` the game supplied.
    given: usize,
    particles: Vec<Particle>,
    age: f32,
    /// Emission owed from the last step, in particles.
    carry: Vec<f32>,
    /// Whether each instantaneous emitter has fired.
    fired: Vec<bool>,
    created: u32,
    rng: Rng,
    /// Frames per sequence of the material's sheet.
    sheet_frames: Vec<f32>,
    /// Children not yet started: remaining delay and the definition.
    waiting: Vec<(f32, Arc<SystemDef>)>,
    children: Vec<Instance>,
    stopped: bool,
}

impl Instance {
    fn new(library: &Library, name: &str, points: &[ControlPoint], seed: u64) -> Option<Self> {
        let def = library.get(name)?.clone();
        let mut points = points.to_vec();
        if points.is_empty() {
            points.push(ControlPoint::at(Vec3::ZERO));
        }
        let waiting = def
            .children
            .iter()
            .filter_map(|(child, delay)| Some((*delay, library.get(child)?.clone())))
            .collect();
        let mut instance = Self {
            given: points.len(),
            carry: vec![0.0; def.emitters.len()],
            fired: vec![false; def.emitters.len()],
            def,
            points,
            particles: Vec::new(),
            age: 0.0,
            created: 0,
            rng: Rng::seeded(&[seed]),
            sheet_frames: Vec::new(),
            waiting,
            children: Vec::new(),
            stopped: false,
        };
        for _ in 0..instance.def.initial_particles {
            instance.create(0.0);
        }
        // A zero-delay child starts with its parent.
        instance.start_children(library, seed);
        Some(instance)
    }

    /// Tells the system how many frames each sheet sequence has.
    pub fn set_sheet(&mut self, frames: Vec<f32>) {
        self.sheet_frames = frames;
    }

    /// The system it runs.
    #[must_use]
    pub fn def(&self) -> &Arc<SystemDef> {
        &self.def
    }

    /// Its live particles.
    #[must_use]
    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    /// Its running children.
    #[must_use]
    pub fn children(&self) -> &[Instance] {
        &self.children
    }

    /// Seconds since it started.
    #[must_use]
    pub fn age(&self) -> f32 {
        self.age
    }

    /// Stops it emitting; what is alive plays out.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.waiting.clear();
        for child in &mut self.children {
            child.stop();
        }
    }

    /// Whether it has nothing left to do: no emitter will fire again and no
    /// particle (of its own or its children's) is alive.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.particles.is_empty()
            && self.waiting.is_empty()
            && self.children.iter().all(Instance::finished)
            && (self.stopped || !self.may_emit())
    }

    fn may_emit(&self) -> bool {
        self.def
            .emitters
            .iter()
            .enumerate()
            .any(|(i, e)| match e.kind {
                Emitter::Instantaneously => !self.fired[i],
                Emitter::Continuously => {
                    let duration = e.float("emission_duration", 0.0);
                    let start = e.float("emission_start_time", 0.0);
                    duration <= 0.0 || self.age < start + duration
                }
            })
    }

    /// Advances by `dt` seconds. `library` supplies the children to start.
    pub fn update(&mut self, library: &Library, dt: f32, world: &dyn Collider) {
        let mut left = dt.max(0.0);
        while left > 0.0 {
            let step = left.min(MAX_STEP);
            self.step(library, step, world);
            left -= step;
        }
    }

    fn step(&mut self, library: &Library, dt: f32, world: &dyn Collider) {
        for point in &mut self.points {
            point.velocity = (point.position - point.previous) / dt;
        }
        self.age += dt;
        if !self.stopped {
            self.emit(dt);
        }
        self.apply_forces(dt);
        self.apply_operators(dt);
        self.apply_constraints(world);
        self.particles.retain(|p| !p.dead);
        for point in &mut self.points {
            point.previous = point.position;
        }
        let points = self.points.clone();
        let age = self.age;
        for (delay, _) in &mut self.waiting {
            *delay -= dt;
        }
        self.start_children(library, age.to_bits().into());
        for child in &mut self.children {
            // Control points the parent was given are shared; the rest a
            // child may have set itself from the parent's particles.
            for (i, point) in points.iter().enumerate().take(self.given) {
                if i < child.points.len() {
                    child.points[i] = *point;
                } else {
                    child.points.push(*point);
                }
            }
            child.update(library, dt, world);
        }
        self.children.retain(|c| !c.finished());
    }

    fn start_children(&mut self, library: &Library, seed: u64) {
        let mut i = 0;
        while i < self.waiting.len() {
            if self.waiting[i].0 <= 0.0 {
                let (_, def) = self.waiting.remove(i);
                let child_seed = seed ^ (u64::from(self.created) << 20) ^ i as u64;
                if let Some(mut child) = library.spawn(&def.name, &self.points, child_seed) {
                    child.sheet_frames.clone_from(&self.sheet_frames);
                    self.children.push(child);
                }
            } else {
                i += 1;
            }
        }
    }

    // --- emission

    fn emit(&mut self, dt: f32) {
        let mut count = 0usize;
        for i in 0..self.def.emitters.len() {
            let e = self.def.emitters[i].clone();
            let start = e.float("emission_start_time", 0.0);
            match e.kind {
                Emitter::Instantaneously => {
                    if !self.fired[i] && self.age >= start {
                        self.fired[i] = true;
                        let wanted = e.float("num_to_emit", 0.0);
                        let capped = e.int("maximum emission per frame", 100).max(0) as usize;
                        count +=
                            (wanted.round().max(0.0) as usize).min(capped.max(wanted as usize));
                    }
                }
                Emitter::Continuously => {
                    let duration = e.float("emission_duration", 0.0);
                    let rate = e.float("emission_rate", 100.0);
                    if self.age < start || (duration > 0.0 && self.age > start + duration) {
                        continue;
                    }
                    self.carry[i] += rate * dt * e.strength(self.age);
                    let whole = self.carry[i].floor();
                    self.carry[i] -= whole;
                    count += whole as usize;
                }
            }
        }
        for _ in 0..count {
            if self.particles.len() >= self.def.max_particles {
                break;
            }
            self.create(self.age);
        }
    }

    fn create(&mut self, creation_time: f32) {
        let def = self.def.clone();
        let mut p = Particle {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            age: 0.0,
            life: 1.0,
            radius: def.radius.max(0.0),
            rotation: 0.0,
            rotation_speed: 0.0,
            yaw: 0.0,
            color: [def.color[0], def.color[1], def.color[2]],
            alpha: def.color[3],
            sequence: def.sequence,
            sequence2: 0,
            trail_length: 0.1,
            id: self.created,
            radius0: 0.0,
            color0: [1.0; 3],
            alpha0: 1.0,
            anchor: self.points[0].position,
            dead: false,
        };
        self.created = self.created.wrapping_add(1);
        for init in &def.initializers {
            self.initialize(init, &mut p, creation_time);
        }
        p.radius0 = p.radius;
        p.color0 = p.color;
        p.alpha0 = p.alpha;
        self.particles.push(p);
    }

    fn point(&self, index: i32) -> ControlPoint {
        let i = usize::try_from(index).unwrap_or(0);
        self.points.get(i).copied().unwrap_or(self.points[0])
    }

    fn rand_range(&mut self, low: f32, high: f32, exponent: f32) -> f32 {
        let t = self.rng.unit();
        let t = if (exponent - 1.0).abs() > f32::EPSILON {
            t.powf(exponent)
        } else {
            t
        };
        low + (high - low) * t
    }

    fn rand_vec(&mut self, low: Vec3, high: Vec3) -> Vec3 {
        Vec3::new(
            self.rand_range(low.x, high.x, 1.0),
            self.rand_range(low.y, high.y, 1.0),
            self.rand_range(low.z, high.z, 1.0),
        )
    }

    fn unit_direction(&mut self) -> Vec3 {
        let z = self.rng.unit() * 2.0 - 1.0;
        let a = self.rng.unit() * std::f32::consts::TAU;
        let r = (1.0 - z * z).max(0.0).sqrt();
        Vec3::new(r * a.cos(), r * a.sin(), z)
    }

    #[allow(clippy::too_many_lines)]
    fn initialize(&mut self, init: &Step<Init>, p: &mut Particle, creation_time: f32) {
        let cp_index = init.int("control_point_number", init.int("control point number", 0));
        match init.kind {
            Init::RadiusRandom => {
                let (lo, hi) = init.range("radius_min", "radius_max", 1.0, 1.0);
                p.radius = self.rand_range(lo, hi, init.float("radius_random_exponent", 1.0));
            }
            Init::AlphaRandom => {
                let lo = init.int("alpha_min", 255) as f32 / 255.0;
                let hi = init.int("alpha_max", 255) as f32 / 255.0;
                p.alpha = self.rand_range(lo, hi, init.float("alpha_random_exponent", 1.0));
            }
            Init::ColorRandom => {
                let a = init.color("color1", [1.0; 3]);
                let b = init.color("color2", [1.0; 3]);
                let t = self.rng.unit();
                p.color = [
                    a[0] + (b[0] - a[0]) * t,
                    a[1] + (b[1] - a[1]) * t,
                    a[2] + (b[2] - a[2]) * t,
                ];
            }
            Init::LifetimeRandom => {
                let (lo, hi) = init.range("lifetime_min", "lifetime_max", 0.0, 0.0);
                p.life = self
                    .rand_range(lo, hi, init.float("lifetime_random_exponent", 1.0))
                    .max(1e-3);
            }
            Init::RotationRandom => {
                let (lo, hi) = init.range("rotation_offset_min", "rotation_offset_max", 0.0, 360.0);
                let degrees = init.float("rotation_initial", 0.0)
                    + self.rand_range(lo, hi, init.float("rotation_random_exponent", 1.0));
                p.rotation = degrees.to_radians();
            }
            Init::RotationYawRandom => {
                let (lo, hi) = init.range("yaw_offset_min", "yaw_offset_max", 0.0, 360.0);
                p.yaw = init.float("yaw_initial", 0.0)
                    + self.rand_range(lo, hi, init.float("yaw_random_exponent", 1.0));
            }
            Init::RotationYawFlip => {
                if self.rng.unit() < init.float("Flip Percentage", 0.5) {
                    p.yaw += 180.0;
                }
            }
            Init::SequenceRandom | Init::SequenceTwoRandom => {
                let lo = init.int("sequence_min", 0);
                let hi = init.int("sequence_max", 0).max(lo);
                let n = lo + self.rng.below((hi - lo + 1) as usize) as i32;
                if init.kind == Init::SequenceRandom {
                    p.sequence = n;
                } else {
                    p.sequence2 = n;
                }
            }
            Init::TrailLengthRandom => {
                let (lo, hi) = init.range("length_min", "length_max", 0.1, 0.1);
                p.trail_length = self.rand_range(lo, hi, init.float("length_random_exponent", 1.0));
            }
            Init::LifetimeFromSequence => {
                let fps = init.float("Frames Per Second", 30.0).max(0.01);
                let frames = usize::try_from(p.sequence)
                    .ok()
                    .and_then(|i| self.sheet_frames.get(i))
                    .copied()
                    .unwrap_or(DEFAULT_SEQUENCE_FRAMES);
                p.life = (frames / fps).max(1e-3);
            }
            Init::LifetimePreAgeNoise => {
                let (lo, hi) = init.range("start age minimum", "start age maximum", 0.0, 0.0);
                let n = noise3(p.position * init.float("spatial noise coordinate scale", 1.0));
                p.age = lo + (hi - lo) * n;
            }
            Init::PositionSphere => {
                let cp = self.point(cp_index);
                let (dmin, dmax) = init.range("distance_min", "distance_max", 0.0, 0.0);
                let bias = init.vec3("distance_bias", Vec3::ONE);
                let dir = self.unit_direction();
                let distance = self.rand_range(dmin, dmax, 1.0);
                let offset = dir * bias * distance;
                let local = init.flag("bias in local system", false);
                p.position = cp.position + if local { cp.to_world(offset) } else { offset };
                let (smin, smax) = init.range("speed_min", "speed_max", 0.0, 0.0);
                let speed = self.rand_range(smin, smax, init.float("speed_random_exponent", 1.0));
                let lmin = init.vec3("speed_in_local_coordinate_system_min", Vec3::ZERO);
                let lmax = init.vec3("speed_in_local_coordinate_system_max", Vec3::ZERO);
                let local_speed = self.rand_vec(lmin, lmax);
                p.velocity += dir * speed + cp.to_world(local_speed);
            }
            Init::PositionBox => {
                let cp = self.point(cp_index);
                let low = init.vec3("min", Vec3::splat(-1.0));
                let high = init.vec3("max", Vec3::splat(1.0));
                let local = self.rand_vec(low, high);
                p.position = cp.position + cp.to_world(local);
            }
            Init::PositionOffset => {
                let cp = self.point(cp_index);
                let low = init.vec3("offset min", Vec3::ZERO);
                let high = init.vec3("offset max", Vec3::ZERO);
                let mut offset = self.rand_vec(low, high);
                if init.flag("offset proportional to radius 0/1", false) {
                    offset *= p.radius;
                }
                p.position += if init.flag("offset in local space 0/1", false) {
                    cp.to_world(offset)
                } else {
                    offset
                };
            }
            Init::PositionWarp => {
                let cp = self.point(cp_index);
                let low = init.vec3("warp min", Vec3::ONE);
                let high = init.vec3("warp max", Vec3::ONE);
                let local = cp.basis().transpose() * (p.position - cp.position);
                let warp = self.rand_vec(low, high);
                p.position = cp.position + cp.to_world(local * warp);
            }
            Init::PositionAlongPath => {
                let start = self.point(init.int("start control point number", 0));
                let end = self.point(init.int("end control point number", 1));
                let span = init
                    .float("particles to map from start to end", 50.0)
                    .max(1.0);
                let mut t = (p.id as f32 % span) / span;
                if !init.flag("restart behavior (0 = bounce, 1 = loop ) ", false) {
                    let cycle = (p.id as f32 / span).floor() as u32;
                    if cycle % 2 == 1 {
                        t = 1.0 - t;
                    }
                }
                let mid = init.float("mid point position", 0.5).clamp(0.01, 0.99);
                let bulge = init.float("bulge", 0.0);
                let line = start.position.lerp(end.position, t);
                let swell = if t < mid {
                    t / mid
                } else {
                    (1.0 - t) / (1.0 - mid)
                };
                let sideways = self.unit_direction();
                p.position = line + sideways * bulge * swell;
            }
            Init::PositionInHierarchy => {
                let start = self.point(init.int("start control point number", 0));
                let end = self.point(init.int("end control point number", 1));
                let t = self.rng.unit();
                let reach = init.float("maximum distance", 0.0);
                let bias = init.vec3("distance_bias", Vec3::ONE);
                let dir = self.unit_direction() * bias;
                p.position = start.position.lerp(end.position, t) + dir * reach * self.rng.unit();
            }
            Init::PositionOnModel => {
                p.position = self.point(cp_index).position;
            }
            Init::VelocityNoise => {
                let cp = self.point(init.int("Control Point Number", 0));
                let low = init.vec3("output minimum", Vec3::ZERO);
                let high = init.vec3("output maximum", Vec3::ZERO);
                let scale = init.float("Spatial Noise Coordinate Scale", 1.0);
                let offset = init.vec3("Spatial Coordinate Offset", Vec3::ZERO);
                let time = (creation_time + init.float("Time Coordinate Offset", 0.0))
                    * init.float("Time Noise Coordinate Scale", 1.0);
                let abs = init.vec3("Absolute Value", Vec3::ZERO);
                let invert = init.vec3("Invert Abs Value", Vec3::ZERO);
                let base = (p.position + offset) * scale + Vec3::splat(time);
                let axis = |value: f32, abs: f32, invert: f32| {
                    if abs > 0.5 {
                        let a = (value * 2.0 - 1.0).abs();
                        if invert > 0.5 { 1.0 - a } else { a }
                    } else {
                        value
                    }
                };
                let noise = Vec3::new(
                    axis(noise3(base), abs.x, invert.x),
                    axis(noise3(base + Vec3::new(31.4, 7.7, 0.0)), abs.y, invert.y),
                    axis(noise3(base + Vec3::new(0.0, 19.1, 53.9)), abs.z, invert.z),
                );
                let v = low + (high - low) * noise;
                p.velocity += if init.flag("Apply Velocity in Local Space (0/1)", false) {
                    cp.to_world(v)
                } else {
                    v
                };
            }
            Init::VelocityInherit => {
                let cp = self.point(init.int("control point number", 0));
                p.velocity += cp.velocity * init.float("velocity scale", 1.0);
            }
            Init::RemapScalar => {
                let input = read_scalar(p, init.int("input field", 0), creation_time);
                let (lo, hi) = init.range("input minimum", "input maximum", 0.0, 1.0);
                if init.flag("only active within specified input range", false)
                    && !(lo.min(hi)..=lo.max(hi)).contains(&input)
                {
                    return;
                }
                let t = ramp(input, lo, hi);
                let (omin, omax) = init.range("output minimum", "output maximum", 0.0, 1.0);
                let value = omin + (omax - omin) * t;
                let field = init.int("output field", 0);
                let value = if init.flag("output is scalar of initial random range", false) {
                    read_scalar(p, field, creation_time) * value
                } else {
                    value
                };
                write_scalar(p, field, value);
            }
            Init::RemapNoise => {
                let scale = init.float("spatial noise coordinate scale", 1.0);
                let offset = init.vec3("spatial coordinate offset", Vec3::ZERO);
                let time = (creation_time + init.float("time coordinate offset", 0.0))
                    * init.float("time noise coordinate scale", 1.0);
                let mut n = noise3((p.position + offset) * scale + Vec3::splat(time));
                if init.flag("absolute value", false) {
                    n = (n * 2.0 - 1.0).abs();
                    if init.flag("invert absolute value", false) {
                        n = 1.0 - n;
                    }
                }
                let (omin, omax) = init.range("output minimum", "output maximum", 0.0, 1.0);
                write_scalar(p, init.int("output field", 0), omin + (omax - omin) * n);
            }
        }
    }

    // --- forces, operators, constraints

    fn apply_forces(&mut self, dt: f32) {
        let forces = self.def.forces.clone();
        let points = self.points.clone();
        let mut rng = self.rng.clone();
        for force in &forces {
            let strength = force.strength(self.age);
            for p in &mut self.particles {
                let accel = match force.kind {
                    Force::Random => {
                        let low = force.vec3("min force", Vec3::ZERO);
                        let high = force.vec3("max force", Vec3::ZERO);
                        Vec3::new(
                            low.x + (high.x - low.x) * rng.unit(),
                            low.y + (high.y - low.y) * rng.unit(),
                            low.z + (high.z - low.z) * rng.unit(),
                        )
                    }
                    Force::Twist => {
                        let axis = force.vec3("twist axis", Vec3::Z).normalize_or_zero();
                        let axis = if force.flag("object local space axis 0/1", false) {
                            points[0].to_world(axis)
                        } else {
                            axis
                        };
                        let radial = p.position - points[0].position;
                        axis.cross(radial).normalize_or_zero() * force.float("amount of force", 0.0)
                    }
                    Force::PullToControlPoint => {
                        let cp = points
                            .get(force.int("control point number", 0).max(0) as usize)
                            .unwrap_or(&points[0]);
                        let to = cp.position - p.position;
                        let falloff = force.float("falloff power", 0.0);
                        let scale = if falloff > 0.0 {
                            1.0 / to.length().max(1.0).powf(falloff)
                        } else {
                            1.0
                        };
                        to.normalize_or_zero() * force.float("amount of force", 0.0) * scale
                    }
                };
                p.velocity += accel * dt * strength;
            }
        }
        self.rng = rng;
    }

    #[allow(clippy::too_many_lines)]
    fn apply_operators(&mut self, dt: f32) {
        let operators = self.def.operators.clone();
        let points = self.points.clone();
        let time = self.age;
        for op in &operators {
            if op.kind == Op::SetChildControlPoints {
                self.set_child_points(op);
                continue;
            }
            let mut rng = self.rng.clone();
            for p in &mut self.particles {
                if p.age == 0.0 && op.kind != Op::Movement {
                    // Operators see a particle from its first full step.
                }
                run_operator(op, p, dt, time, &points, &mut rng);
            }
            self.rng = rng;
        }
        for p in &mut self.particles {
            p.age += dt;
        }
    }

    fn set_child_points(&mut self, op: &Step<Op>) {
        let first = op.int("First control point to set", 1).max(0) as usize;
        let count = op.int("# of control points to set", 1).max(0) as usize;
        let skip = op.int("first particle to copy", 0).max(0) as usize;
        let positions: Vec<Vec3> = self
            .particles
            .iter()
            .skip(skip)
            .take(count)
            .map(|p| p.position)
            .collect();
        for child in &mut self.children {
            for (i, position) in positions.iter().enumerate() {
                let slot = first + i;
                while child.points.len() <= slot {
                    child.points.push(ControlPoint::at(*position));
                }
                child.points[slot].move_to(*position);
            }
        }
    }

    fn apply_constraints(&mut self, world: &dyn Collider) {
        let constraints = self.def.constraints.clone();
        let points = self.points.clone();
        for c in &constraints {
            for p in &mut self.particles {
                match c.kind {
                    Constraint::Collision => collide(c, p, world),
                    Constraint::Plane => {
                        let cp = &points[usize::try_from(c.int("control point number", 0))
                            .unwrap_or(0)
                            .min(points.len() - 1)];
                        let local_normal = c.vec3("plane normal", Vec3::NEG_Z);
                        let local_point = c.vec3("plane point", Vec3::ZERO);
                        let (normal, point) = if c.flag("global normal", false) {
                            (local_normal, cp.position + local_point)
                        } else {
                            (
                                cp.to_world(local_normal),
                                cp.position + cp.to_world(local_point),
                            )
                        };
                        let normal = normal.normalize_or_zero();
                        // The kept side is where the normal points.
                        let depth = (p.position - point).dot(normal);
                        if depth < 0.0 {
                            p.position -= normal * depth;
                            let into = p.velocity.dot(normal);
                            if into < 0.0 {
                                p.velocity -= normal * into;
                            }
                        }
                    }
                    Constraint::DistanceToControlPoint => {
                        let cp = &points[usize::try_from(c.int("control point number", 0))
                            .unwrap_or(0)
                            .min(points.len() - 1)];
                        let center = cp.position + c.vec3("offset of center", Vec3::ZERO);
                        let (lo, hi) = c.range("minimum distance", "maximum distance", 0.0, 100.0);
                        let offset = p.position - center;
                        let length = offset.length();
                        if length > hi {
                            p.position = center + offset / length * hi;
                        } else if length < lo && length > 1e-4 {
                            p.position = center + offset / length * lo;
                        }
                    }
                }
            }
        }
    }
}

fn collide(c: &Step<Constraint>, p: &mut Particle, world: &dyn Collider) {
    let from = p.position - p.velocity * (1.0 / 60.0);
    let Some(hit) = world.trace(from, p.position) else {
        return;
    };
    if c.flag("kill particle on collision", false) {
        p.dead = true;
        return;
    }
    let bounce = c.float("amount of bounce", 0.0);
    let slide = c.float("amount of slide", 1.0);
    let normal_speed = p.velocity.dot(hit.normal);
    let tangent = p.velocity - hit.normal * normal_speed;
    p.velocity = tangent * slide - hit.normal * normal_speed * bounce;
    p.position = hit.point + hit.normal * (p.radius * c.float("radius scale", 1.0)).max(0.1);
}

#[allow(clippy::too_many_lines)]
fn run_operator(
    op: &Step<Op>,
    p: &mut Particle,
    dt: f32,
    time: f32,
    points: &[ControlPoint],
    rng: &mut Rng,
) {
    let t = p.life_fraction();
    let cp_of = |key: &str| {
        let i = usize::try_from(op.int(key, 0)).unwrap_or(0);
        points.get(i).copied().unwrap_or(points[0])
    };
    match op.kind {
        Op::Movement => {
            p.velocity += op.vec3("gravity", Vec3::ZERO) * dt;
            let drag = op.float("drag", 0.0).clamp(0.0, 1.0);
            if drag > 0.0 {
                p.velocity *= (1.0 - drag).powf(dt / DRAG_TICK);
            }
            p.position += p.velocity * dt;
        }
        Op::LifespanDecay => {
            if p.age + dt >= p.life {
                p.dead = true;
            }
        }
        Op::AlphaFadeIn => {
            let (lo, hi) = op.range("fade in time min", "fade in time max", 0.25, 0.25);
            let exponent = op.float("fade in time exponent", 1.0);
            let mut span = lo + (hi - lo) * p.random(1).powf(exponent);
            if !op.flag("proportional 0/1", true) {
                span /= p.life.max(1e-3);
            }
            if span > 0.0 && t < span {
                p.alpha = p.alpha0 * (t / span);
            }
        }
        Op::AlphaFadeOut => {
            let (lo, hi) = op.range("fade out time min", "fade out time max", 0.25, 0.5);
            let exponent = op.float("fade out time exponent", 1.0);
            let mut span = lo + (hi - lo) * p.random(2).powf(exponent);
            if !op.flag("proportional 0/1", true) {
                span /= p.life.max(1e-3);
            }
            let start = 1.0 - span;
            if span > 0.0 && t > start {
                let mut f = (t - start) / span;
                if op.flag("ease in and out", false) {
                    f = f * f * (3.0 - 2.0 * f);
                }
                p.alpha = p.alpha0 * (1.0 - f);
            }
        }
        Op::AlphaFadeAndDecay => {
            let start_alpha = op.float("start_alpha", 1.0);
            let end_alpha = op.float("end_alpha", 0.0);
            let in_start = op.float("start_fade_in_time", 0.0);
            let in_end = op.float("end_fade_in_time", 0.0);
            let out_start = op.float("start_fade_out_time", 1.0);
            let out_end = op.float("end_fade_out_time", 1.0);
            if t >= out_end {
                p.dead = true;
            } else if t > out_start {
                p.alpha = p.alpha0 * lerp(1.0, end_alpha, ramp(t, out_start, out_end));
            } else if t < in_end {
                p.alpha = p.alpha0 * lerp(start_alpha, 1.0, ramp(t, in_start, in_end));
            }
        }
        Op::ColorFade => {
            let start = op.float("fade_start_time", 0.0);
            let end = op.float("fade_end_time", 1.0);
            let target = op.color("color_fade", [1.0; 3]);
            let mut f = ramp(t, start, end);
            if op.flag("ease_in_and_out", false) {
                f = f * f * (3.0 - 2.0 * f);
            }
            for ((now, from), to) in p.color.iter_mut().zip(p.color0).zip(target) {
                *now = lerp(from, to, f);
            }
        }
        Op::RadiusScale => {
            let start = op.float("start_time", 0.0);
            let end = op.float("end_time", 1.0);
            let mut f = ramp(t, start, end);
            if op.flag("ease_in_and_out", false) {
                f = f * f * (3.0 - 2.0 * f);
            }
            let bias = op.float("scale_bias", 0.5).clamp(0.01, 0.99);
            f = f.powf(bias.ln() / 0.5_f32.ln());
            let scale = lerp(
                op.float("radius_start_scale", 1.0),
                op.float("radius_end_scale", 1.0),
                f,
            );
            p.radius = p.radius0 * scale;
        }
        Op::SpinRoll => {
            let rate = op.int("spin_rate_degrees", 0) as f32;
            let stop = op.float("spin_stop_time", 0.0);
            let speed = if stop > 0.0 {
                rate * (1.0 - (t / stop).min(1.0))
            } else {
                rate
            };
            p.rotation_speed = speed.to_radians();
            p.rotation += p.rotation_speed * dt;
        }
        Op::OscillateScalar | Op::OscillateVector => {
            oscillate(op, p, dt, time);
        }
        Op::LockToControlPoint => {
            let cp = cp_of("control_point_number");
            let start = op.float("start_fadeout_min", 0.0);
            let end = op.float("end_fadeout_min", 1.0);
            let hold = 1.0 - ramp(t, start, end);
            p.position += (cp.position - p.anchor) * hold;
            p.anchor = cp.position;
        }
        Op::DampenToControlPoint => {
            let cp = cp_of("control_point_number");
            let range = op.float("falloff range", 1.0).max(1e-3);
            let distance = (p.position - cp.position).length();
            if distance < range {
                let damp = op.float("dampen scale", 0.0) * (1.0 - distance / range);
                p.velocity *= (1.0 - damp).max(0.0).powf(dt / DRAG_TICK);
            }
        }
        Op::NoiseVector => {
            let scale = op.float("noise coordinate scale", 1.0);
            let low = op.vec3("output minimum", Vec3::ZERO);
            let high = op.vec3("output maximum", Vec3::ONE);
            let base = p.position * scale + Vec3::splat(time);
            let n = Vec3::new(
                noise3(base),
                noise3(base + Vec3::new(31.4, 7.7, 0.0)),
                noise3(base + Vec3::new(0.0, 19.1, 53.9)),
            );
            let v = low + (high - low) * n;
            write_vector(p, op.int("output field", FIELD_TINT), v);
        }
        Op::RemapDistance => {
            let cp = cp_of("control point");
            let distance = (p.position - cp.position).length();
            let (dmin, dmax) = op.range("distance minimum", "distance maximum", 0.0, 128.0);
            let (omin, omax) = op.range("output minimum", "output maximum", 0.0, 1.0);
            let value = lerp(omin, omax, ramp(distance, dmin, dmax));
            let field = op.int("output field", 0);
            let value = if op.flag("output is scalar of initial random range", false) {
                // Scales what the particle started with, not what it has now.
                (if field == FIELD_ALPHA {
                    p.alpha0
                } else {
                    read_scalar(p, field, 0.0)
                }) * value
            } else {
                value
            };
            write_scalar(p, field, value);
        }
        Op::CullPlane => {
            let cp = cp_of("Control Point for point on plane");
            let normal = op.vec3("Plane Normal", Vec3::Z).normalize_or_zero();
            let point = cp.position + normal * op.float("Cull plane offset", 0.0);
            if (p.position - point).dot(normal) < 0.0 {
                p.dead = true;
            }
        }
        Op::CullRandom => {
            let start = op.float("Cull Start Time", 0.0);
            let end = op.float("Cull End Time", 1.0);
            if time >= start && time <= end {
                let share = op.float("Cull Percentage", 0.0) * dt / (end - start).max(dt);
                if rng.unit() < share {
                    p.dead = true;
                }
            }
        }
        Op::SetChildControlPoints | Op::Inert => {}
    }
}

fn oscillate(op: &Step<Op>, p: &mut Particle, dt: f32, time: f32) {
    let field = op.int("oscillation field", 0);
    let scalar = op.kind == Op::OscillateScalar;
    let start = lerp(
        op.float("start time min", 0.0),
        op.float("start time max", 0.0),
        p.random(3),
    );
    let end = lerp(
        op.float("end time min", 1.0),
        op.float("end time max", 1.0),
        p.random(4),
    );
    let proportional = op.flag("start/end proportional", true);
    let now = if proportional {
        p.life_fraction()
    } else {
        time
    };
    if now < start || now > end {
        return;
    }
    let multiplier = op.float("oscillation multiplier", 2.0);
    let phase = op.float("oscillation start phase", 0.5);
    let sample = |low: f32, high: f32, salt: u32| lerp(low, high, p.random(salt));
    let wave = |rate: f32, frequency: f32| {
        // The field swings around its value by `rate` per second.
        let angle = (p.age * frequency + phase) * std::f32::consts::TAU;
        angle.sin() * rate * multiplier.max(0.0).max(1.0) * dt
    };
    if scalar {
        let rate = sample(
            op.float("oscillation rate min", 0.0),
            op.float("oscillation rate max", 0.0),
            5,
        );
        let frequency = sample(
            op.float("oscillation frequency min", 1.0),
            op.float("oscillation frequency max", 1.0),
            6,
        );
        let current = read_scalar(p, field, p.age);
        write_scalar(p, field, current + wave(rate, frequency));
    } else {
        let low = op.vec3("oscillation rate min", Vec3::ZERO);
        let high = op.vec3("oscillation rate max", Vec3::ZERO);
        let flow = op.vec3("oscillation frequency min", Vec3::ONE);
        let fhigh = op.vec3("oscillation frequency max", Vec3::ONE);
        let axis = |i: usize, salt: u32| {
            wave(
                lerp(low[i], high[i], p.random(salt)),
                lerp(flow[i], fhigh[i], p.random(salt + 10)),
            )
        };
        let delta = Vec3::new(axis(0, 7), axis(1, 8), axis(2, 9));
        let current = read_vector(p, field);
        write_vector(p, field, current + delta);
    }
}

fn read_scalar(p: &Particle, field: i32, creation_time: f32) -> f32 {
    match field {
        FIELD_LIFE => p.life,
        FIELD_RADIUS => p.radius,
        FIELD_ROTATION => p.rotation.to_degrees(),
        FIELD_ROTATION_SPEED => p.rotation_speed.to_degrees(),
        FIELD_ALPHA => p.alpha,
        FIELD_CREATION_TIME => creation_time,
        FIELD_SEQUENCE => p.sequence as f32,
        FIELD_TRAIL_LENGTH => p.trail_length,
        FIELD_YAW => p.yaw,
        _ => 0.0,
    }
}

fn write_scalar(p: &mut Particle, field: i32, value: f32) {
    match field {
        FIELD_LIFE => p.life = value.max(1e-3),
        FIELD_RADIUS => {
            p.radius = value;
            p.radius0 = value;
        }
        FIELD_ROTATION => p.rotation = value.to_radians(),
        FIELD_ROTATION_SPEED => p.rotation_speed = value.to_radians(),
        FIELD_ALPHA => {
            p.alpha = value;
            p.alpha0 = value;
        }
        FIELD_SEQUENCE => p.sequence = value.round() as i32,
        FIELD_TRAIL_LENGTH => p.trail_length = value,
        FIELD_YAW => p.yaw = value,
        _ => {}
    }
}

fn read_vector(p: &Particle, field: i32) -> Vec3 {
    match field {
        FIELD_XYZ => p.position,
        FIELD_TINT => Vec3::from(p.color),
        _ => Vec3::ZERO,
    }
}

fn write_vector(p: &mut Particle, field: i32, value: Vec3) {
    match field {
        FIELD_XYZ => p.position = value,
        FIELD_TINT => {
            p.color = value.to_array();
            p.color0 = p.color;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Small maths

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// `0` at `start`, `1` at `end`, clamped; a step if they coincide.
fn ramp(x: f32, start: f32, end: f32) -> f32 {
    if (end - start).abs() < f32::EPSILON {
        return if x >= end { 1.0 } else { 0.0 };
    }
    ((x - start) / (end - start)).clamp(0.0, 1.0)
}

fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

fn hash01(a: u32, b: u32) -> f32 {
    (hash_u32(a.wrapping_mul(0x9e37_79b9) ^ hash_u32(b.wrapping_add(0x85eb_ca6b))) >> 8) as f32
        / (1u32 << 24) as f32
}

fn lattice(x: i32, y: i32, z: i32) -> f32 {
    let h = hash_u32(
        (x as u32).wrapping_mul(0x8da6_b343)
            ^ (y as u32).wrapping_mul(0xd816_3841)
            ^ (z as u32).wrapping_mul(0xcb1a_b31f),
    );
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Smooth value noise in `0..1`.
fn noise3(point: Vec3) -> f32 {
    let base = point.floor();
    let fraction = point - base;
    let smooth = fraction * fraction * (Vec3::splat(3.0) - 2.0 * fraction);
    let cell = base.as_ivec3();
    let corner = |dx: i32, dy: i32, dz: i32| lattice(cell.x + dx, cell.y + dy, cell.z + dz);
    let along_x = |dy: i32, dz: i32| lerp(corner(0, dy, dz), corner(1, dy, dz), smooth.x);
    let low = lerp(along_x(0, 0), along_x(1, 0), smooth.y);
    let high = lerp(along_x(0, 1), along_x(1, 1), smooth.y);
    lerp(low, high, smooth.z)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn op(function: &str, attrs: &[(&str, Value)]) -> Operator {
        Operator {
            function: function.to_owned(),
            attributes: attrs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn vec3(x: f32, y: f32, z: f32) -> Value {
        Value::Vec(vec![x, y, z])
    }

    fn burst(count: f32, life: f32) -> ParticleSystem {
        ParticleSystem {
            name: "burst".into(),
            max_particles: 100,
            radius: 2.0,
            color: [255; 4],
            emitters: vec![op(
                "emit_instantaneously",
                &[("num_to_emit", Value::Float(count))],
            )],
            initializers: vec![op(
                "Lifetime Random",
                &[
                    ("lifetime_min", Value::Float(life)),
                    ("lifetime_max", Value::Float(life)),
                ],
            )],
            operators: vec![op("Lifespan Decay", &[]), op("Movement Basic", &[])],
            ..ParticleSystem::default()
        }
    }

    fn run(system: ParticleSystem) -> (Library, Instance) {
        let mut library = Library::new();
        let name = system.name.clone();
        assert!(library.add(&[system]).is_empty());
        let instance = library
            .spawn(&name, &[ControlPoint::at(Vec3::ZERO)], 1)
            .unwrap();
        (library, instance)
    }

    #[test]
    fn an_instant_burst_emits_once_and_dies_after_its_lifetime() {
        let (library, mut s) = run(burst(10.0, 0.5));
        s.update(&library, 0.1, &EmptyWorld);
        assert_eq!(s.particles().len(), 10);
        s.update(&library, 0.1, &EmptyWorld);
        assert_eq!(s.particles().len(), 10, "no second burst");
        assert!(!s.finished());
        s.update(&library, 0.5, &EmptyWorld);
        assert!(s.particles().is_empty());
        assert!(s.finished());
    }

    #[test]
    fn continuous_emission_follows_rate_and_duration() {
        let mut system = burst(0.0, 10.0);
        system.emitters = vec![op(
            "emit_continuously",
            &[
                ("emission_rate", Value::Float(100.0)),
                ("emission_duration", Value::Float(0.5)),
            ],
        )];
        let (library, mut s) = run(system);
        for _ in 0..100 {
            s.update(&library, 0.01, &EmptyWorld);
        }
        let n = s.particles().len();
        assert!((48..=52).contains(&n), "{n} particles from 0.5 s at 100/s");
    }

    #[test]
    fn particles_stop_at_the_system_cap() {
        let mut system = burst(500.0, 5.0);
        system.max_particles = 40;
        let (library, mut s) = run(system);
        s.update(&library, 0.05, &EmptyWorld);
        assert_eq!(s.particles().len(), 40);
    }

    #[test]
    fn gravity_pulls_and_a_floor_stops_a_particle() {
        let mut system = burst(1.0, 5.0);
        system.initializers.push(op(
            "Position Modify Offset Random",
            &[
                ("offset min", vec3(0.0, 0.0, 100.0)),
                ("offset max", vec3(0.0, 0.0, 100.0)),
            ],
        ));
        system.operators = vec![
            op("Lifespan Decay", &[]),
            op("Movement Basic", &[("gravity", vec3(0.0, 0.0, -800.0))]),
        ];
        system.constraints = vec![op(
            "Collision via traces",
            &[("amount of bounce", Value::Float(0.0))],
        )];
        let (library, mut s) = run(system);
        for _ in 0..120 {
            s.update(&library, 1.0 / 60.0, &Floor(0.0));
        }
        let z = s.particles()[0].position.z;
        assert!((0.0..3.0).contains(&z), "rests on the floor, z = {z}");
    }

    #[test]
    fn a_sphere_initializer_places_and_launches() {
        let mut system = burst(50.0, 1.0);
        system.initializers.push(op(
            "Position Within Sphere Random",
            &[
                ("distance_max", Value::Float(10.0)),
                ("speed_min", Value::Float(100.0)),
                ("speed_max", Value::Float(100.0)),
            ],
        ));
        let (library, mut s) = run(system);
        s.update(&library, 0.001, &EmptyWorld);
        for p in s.particles() {
            assert!(p.position.length() <= 10.5);
            assert!((p.velocity.length() - 100.0).abs() < 1.0, "{p:?}");
        }
    }

    #[test]
    fn fades_and_scales_follow_the_life_fraction() {
        let mut system = burst(1.0, 1.0);
        system.operators = vec![
            op("Lifespan Decay", &[]),
            op(
                "Radius Scale",
                &[
                    ("radius_start_scale", Value::Float(1.0)),
                    ("radius_end_scale", Value::Float(5.0)),
                    ("scale_bias", Value::Float(0.5)),
                ],
            ),
            op(
                "Alpha Fade and Decay",
                &[
                    ("start_fade_out_time", Value::Float(0.5)),
                    ("end_fade_out_time", Value::Float(1.0)),
                ],
            ),
        ];
        let (library, mut s) = run(system);
        s.update(&library, 0.5, &EmptyWorld);
        let p = &s.particles()[0];
        assert!((p.radius - 2.0 * 3.0).abs() < 0.2, "radius {}", p.radius);
        assert!(p.alpha > 0.95);
        s.update(&library, 0.4, &EmptyWorld);
        let p = &s.particles()[0];
        assert!(p.alpha < 0.3, "alpha {}", p.alpha);
    }

    #[test]
    fn a_plane_constraint_keeps_particles_on_one_side() {
        let mut system = burst(1.0, 5.0);
        system.initializers.push(op(
            "Position Within Sphere Random",
            &[
                ("speed_min", Value::Float(0.0)),
                ("speed_max", Value::Float(0.0)),
                (
                    "speed_in_local_coordinate_system_min",
                    vec3(0.0, 0.0, -200.0),
                ),
                (
                    "speed_in_local_coordinate_system_max",
                    vec3(0.0, 0.0, -200.0),
                ),
            ],
        ));
        system.constraints = vec![op(
            "Prevent passing through a plane",
            &[
                ("plane normal", vec3(0.0, 0.0, 1.0)),
                ("plane point", vec3(0.0, 0.0, -10.0)),
            ],
        )];
        let (library, mut s) = run(system);
        for _ in 0..60 {
            s.update(&library, 1.0 / 60.0, &EmptyWorld);
        }
        assert!(s.particles()[0].position.z >= -10.001);
    }

    #[test]
    fn children_start_after_their_delay_and_share_control_points() {
        let mut parent = burst(0.0, 5.0);
        parent.name = "parent".into();
        parent.emitters.clear();
        parent.children = vec![source_formats::pcf::Child {
            name: "child".into(),
            delay: 0.2,
        }];
        let mut child = burst(3.0, 1.0);
        child.name = "child".into();
        let mut library = Library::new();
        library.add(&[parent, child]);
        let mut s = library
            .spawn("parent", &[ControlPoint::at(Vec3::new(5.0, 0.0, 0.0))], 3)
            .unwrap();
        s.update(&library, 0.1, &EmptyWorld);
        assert!(s.children().is_empty());
        s.update(&library, 0.2, &EmptyWorld);
        assert_eq!(s.children().len(), 1);
        assert_eq!(s.children()[0].points[0].position.x, 5.0);
        assert!(!s.finished(), "alive while a child is");
        s.update(&library, 1.5, &EmptyWorld);
        assert!(s.finished());
    }

    #[test]
    fn unknown_operators_are_reported_not_fatal() {
        let mut system = burst(1.0, 1.0);
        system.operators.push(op("Imaginary Operator", &[]));
        let mut library = Library::new();
        assert_eq!(library.add(&[system]), ["Imaginary Operator"]);
    }

    #[test]
    fn the_same_seed_gives_the_same_particles() {
        let make = || {
            let mut system = burst(20.0, 1.0);
            system.initializers.push(op(
                "Position Within Sphere Random",
                &[("distance_max", Value::Float(30.0))],
            ));
            let (library, mut s) = run(system);
            s.update(&library, 0.05, &EmptyWorld);
            s.particles().iter().map(|p| p.position).collect::<Vec<_>>()
        };
        assert_eq!(make(), make());
    }

    #[test]
    fn noise_is_smooth_and_in_range() {
        for i in 0..200 {
            let p = Vec3::new(i as f32 * 0.37, i as f32 * 0.11, 3.0);
            let n = noise3(p);
            assert!((0.0..=1.0).contains(&n));
            assert!((noise3(p + Vec3::splat(0.001)) - n).abs() < 0.05);
        }
    }

    #[test]
    fn a_long_step_is_split() {
        let (library, mut s) = run(burst(5.0, 0.3));
        s.update(&library, 2.0, &EmptyWorld);
        assert!(s.finished());
    }

    /// Runs every system of a real install: all operators known, and each
    /// system plays out. `RUSTLIFE_TEST_GAME_DIR=<install> cargo test -p
    /// rl_client --lib real_systems -- --ignored --nocapture`.
    #[test]
    #[ignore = "requires a Half-Life 2: Deathmatch install"]
    fn real_systems_play_out() {
        use source_fs::content::{ContentOptions, mount_game};
        let dir = std::env::var_os("RUSTLIFE_TEST_GAME_DIR").map(std::path::PathBuf::from);
        let options = ContentOptions {
            game_dir: dir,
            ..ContentOptions::default()
        };
        let Ok(game) = mount_game(&options) else {
            return;
        };
        let mut library = Library::new();
        let mut unknown = Vec::new();
        for file in game.vfs.list("particles/*.pcf") {
            let bytes = game.vfs.read(&file.path).unwrap().unwrap();
            let systems = source_formats::pcf::parse(&bytes).unwrap().systems();
            unknown.extend(library.add(&systems));
        }
        unknown.sort();
        unknown.dedup();
        eprintln!("{} systems; unimplemented: {unknown:?}", library.len());
        assert!(unknown.is_empty(), "{unknown:?}");
        let mut names: Vec<String> = library.systems.keys().cloned().collect();
        names.sort();
        for name in names {
            let mut s = library
                .spawn(&name, &[ControlPoint::facing(Vec3::ZERO, Vec3::Z)], 7)
                .unwrap();
            let mut peak = 0;
            for _ in 0..120 {
                s.update(&library, 1.0 / 60.0, &Floor(-50.0));
                peak = peak.max(s.particles().len());
                for p in s.particles() {
                    assert!(p.position.is_finite(), "{name}: {p:?}");
                    assert!(p.radius.is_finite() && p.alpha.is_finite(), "{name}");
                }
            }
            eprintln!("{name}: peak {peak}, finished {}", s.finished());
        }
    }
}
