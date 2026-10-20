//! Movers: the brush entities that map logic and players change.
//!
//! - **Doors** (`func_door`, `func_door_rotating`, `func_movelinear`) slide
//!   along a direction or turn about an axis between a closed and an open
//!   placement, at their `speed`, waiting `wait` seconds open before closing
//!   again (`-1`: until told to).
//! - **Buttons** (`func_button`) are pressed by use, touch or damage as
//!   their flags allow, or by the `Press` input; a moving button slides in
//!   and back out like a door.
//! - **Brushes** (`func_brush`, `func_wall_toggle` and any other named
//!   solid brush entity) are shown or hidden, solid or passable.
//! - **Breakables** (`func_breakable`, `func_breakable_surf`) lose health
//!   to damage and are gone when it runs out.
//!
//! A mover's progress between closed and open is one number,
//! [`Mover::position`] (`0..=1`), so its whole visible state replicates in a
//! few bytes and a client can interpolate it. Movers parented to another
//! mover (`parentname`) are carried along by it.
//!
//! The rules follow Source's documented behaviour for these entities, with
//! two simplifications: rotating doors always swing towards their open
//! placement (Source swings use-opened doors away from the user), and a
//! breakable is gone as a whole (glass in Source shatters pane by pane).

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use source_formats::entities::MapEntity;

use crate::collision::rotation_from_angles;
use crate::simulation::PlayerId;

/// `func_door` and `func_door_rotating` spawnflags.
mod door_flags {
    /// Starts at its open placement, which then counts as closed.
    pub(super) const START_OPEN: u32 = 1;
    /// A rotating door turns the other way.
    pub(super) const REVERSE: u32 = 2;
    /// Players pass through it.
    pub(super) const PASSABLE: u32 = 8;
    /// Never closes by itself.
    pub(super) const NO_AUTO_RETURN: u32 = 32;
    /// A rotating door turns about the X axis (roll).
    pub(super) const ROLL: u32 = 64;
    /// A rotating door turns about the Y axis (pitch).
    pub(super) const PITCH: u32 = 128;
    /// Players open it with use.
    pub(super) const USE_OPENS: u32 = 256;
    /// Players open it by touching it.
    pub(super) const TOUCH_OPENS: u32 = 1024;
    /// Starts locked.
    pub(super) const LOCKED: u32 = 2048;
}

/// `func_button` spawnflags.
mod button_flags {
    /// Stays where it is when pressed.
    pub(super) const DONT_MOVE: u32 = 1;
    /// Pressed by touching it.
    pub(super) const TOUCH_ACTIVATES: u32 = 256;
    /// Pressed by damaging it.
    pub(super) const DAMAGE_ACTIVATES: u32 = 512;
    /// Pressed with use.
    pub(super) const USE_ACTIVATES: u32 = 1024;
    /// Starts locked.
    pub(super) const LOCKED: u32 = 2048;
}

/// `func_breakable` spawnflag: only the `Break` input breaks it.
const BREAK_ON_TRIGGER_ONLY: u32 = 1;

/// Default speed of doors (units or degrees per second) and buttons.
const DOOR_SPEED: f32 = 100.0;
const BUTTON_SPEED: f32 = 5.0;
/// Default seconds a door stays open, and a button pressed.
const DOOR_WAIT: f32 = 4.0;
const BUTTON_WAIT: f32 = 3.0;

/// When a mover blocks players, shots and props.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Solidity {
    /// Never (a passable door, a `func_brush` of solidity 1).
    Never,
    /// While enabled (shown).
    WhenEnabled,
    /// Always, even hidden (a `func_brush` of solidity 2).
    Always,
}

/// Whether a mover is there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presence {
    /// Shown, and solid as its solidity says.
    Enabled,
    /// Hidden (a brush switched off), and solid only if always solid.
    Disabled,
    /// Killed or broken: gone for good.
    Gone,
}

/// What players can do to a mover to open or press it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Activation {
    by_use: bool,
    by_touch: bool,
    by_damage: bool,
}

