//! Source-like player movement.
//!
//! [`simulate`] advances one player by one tick: it reads the player's
//! [`PlayerState`] and the tick's [`UserCmd`] and returns the next state. It
//! is a pure function of its inputs — the same state, command, tuning and
//! world always produce bit-identical results — and allocates nothing, which
//! is what lets the client predict its own player with the very code the
//! server runs.
//!
//! The algorithm follows the publicly documented structure of the Source
//! engine's player movement (`docs/movement.md` walks through it), written
//! afresh for rustlife:
//!
//! 1. **Duck** (`duck`): crouch over time on the ground (the hull shrinks
//!    when fully down), instantly in the air by lifting the feet.
//! 2. **Ladders** (`ladder`): a mounted player climbs along the ladder.
//! 3. **Categorize** (`ground`): how deep in water the player is, and
//!    whether a floor lies within 2 units below.
//! 4. **Swim** (`water`) when at least waist-deep; otherwise
//! 5. **Walk or fly**: half the tick's gravity, a jump if requested,
//!    friction and ground acceleration (`accelerate`) on a floor or capped
//!    air acceleration off it, the move itself (`slide`: slide along what
//!    is hit, step up ledges, stick to slopes going down), re-categorize, and
//!    the other half of gravity.
//!
//! Everything is in Source space: Z up, units of inches, angles in degrees.

use glam::Vec3;
use rl_data::MovementConfig;
use serde::{Deserialize, Serialize};

use crate::collision::{CollisionQuery, CollisionWorld, Hull, Ladder};
use crate::physics::BodyId;
use crate::weapons::WeaponId;

mod accelerate;
mod duck;
mod ground;
mod ladder;
mod slide;
mod water;

/// Buttons held during a tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Buttons(u16);

impl Buttons {
    /// No buttons.
    pub const NONE: Self = Self(0);
    /// Jump (and swim up).
    pub const JUMP: Self = Self(1 << 0);
    /// Crouch.
    pub const DUCK: Self = Self(1 << 1);
    /// Walk slowly instead of running.
    pub const WALK: Self = Self(1 << 2);
    /// Use: grab or let go of a ladder.
    pub const USE: Self = Self(1 << 3);
    /// Primary fire.
    pub const ATTACK: Self = Self(1 << 4);
    /// Secondary fire.
    pub const ATTACK2: Self = Self(1 << 5);
    /// Reload the weapon in hand.
    pub const RELOAD: Self = Self(1 << 6);

    /// Whether every button of `other` is held.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The buttons of both sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The raw bits, for serialisation.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Buttons from raw bits; unknown bits are kept but mean nothing.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// These buttons with `other` held (`value` true) or released.
    #[must_use]
    pub const fn with(self, other: Self, value: bool) -> Self {
        if value {
            Self(self.0 | other.0)
        } else {
            Self(self.0 & !other.0)
        }
    }
}

impl std::ops::BitOr for Buttons {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// One tick of player input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UserCmd {
    /// Forward (+) / backward (−) movement, `-1..=1`.
    pub forward: f32,
    /// Right (+) / left (−) movement, `-1..=1`.
    pub side: f32,
    /// View pitch, degrees; positive looks down (Source convention).
    pub pitch: f32,
    /// View yaw, degrees around +Z; 0 faces +X.
    pub yaw: f32,
    /// Buttons held.
    pub buttons: Buttons,
    /// A weapon to take in hand this tick, if the player chose one.
    pub select: Option<WeaponId>,
    /// The command's number: each player numbers their commands in order.
    /// It seeds the randomness of what the command fires (bullet spread),
    /// so a client predicting its own shots draws the numbers the server
    /// does.
    pub number: u32,
    /// The server tick, with its fraction, of the world the player was
    /// looking at when they gave the command: what they aimed at. Hitscan
    /// shots and swings see other players as they were then (lag
    /// compensation, within `net.ron`'s `max_rewind`). `None` (local play,
    /// bots) means now.
    pub view_tick: Option<f64>,
}

/// How deep in water a player is.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum WaterLevel {
    /// Dry.
    #[default]
    None,
    /// Feet in water.
    Feet,
    /// Waist-deep: swimming.
    Waist,
    /// Fully submerged.
    Eyes,
}