/// What kind of mover an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoverClass {
    /// Opens and closes.
    Door,
    /// Is pressed.
    Button,
    /// Is shown and hidden.
    Brush,
    /// Breaks.
    Breakable,
}

/// Where a mover travels between closed (0) and open (1).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Path {
    /// Nowhere.
    Fixed,
    /// From one origin to another, angles unchanged.
    Linear { closed: Vec3, open: Vec3 },
    /// From one set of angles to another about the origin.
    Angular { closed: [f32; 3], open: [f32; 3] },
}

impl Path {
    /// How far the whole path is: units, or degrees of the largest turn.
    fn length(&self) -> f32 {
        match *self {
            Self::Fixed => 0.0,
            Self::Linear { closed, open } => closed.distance(open),
            Self::Angular { closed, open } => (0..3)
                .map(|axis| (open[axis] - closed[axis]).abs())
                .fold(0.0, f32::max),
        }
    }
}

/// Where a mover is in its cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// At rest, closed (a button: ready).
    Closed,
    /// Moving open (a button: in).
    Opening,
    /// At rest, open since tick `since` (a button: pressed).
    Open {
        /// The tick it got there.
        since: u64,
    },
    /// Moving closed.
    Closing,
}

/// The part of a mover that changes, as replicated to clients.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MoverState {
    /// Progress from closed (0) to open (1).
    pub position: f32,
    /// Whether it blocks anything.
    pub solid: bool,
    /// Whether it is drawn.
    pub visible: bool,
}

/// A brush entity map logic and players change. See the [module
/// docs](self).
#[derive(Debug, Clone)]
pub struct Mover {
    /// Its BSP model.
    pub model: usize,
    /// What it is.
    pub class: MoverClass,
    /// The entity's origin and angles as the map placed it.
    origin: Vec3,
    angles: [f32; 3],
    path: Path,
    /// Path fraction per second.
    rate: f32,
    /// Progress from closed (0) to open (1).
    pub position: f32,
    /// Where it is in its cycle.
    pub phase: Phase,
    /// Seconds open before closing by itself; `None` stays open.
    wait: Option<f32>,
    /// Locked: players cannot use or touch it open.
    pub locked: bool,
    presence: Presence,
    solidity: Solidity,
    /// Damage per tick to a player it is blocked by.
    pub crush_damage: f32,
    /// Whether, blocked, it goes back the way it came.
    reverses: bool,
    activation: Activation,
    /// A breakable's health; `None` when damage cannot break it.
    pub health: Option<f32>,
    /// The mover it moves with, by index, once resolved.
    pub parent: Option<usize>,
    /// Its parent's name, lower case, until resolved.
    pub parent_name: Option<String>,
    /// The player who last set it going.
    pub activator: Option<PlayerId>,
}

impl Mover {
    /// The mover an entity with BSP model `model` makes, or `None` if its
    /// class does not move, break or toggle. `size` is the model's size,
    /// which a sliding door's travel is measured by.
    #[must_use]
    pub fn from_entity(entity: &MapEntity, model: usize, size: Vec3) -> Option<Self> {
        let flags = entity.number("spawnflags").map_or(0, |flags| flags as u32);
        let origin = Vec3::from(entity.origin);
        let angles = entity.angles;
        let mut mover = Self {
            model,
            class: MoverClass::Brush,
            origin,
            angles,
            path: Path::Fixed,
            rate: 0.0,
            position: 0.0,
            phase: Phase::Closed,
            wait: None,
            locked: false,
            presence: if entity.property("StartDisabled") == Some("1") {
                Presence::Disabled
            } else {
                Presence::Enabled
            },
            solidity: Solidity::WhenEnabled,
            crush_damage: entity.number("dmg").unwrap_or(0.0).max(0.0),
            reverses: false,
            activation: Activation::default(),
            health: None,
            parent: None,
            parent_name: entity
                .property("parentname")
                .filter(|name| !name.is_empty())
                .map(str::to_ascii_lowercase),
            activator: None,
        };
        let wait = |default: f32| {
            let wait = entity.number("wait").unwrap_or(default);
            (wait >= 0.0).then_some(wait)
        };
        match entity.classname.as_str() {
            "func_door" | "func_movelinear" | "func_door_rotating" => {
                mover.class = MoverClass::Door;
                mover.path = if entity.classname == "func_door_rotating" {
                    rotating_path(entity, flags)
                } else {
                    sliding_path(entity, size)
                };
                if flags & door_flags::START_OPEN != 0 {
                    mover.path = mover.path.reversed();
                }
                let speed = entity.number("speed").unwrap_or(DOOR_SPEED);
                mover.set_speed(speed);
                mover.wait = wait(DOOR_WAIT).filter(|_| flags & door_flags::NO_AUTO_RETURN == 0);
                mover.locked = flags & door_flags::LOCKED != 0;
                if flags & door_flags::PASSABLE != 0 {
                    mover.solidity = Solidity::Never;
                }
                mover.reverses =
                    mover.wait.is_some() && entity.property("forceclosed") != Some("1");
                mover.activation = Activation {
                    by_use: flags & door_flags::USE_OPENS != 0,
                    by_touch: flags & door_flags::TOUCH_OPENS != 0,
                    by_damage: false,
                };
            }
            "func_button" | "func_rot_button" => {
                mover.class = MoverClass::Button;
                if flags & button_flags::DONT_MOVE == 0 {
                    mover.path = sliding_path(entity, size);
                }
                mover.set_speed(entity.number("speed").unwrap_or(BUTTON_SPEED));
                mover.wait = wait(BUTTON_WAIT);
                mover.locked = flags & button_flags::LOCKED != 0;
                mover.activation = Activation {
                    by_use: flags & button_flags::USE_ACTIVATES != 0,
                    by_touch: flags & button_flags::TOUCH_ACTIVATES != 0,
                    by_damage: flags & button_flags::DAMAGE_ACTIVATES != 0,
                };
            }
            "func_breakable" | "func_breakable_surf" => {
                mover.class = MoverClass::Breakable;
                mover.health = entity
                    .number("health")
                    .filter(|&health| health > 0.0 && flags & BREAK_ON_TRIGGER_ONLY == 0);
            }
            "func_brush" => {
                mover.solidity = match entity.number("Solidity") {
                    Some(1.0) => Solidity::Never,
                    Some(2.0) => Solidity::Always,
                    _ => Solidity::WhenEnabled,
                };
            }
            _ => {}
        }
        Some(mover)
    }

    /// Sets the speed: units (or degrees) per second along the path.
    pub fn set_speed(&mut self, speed: f32) {
        let length = self.path.length();
        self.rate = if length > 0.0 && speed > 0.0 {
            speed / length
        } else {
            f32::INFINITY
        };
    }

    /// Whether it blocks players and shots now.
    #[must_use]
    pub fn solid(&self) -> bool {
        match (self.presence, self.solidity) {
            (Presence::Gone, _)
            | (_, Solidity::Never)
            | (Presence::Disabled, Solidity::WhenEnabled) => false,
            (Presence::Enabled, Solidity::WhenEnabled) | (_, Solidity::Always) => true,
        }
    }