/// Everything about a player that movement reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    /// Position of the player's feet.
    pub origin: Vec3,
    /// Velocity, units/s.
    pub velocity: Vec3,
    /// Standing on a floor.
    pub on_ground: bool,
    /// Normal of the floor stood on (zero in the air).
    pub ground_normal: Vec3,
    /// Fully crouched: the ducked hull is in use.
    pub ducked: bool,
    /// Crouch progress, `0` standing to `1` fully down; drives the eye
    /// height.
    pub duck_amount: f32,
    /// Depth in water.
    pub water_level: WaterLevel,
    /// The ladder being climbed, by index into [`MovementWorld::ladders`].
    pub ladder: Option<u16>,
    /// Buttons held last tick, so jump and use act only on a fresh press.
    /// Jump is also set by a press in the air, so it must be released
    /// before the player can jump again.
    pub old_buttons: Buttons,
}

impl PlayerState {
    /// A player standing still at `origin`.
    #[must_use]
    pub fn at(origin: Vec3) -> Self {
        Self {
            origin,
            velocity: Vec3::ZERO,
            on_ground: false,
            ground_normal: Vec3::ZERO,
            ducked: false,
            duck_amount: 0.0,
            water_level: WaterLevel::None,
            ladder: None,
            old_buttons: Buttons::NONE,
        }
    }

    /// The collision hull currently in use.
    #[must_use]
    pub fn hull(&self, config: &MovementConfig) -> Hull {
        if self.ducked {
            Hull::from(&config.hulls.ducked)
        } else {
            Hull::from(&config.hulls.standing)
        }
    }

    /// Height of the eyes above [`origin`](Self::origin). Eases between the
    /// standing and ducked heights as the player crouches; while ducked in
    /// the air, the raised feet keep the eyes where they were.
    #[must_use]
    pub fn eye_height(&self, config: &MovementConfig) -> f32 {
        let standing = config.hulls.standing.eye_height;
        let ducked = config.hulls.ducked.eye_height;
        if self.ducked {
            return ducked;
        }
        let t = self.duck_amount.clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        standing + (ducked - standing) * eased
    }

    /// Horizontal speed, units/s.
    #[must_use]
    pub fn speed_2d(&self) -> f32 {
        self.velocity.truncate().length()
    }
}

/// Most props one tick's move remembers touching.
pub const MAX_TOUCHES: usize = 4;

/// A prop the player ran into during a tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// The prop.
    pub body: BodyId,
    /// Normal of the prop's surface hit, pointing out of it.
    pub normal: Vec3,
    /// How fast the player was moving into the surface, units/s.
    pub speed: f32,
}

/// Things that happened during a tick, for sounds, effects and damage.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MoveEvents {
    /// The player jumped.
    pub jumped: bool,
    /// The player landed, falling at this speed (units/s, positive).
    pub landed: Option<f32>,
    /// Props walked or flown into (not stood on), fastest contact per
    /// prop; see [`MoveEvents::touches`].
    touches: [Option<Touch>; MAX_TOUCHES],
}

impl MoveEvents {
    /// The props the player ran into this tick.
    pub fn touches(&self) -> impl Iterator<Item = &Touch> {
        self.touches.iter().flatten()
    }

    /// Records running into a prop, keeping the fastest contact with each
    /// and dropping new props once [`MAX_TOUCHES`] are known.
    fn touch(&mut self, touch: Touch) {
        if let Some(known) = self
            .touches
            .iter_mut()
            .flatten()
            .find(|known| known.body == touch.body)
        {
            if touch.speed > known.speed {
                *known = touch;
            }
        } else if let Some(free) = self.touches.iter_mut().find(|slot| slot.is_none()) {
            *free = Some(touch);
        }
    }
}

/// What movement needs from the world: collision plus the map's ladders.
pub trait MovementWorld: CollisionQuery {
    /// The ladders players can climb.
    fn ladders(&self) -> &[Ladder];
    /// Points where a climber may step off a ladder.
    fn ladder_dismounts(&self) -> &[Vec3];

    /// The move, at most `max_distance` long, that takes `hull`, at
    /// `origin`, towards the nearest way out of the movable things it
    /// overlaps (props), if any. It never leads into the map.
    fn penetration(&self, _origin: Vec3, _hull: &Hull, _max_distance: f32) -> Option<Vec3> {
        None
    }
}

impl MovementWorld for CollisionWorld {
    fn ladders(&self) -> &[Ladder] {
        CollisionWorld::ladders(self)
    }

    fn ladder_dismounts(&self) -> &[Vec3] {
        CollisionWorld::ladder_dismounts(self)
    }
}

/// Advances `state` by one tick of `dt` seconds under `cmd`.
#[must_use]
pub fn simulate(
    state: &PlayerState,
    cmd: &UserCmd,
    config: &MovementConfig,
    dt: f32,
    world: &impl MovementWorld,
) -> (PlayerState, MoveEvents) {
    let mut mover = Mover {
        state: *state,
        cmd: *cmd,
        config,
        dt,
        world,
        events: MoveEvents::default(),
    };
    mover.run();
    (mover.state, mover.events)
}

/// Downward speed above which a player is falling hard enough to be "in the
/// air" even over a floor. Source's `NON_JUMP_VELOCITY`: anything rising
/// faster than this cannot be standing on something.
const NON_JUMP_VELOCITY: f32 = 140.0;

/// One tick's worth of movement in progress.
struct Mover<'a, W> {
    state: PlayerState,
    cmd: UserCmd,
    config: &'a MovementConfig,
    dt: f32,
    world: &'a W,
    events: MoveEvents,
}

impl<W: MovementWorld> Mover<'_, W> {
    fn run(&mut self) {
        self.check_velocity();
        self.unstick();
        let falling_speed = (!self.state.on_ground).then_some(-self.state.velocity.z);

        self.update_duck();
        if self.ladder_tick() {
            self.finish_buttons();
            return;
        }

        self.categorize_position();
        if self.state.water_level >= WaterLevel::Waist {
            self.water_move();
            self.categorize_position();
        } else {
            self.walk_or_fly();
        }

        if let Some(speed) = falling_speed
            && self.state.on_ground
            && speed > 0.0
        {
            self.events.landed = Some(speed);
        }
        self.finish_buttons();
    }

    /// Steps 5 of the module docs: gravity, jumping, friction, the move.
    fn walk_or_fly(&mut self) {
        self.start_gravity();
        if self.cmd.buttons.contains(Buttons::JUMP) {
            self.check_jump();
        } else {
            self.set_jump_held(false);
        }
        if self.state.on_ground {
            self.state.velocity.z = 0.0;
            self.friction();
        }
        self.check_velocity();
        if self.state.on_ground {
            self.walk_move();
        } else {
            self.air_move();
        }
        self.categorize_position();
        self.check_velocity();
        self.finish_gravity();
        if self.state.on_ground {
            self.state.velocity.z = 0.0;
        }
    }

    /// Remembers whether use was held, for edge detection next tick.
    fn finish_buttons(&mut self) {
        let held = self.cmd.buttons.contains(Buttons::USE);
        self.state.old_buttons = self.state.old_buttons.with(Buttons::USE, held);
    }

    /// Whether jump counts as still held from an earlier tick.
    fn jump_held(&self) -> bool {
        self.state.old_buttons.contains(Buttons::JUMP)
    }

    fn set_jump_held(&mut self, held: bool) {
        self.state.old_buttons = self.state.old_buttons.with(Buttons::JUMP, held);
    }

    /// Clamps every velocity component to the configured maximum and
    /// discards non-finite values, so one bad input cannot poison the
    /// state.
    fn check_velocity(&mut self) {
        let limit = self.config.max_velocity;
        let mut velocity = self.state.velocity;
        for axis in 0..3 {
            if !velocity[axis].is_finite() {
                velocity[axis] = 0.0;
            }
            velocity[axis] = velocity[axis].clamp(-limit, limit);
        }
        self.state.velocity = velocity;
        if !self.state.origin.is_finite() {
            self.state.origin = Vec3::ZERO;
        }
    }

    /// The first half of the tick's gravity (the second half is applied
    /// after the move, so a jump's arc integrates symmetrically).
    fn start_gravity(&mut self) {
        self.state.velocity.z -= self.config.gravity * 0.5 * self.dt;
    }

    fn finish_gravity(&mut self) {
        self.state.velocity.z -= self.config.gravity * 0.5 * self.dt;
    }

    /// Jumps if allowed: from a floor, or off the surface while wading. A
    /// held button only jumps once unless auto-hop is on.
    fn check_jump(&mut self) {
        let wading = self.state.water_level >= WaterLevel::Feet && self.state.velocity.z <= 0.0;
        if !(self.state.on_ground || wading) {
            // Pressed in the air: it must be released before the next jump.
            self.set_jump_held(true);
            return;
        }
        if self.jump_held() && !self.config.jump.auto_hop {
            return;
        }
        self.state.on_ground = false;
        self.state.ground_normal = Vec3::ZERO;
        self.state.velocity.z += self.config.jump.launch_speed(self.config.gravity);
        self.set_jump_held(true);
        self.events.jumped = true;
    }

    /// Pushes the player out of a prop that came to overlap them, at most
    /// `unstick_speed` for the tick, so they are never held inside it.
    ///
    /// The way out is the shortest one the world offers, taken only as far
    /// as the map allows, so the player is never pushed into a wall.
    fn unstick(&mut self) {
        let hull = self.hull();
        let reach = self.config.unstick_speed * self.dt;
        if let Some(step) = self.world.penetration(self.state.origin, &hull, reach) {
            self.state.origin += step;
        }
    }

    /// The hull in use.
    fn hull(&self) -> Hull {
        self.state.hull(self.config)
    }

    /// Sweeps the current hull from `start` to `end` against player solids.
    fn trace(&self, start: Vec3, end: Vec3) -> crate::collision::Trace {
        self.world.trace_hull(
            start,
            end,
            &self.hull(),
            crate::collision::contents::MASK_PLAYER_SOLID,
        )
    }

    /// Top speed this tick: running or walking, slowed while crouching.
    fn max_speed(&self) -> f32 {
        let ground = &self.config.ground;
        let mut speed = if self.cmd.buttons.contains(Buttons::WALK) {
            ground.walk_speed
        } else {
            ground.run_speed
        };
        let crouching =
            self.state.ducked || (self.cmd.buttons.contains(Buttons::DUCK) && self.state.on_ground);
        if crouching {
            speed *= self.config.duck.speed_scale;
        }
        speed
    }
}