    /// Whether it is drawn now.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.presence == Presence::Enabled
    }

    /// The replicated part of its state.
    #[must_use]
    pub fn state(&self) -> MoverState {
        MoverState {
            position: self.position,
            solid: self.solid(),
            visible: self.visible(),
        }
    }

    /// Takes on a replicated state (a client's replica, which runs no
    /// logic of its own and only shows what the server says).
    pub fn restore(&mut self, state: MoverState) {
        self.position = state.position.clamp(0.0, 1.0);
        self.presence = if state.visible {
            Presence::Enabled
        } else {
            Presence::Disabled
        };
        self.solidity = if state.solid {
            Solidity::Always
        } else {
            Solidity::Never
        };
    }

    /// Its own placement at `position`, before any parent's motion:
    /// origin and rotation.
    #[must_use]
    pub fn local_pose(&self, position: f32) -> (Vec3, Quat) {
        let position = position.clamp(0.0, 1.0);
        match self.path {
            Path::Fixed => (self.origin, quat_from_angles(self.angles)),
            Path::Linear { closed, open } => {
                (closed.lerp(open, position), quat_from_angles(self.angles))
            }
            Path::Angular { closed, open } => {
                let angles = std::array::from_fn(|axis| {
                    closed[axis] + (open[axis] - closed[axis]) * position
                });
                (self.origin, quat_from_angles(angles))
            }
        }
    }

    /// Where it starts: its placement when closed.
    #[must_use]
    pub fn rest_pose(&self) -> (Vec3, Quat) {
        self.local_pose(0.0)
    }

    /// Whether, once open, it stays open until told to close: use then
    /// closes it again.
    #[must_use]
    pub fn stays_open(&self) -> bool {
        self.wait.is_none()
    }

    /// Whether it is moving.
    #[must_use]
    pub fn moving(&self) -> bool {
        matches!(self.phase, Phase::Opening | Phase::Closing)
    }

    /// Where a tick of `dt` seconds would take it, if it is moving.
    #[must_use]
    pub fn next_position(&self, dt: f32) -> Option<f32> {
        let step = self.rate * dt;
        match self.phase {
            Phase::Opening => Some((self.position + step).min(1.0)),
            Phase::Closing => Some((self.position - step).max(0.0)),
            Phase::Closed | Phase::Open { .. } => None,
        }
    }

    /// Moves to `position` on tick `tick`; returns the output to fire if it
    /// got to the end of its travel.
    pub fn arrive(&mut self, position: f32, tick: u64) -> Option<&'static str> {
        self.position = position;
        match self.phase {
            Phase::Opening if position >= 1.0 => {
                self.phase = Phase::Open { since: tick };
                Some(self.ends().0)
            }
            Phase::Closing if position <= 0.0 => {
                self.phase = Phase::Closed;
                Some(self.ends().1)
            }
            _ => None,
        }
    }

    /// The outputs fired on getting fully open and fully closed.
    fn ends(&self) -> (&'static str, &'static str) {
        match self.class {
            MoverClass::Button => ("onin", "onout"),
            _ => ("onfullyopen", "onfullyclosed"),
        }
    }

    /// Starts opening; returns the output to fire, if it was not open or
    /// opening already.
    pub fn open(&mut self, activator: Option<PlayerId>, tick: u64) -> Option<&'static str> {
        if matches!(self.phase, Phase::Opening | Phase::Open { .. }) || self.gone() {
            return None;
        }
        self.activator = activator;
        self.phase = Phase::Opening;
        if self.rate.is_infinite() {
            self.position = 1.0;
            self.phase = Phase::Open { since: tick };
        }
        Some(match self.class {
            MoverClass::Button => "onpressed",
            _ => "onopen",
        })
    }

    /// Starts closing; returns the output to fire, if it was not closed or
    /// closing already.
    pub fn close(&mut self, activator: Option<PlayerId>) -> Option<&'static str> {
        if matches!(self.phase, Phase::Closing | Phase::Closed) || self.gone() {
            return None;
        }
        self.activator = activator;
        self.phase = Phase::Closing;
        if self.rate.is_infinite() {
            self.position = 0.0;
            self.phase = Phase::Closed;
        }
        Some(match self.class {
            MoverClass::Button => "onout",
            _ => "onclose",
        })
    }

    /// Opens a closed (or closing) mover, closes an open one.
    pub fn toggle(&mut self, activator: Option<PlayerId>, tick: u64) -> Option<&'static str> {
        match self.phase {
            Phase::Closed | Phase::Closing => self.open(activator, tick),
            Phase::Open { .. } | Phase::Opening => self.close(activator),
        }
    }

    /// Closes it if it has been open its `wait` by tick `tick` (`dt`
    /// seconds a tick); returns the output to fire.
    pub fn expire(&mut self, tick: u64, dt: f32) -> Option<&'static str> {
        let Phase::Open { since } = self.phase else {
            return None;
        };
        let wait = self.wait?;
        ((tick - since) as f32 * dt >= wait).then(|| self.close(None))?
    }

    /// Something stands in its way: it goes back the way it came, if it
    /// does that. Returns the output to fire.
    pub fn blocked(&mut self, tick: u64) -> Option<&'static str> {
        let output = match self.phase {
            Phase::Opening => "onblockedopening",
            Phase::Closing => "onblockedclosing",
            _ => return None,
        };
        if self.reverses {
            match self.phase {
                Phase::Opening => {
                    self.close(self.activator);
                }
                _ => {
                    self.open(self.activator, tick);
                }
            }
        }
        Some(output)
    }

    /// A player used it: whether that presses or opens it, unlocked.
    #[must_use]
    pub fn used_by_players(&self) -> bool {
        self.activation.by_use
    }

    /// Whether touching it presses or opens it.
    #[must_use]
    pub fn touched_by_players(&self) -> bool {
        self.activation.by_touch
    }

    /// Whether damage presses it.
    #[must_use]
    pub fn pressed_by_damage(&self) -> bool {
        self.activation.by_damage
    }

    /// Shows it (and makes it solid as its kind is), unless it is gone.
    pub fn enable(&mut self) {
        if self.presence == Presence::Disabled {
            self.presence = Presence::Enabled;
        }
    }

    /// Hides it (and makes it passable unless always solid), unless it is
    /// gone.
    pub fn disable(&mut self) {
        if self.presence == Presence::Enabled {
            self.presence = Presence::Disabled;
        }
    }

    /// Whether it is enabled.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.presence == Presence::Enabled
    }

    /// Removes it for good.
    pub fn kill(&mut self) {
        self.presence = Presence::Gone;
        self.phase = Phase::Closed;
    }

    /// Whether it was killed or broken.
    #[must_use]
    pub fn gone(&self) -> bool {
        self.presence == Presence::Gone
    }

    /// Takes `amount` damage; returns whether it broke.
    pub fn damage(&mut self, amount: f32) -> bool {
        if self.gone() {
            return false;
        }
        let Some(health) = self.health.as_mut() else {
            return false;
        };
        *health -= amount;
        if *health <= 0.0 {
            self.kill();
            return true;
        }
        false
    }
}

impl Path {
    /// The same path walked the other way.
    fn reversed(self) -> Self {
        match self {
            Self::Fixed => Self::Fixed,
            Self::Linear { closed, open } => Self::Linear {
                closed: open,
                open: closed,
            },
            Self::Angular { closed, open } => Self::Angular {
                closed: open,
                open: closed,
            },
        }
    }
}

/// A door or button sliding along `movedir`: as far as the model is long
/// in that direction, less `lip` (Source's travel).
fn sliding_path(entity: &MapEntity, size: Vec3) -> Path {
    let movedir = entity
        .property("movedir")
        .and_then(parse_angles)
        .unwrap_or([0.0; 3]);
    let direction = rotation_from_angles(movedir) * Vec3::X;
    let lip = entity.number("lip").unwrap_or(0.0);
    let travel = (direction.abs().dot(size) - lip).max(0.0);
    let closed = Vec3::from(entity.origin);
    Path::Linear {
        closed,
        open: closed + direction * travel,
    }
}

/// A rotating door's turn: `distance` degrees about its axis.
fn rotating_path(entity: &MapEntity, flags: u32) -> Path {
    // Angles are [pitch, yaw, roll]: yaw unless flagged otherwise.
    let axis = if flags & door_flags::ROLL != 0 {
        2
    } else {
        usize::from(flags & door_flags::PITCH == 0)
    };
    let mut distance = entity.number("distance").unwrap_or(90.0);
    if flags & door_flags::REVERSE != 0 {
        distance = -distance;
    }
    let closed = entity.angles;
    let mut open = closed;
    open[axis] += distance;
    Path::Angular { closed, open }
}