/// The horizontal forward and right directions of a view yaw (degrees).
fn flat_axes(yaw: f32) -> (Vec3, Vec3) {
    let (sin, cos) = yaw.to_radians().sin_cos();
    (Vec3::new(cos, sin, 0.0), Vec3::new(sin, -cos, 0.0))
}

/// The direction a view looks along (degrees; positive pitch looks down,
/// yaw 0 faces +X).
#[must_use]
pub fn view_direction(pitch: f32, yaw: f32) -> Vec3 {
    view_axes(pitch, yaw).0
}

/// The full forward and right directions of a view (degrees; positive pitch
/// looks down).
fn view_axes(pitch: f32, yaw: f32) -> (Vec3, Vec3) {
    let (sin_pitch, cos_pitch) = pitch.to_radians().sin_cos();
    let (sin_yaw, cos_yaw) = yaw.to_radians().sin_cos();
    (
        Vec3::new(cos_pitch * cos_yaw, cos_pitch * sin_yaw, -sin_pitch),
        Vec3::new(sin_yaw, -cos_yaw, 0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_axes_follow_source_conventions() {
        let (forward, right) = flat_axes(0.0);
        assert!(forward.abs_diff_eq(Vec3::X, 1e-6));
        assert!(right.abs_diff_eq(Vec3::NEG_Y, 1e-6), "+Y is left");
        let (forward, _) = view_axes(90.0, 0.0);
        assert!(
            forward.abs_diff_eq(Vec3::NEG_Z, 1e-6),
            "positive pitch looks down"
        );
    }

    #[test]
    fn touches_keep_the_fastest_contact_per_prop() {
        let mut events = MoveEvents::default();
        let touch = |body, speed| Touch {
            body: BodyId::from_index(body),
            normal: Vec3::X,
            speed,
        };
        events.touch(touch(1, 10.0));
        events.touch(touch(1, 30.0));
        events.touch(touch(1, 20.0));
        for body in 2..10 {
            events.touch(touch(body, 1.0));
        }
        let touches: Vec<_> = events.touches().collect();
        assert_eq!(touches.len(), MAX_TOUCHES);
        assert_eq!(touches[0].speed, 30.0);
    }

    #[test]
    fn buttons_combine() {
        let held = Buttons::JUMP | Buttons::DUCK;
        assert!(held.contains(Buttons::JUMP));
        assert!(held.contains(Buttons::DUCK | Buttons::JUMP));
        assert!(!held.contains(Buttons::WALK));
        assert_eq!(Buttons::from_bits(held.bits()), held);
        assert_eq!(held.with(Buttons::JUMP, false), Buttons::DUCK);
        assert_eq!(Buttons::NONE.with(Buttons::USE, true), Buttons::USE);
    }
}