/// `"pitch yaw roll"` as three numbers.
fn parse_angles(text: &str) -> Option<[f32; 3]> {
    let mut parts = text.split_whitespace().map(str::parse::<f32>);
    let angles = [
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ];
    angles
        .iter()
        .all(|angle| angle.is_finite())
        .then_some(angles)
}

/// Source angles as a rotation.
#[must_use]
pub fn quat_from_angles(angles: [f32; 3]) -> Quat {
    Quat::from_mat3(&rotation_from_angles(angles))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 0.1;

    fn door(keys: &[(&str, &str)]) -> Mover {
        let mut entity = MapEntity::new("func_door", [0.0, 0.0, 100.0]);
        for (key, value) in keys {
            entity = entity.with(key, value);
        }
        Mover::from_entity(&entity, 1, Vec3::new(64.0, 8.0, 128.0)).unwrap()
    }

    /// Runs `mover` until it stops, returning the outputs fired.
    fn run(mover: &mut Mover, tick: &mut u64) -> Vec<&'static str> {
        let mut fired = Vec::new();
        for _ in 0..1000 {
            *tick += 1;
            let Some(next) = mover.next_position(DT) else {
                break;
            };
            fired.extend(mover.arrive(next, *tick));
        }
        fired
    }

    #[test]
    fn a_sliding_door_travels_its_length_less_the_lip() {
        let mut door = door(&[("movedir", "-90 0 0"), ("lip", "8"), ("speed", "60")]);
        assert_eq!(door.local_pose(0.0).0, Vec3::new(0.0, 0.0, 100.0));
        let open = door.local_pose(1.0).0;
        assert!(
            (open - Vec3::new(0.0, 0.0, 220.0)).length() < 1e-3,
            "{open}"
        );

        let mut tick = 0;
        assert_eq!(door.open(None, tick), Some("onopen"));
        assert_eq!(door.open(None, tick), None, "already opening");
        assert_eq!(run(&mut door, &mut tick), ["onfullyopen"]);
        // 120 units at 60 units/s: 2 s, 20 ticks.
        assert_eq!(tick, 21);
        assert_eq!(door.phase, Phase::Open { since: 20 });
    }

    #[test]
    fn a_door_closes_by_itself_after_its_wait_unless_told_to_stay() {
        let mut door = door(&[("wait", "1"), ("speed", "6400")]);
        let mut tick = 0;
        door.open(None, tick);
        run(&mut door, &mut tick);
        let opened = tick;
        assert_eq!(door.expire(opened + 5, DT), None, "half a second");
        assert_eq!(door.expire(opened + 10, DT), Some("onclose"));
        assert!(door.moving());

        let mut stays = self::door(&[("wait", "-1"), ("speed", "6400")]);
        stays.open(None, 0);
        run(&mut stays, &mut tick);
        assert_eq!(stays.expire(tick + 10_000, DT), None);
    }

    #[test]
    fn a_door_that_starts_open_opens_back_to_its_placement() {
        let door = door(&[("spawnflags", "1"), ("movedir", "0 90 0")]);
        let (closed, _) = door.local_pose(0.0);
        let (open, _) = door.local_pose(1.0);
        assert!(
            (closed - Vec3::new(0.0, 8.0, 100.0)).length() < 1e-3,
            "{closed}"
        );
        assert_eq!(open, Vec3::new(0.0, 0.0, 100.0));
    }

    #[test]
    fn rotating_doors_turn_about_the_flagged_axis() {
        let entity = MapEntity::new("func_door_rotating", [0.0; 3])
            .with("distance", "90")
            .with("spawnflags", "128");
        let door = Mover::from_entity(&entity, 1, Vec3::ONE).unwrap();
        let (_, open) = door.local_pose(1.0);
        let forward = open * Vec3::X;
        assert!(
            (forward - Vec3::NEG_Z).length() < 1e-4,
            "pitch tips forward down: {forward}"
        );

        let reversed = MapEntity::new("func_door_rotating", [0.0; 3]).with("spawnflags", "2");
        let door = Mover::from_entity(&reversed, 1, Vec3::ONE).unwrap();
        let (_, open) = door.local_pose(1.0);
        assert!((open * Vec3::X - Vec3::NEG_Y).length() < 1e-4, "yaw −90°");
    }

    #[test]
    fn a_blocked_door_turns_back_unless_it_waits_for_ever_or_is_forced() {
        let mut door = door(&[("wait", "2")]);
        door.open(None, 0);
        assert_eq!(door.blocked(1), Some("onblockedopening"));
        assert_eq!(door.phase, Phase::Closing);

        let mut stubborn = self::door(&[("wait", "-1")]);
        stubborn.open(None, 0);
        stubborn.blocked(1);
        assert_eq!(stubborn.phase, Phase::Opening);

        let mut forced = self::door(&[("forceclosed", "1")]);
        forced.open(None, 0);
        forced.blocked(1);
        assert_eq!(forced.phase, Phase::Opening);
    }

    #[test]
    fn buttons_report_presses_and_reset_after_their_wait() {
        let entity = MapEntity::new("func_button", [0.0; 3])
            .with("spawnflags", "1025")
            .with("wait", "1");
        let mut button = Mover::from_entity(&entity, 1, Vec3::ONE).unwrap();
        assert!(button.used_by_players() && !button.touched_by_players());
        assert_eq!(button.open(None, 10), Some("onpressed"));
        assert_eq!(
            button.phase,
            Phase::Open { since: 10 },
            "a fixed button is in at once"
        );
        assert_eq!(button.open(None, 11), None, "cannot be pressed again yet");
        assert_eq!(button.expire(20, DT), Some("onout"));
        assert_eq!(button.phase, Phase::Closed);
    }

    #[test]
    fn breakables_break_once_their_health_is_gone() {
        let glass = MapEntity::new("func_breakable_surf", [0.0; 3]).with("health", "5");
        let mut glass = Mover::from_entity(&glass, 1, Vec3::ONE).unwrap();
        assert!(glass.solid() && glass.visible());
        assert!(!glass.damage(4.0));
        assert!(glass.damage(4.0));
        assert!(!glass.solid() && !glass.visible());
        assert!(!glass.damage(4.0), "breaks once");

        let only_on_trigger = MapEntity::new("func_breakable", [0.0; 3])
            .with("health", "5")
            .with("spawnflags", "1");
        let mut sturdy = Mover::from_entity(&only_on_trigger, 1, Vec3::ONE).unwrap();
        assert!(!sturdy.damage(100.0));
        assert!(sturdy.solid());
    }

    #[test]
    fn brushes_toggle_and_some_never_collide() {
        let entity = MapEntity::new("func_brush", [0.0; 3]).with("StartDisabled", "1");
        let mut brush = Mover::from_entity(&entity, 1, Vec3::ONE).unwrap();
        assert!(!brush.solid() && !brush.visible());
        brush.enable();
        assert!(brush.solid() && brush.visible());

        let ghost = MapEntity::new("func_brush", [0.0; 3]).with("Solidity", "1");
        let ghost = Mover::from_entity(&ghost, 1, Vec3::ONE).unwrap();
        assert!(!ghost.solid() && ghost.visible());
    }

    #[test]
    fn replicated_state_round_trips() {
        let mut door = door(&[]);
        door.position = 0.25;
        let mut copy = self::door(&[]);
        copy.restore(door.state());
        assert_eq!(copy.state(), door.state());
        door.kill();
        copy.restore(door.state());
        assert_eq!(copy.state(), door.state());
        assert!(!copy.visible());
    }
}
