//! Scripted input for non-interactive runs: screenshots of the player
//! moving, fighting and using the Manipulator, and tests of the running
//! client.
//!
//! An [`InputScript`] adjusts the local player's command each tick by app
//! time:
//!
//! - **weapon**: from the start, take a weapon in hand (given, with plenty
//!   of ammo, if not carried);
//! - **walk**: hold forward for a while;
//! - **grab**: take the Manipulator, go to the grabbable prop nearest the
//!   player — the player is moved to a free spot a short way from it, facing
//!   it — and hold secondary fire at it until it is picked up;
//! - **fire**: press primary fire once (throwing whatever is held);
//! - **shoot**: go to the living player nearest the local one — a short
//!   way off, facing them — and hold primary fire at them for a while;
//! - **visit**: go and look at the pickup nearest the player;
//! - **use**: go to the nearest of the map's movers of a class (a button,
//!   a door), look at it and press use once;
//! - **teleport**: stand at a point, held there, looking a given way.
//!
//! Only walking and pressing fire are plain input; the other steps move
//! the player or hand them a weapon, which only a game this client hosts
//! allows. Joining a server, those steps are skipped.
//!
//! It is a debugging aid only; normal play never inserts one.

use bevy::prelude::*;
use rl_sim::collision::contents::{MASK_PLAYER_SOLID, MASK_SHOT};
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::BodyId;
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::weapons::WeaponId;

/// How far from the target prop the grab step places the player, units.
const GRAB_STAND_OFF: f32 = 110.0;
/// How far from the target player the shoot step places the player, units.
const SHOOT_STAND_OFF: f32 = 320.0;
/// How far from a pickup the visit step places the player, units.
const VISIT_STAND_OFF: f32 = 90.0;
/// How far from a mover's centre the use step places the player, units:
/// within use range of a wall button's face.
const USE_STAND_OFF: f32 = 56.0;
/// Seconds the use step faces the mover before pressing use.
const USE_DELAY: f32 = 0.25;
/// Rounds of the scripted weapon's ammo given with it.
const SCRIPT_AMMO: u32 = 1000;
/// Directions tried around a target when looking for somewhere to stand.
const STAND_DIRECTIONS: usize = 16;
/// A place to stand needs a floor at most this far below the feet, units.
const FLOOR_REACH: f32 = 64.0;

/// Scripted actions by app time, in seconds.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct InputScript {
    /// Hold forward from `.0` for `.1` seconds.
    pub walk: Option<(f32, f32)>,
    /// From this time, go to the nearest grabbable prop and pick it up.
    pub grab_at: Option<f32>,
    /// At this time, press primary fire once.
    pub fire_at: Option<f32>,
    /// The weapon (by id) to take in hand from the start.
    pub weapon: Option<String>,
    /// From `.0` for `.1` seconds, shoot the nearest other player.
    pub shoot: Option<(f32, f32)>,
    /// From `.0` for `.1` seconds, hold primary fire wherever the view is.
    pub hold_fire: Option<(f32, f32)>,
    /// From this time, look at the nearest pickup.
    pub visit_at: Option<f32>,
    /// From `.0`, go to the nearest mover of class `.1` and use it.
    pub use_mover: Option<(f32, String)>,
    /// Stand at this point (Source space), held there every tick, looking
    /// at `(pitch, yaw)`.
    pub teleport: Option<([f32; 3], (f32, f32))>,
    progress: Progress,
}

impl InputScript {
    /// A script of the given actions.
    #[must_use]
    pub fn new(walk: Option<(f32, f32)>, grab_at: Option<f32>, fire_at: Option<f32>) -> Self {
        Self {
            walk,
            grab_at,
            fire_at,
            ..Self::default()
        }
    }

    /// This script, also taking `weapon` in hand from the start.
    #[must_use]
    pub fn with_weapon(mut self, weapon: Option<String>) -> Self {
        self.weapon = weapon;
        self
    }

    /// This script, also going to look at the nearest pickup at `at`.
    #[must_use]
    pub fn with_visit(mut self, at: Option<f32>) -> Self {
        self.visit_at = at;
        self
    }

    /// This script, also using the nearest mover of a class (`.1`) at
    /// `.0`.
    #[must_use]
    pub fn with_use(mut self, use_mover: Option<(f32, String)>) -> Self {
        self.use_mover = use_mover;
        self
    }

    /// This script, also standing at a point looking a given way.
    #[must_use]
    pub fn with_teleport(mut self, teleport: Option<([f32; 3], (f32, f32))>) -> Self {
        self.teleport = teleport;
        self
    }

    /// This script, also holding primary fire from `start` for `duration`
    /// seconds.
    #[must_use]
    pub fn with_fire_held(mut self, hold: Option<(f32, f32)>) -> Self {
        self.hold_fire = hold;
        self
    }

    /// This script, also shooting the nearest player from `start` for
    /// `duration` seconds.
    #[must_use]
    pub fn with_shooting(mut self, shoot: Option<(f32, f32)>) -> Self {
        self.shoot = shoot;
        self
    }
}

/// How far the script has got.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Progress {
    /// Whether the scripted weapon was handed over.
    armed: bool,
    /// Once the grab step has looked for it: the prop being grabbed and
    /// the view angles aimed at it, if one was found.
    target: Search<(BodyId, f32, f32)>,
    /// Whether primary fire was pressed.
    fired: bool,
    /// Once the shoot step has looked for one: the player being shot at
    /// and the view angles aimed at them, if one was found.
    victim: Search<(PlayerId, f32, f32)>,
    /// Once the visit step has looked for one: the view angles aimed at
    /// the nearest pickup.
    sight: Search<(f32, f32)>,
    /// Once the use step has looked for one: the view angles aimed at the
    /// mover.
    lever: Search<(f32, f32)>,
    /// Whether use was pressed.
    used: bool,
}

/// A one-time search for something a step acts on.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Search<T> {
    /// Not looked for yet.
    #[default]
    Pending,
    /// Looked for: found, or not.
    Done(Option<T>),
}

impl<T: Copy> Search<T> {
    /// The result, searching with `find` the first time.
    fn find_once(&mut self, find: impl FnOnce() -> Option<T>) -> Option<T> {
        if let Self::Done(found) = *self {
            return found;
        }
        let found = find();
        *self = Self::Done(found);
        found
    }
}

/// Adjusts `cmd` for app time `now` with the steps that are plain input:
/// walking and pressing fire.
pub(crate) fn apply_input(script: &mut InputScript, now: f32, cmd: &mut UserCmd) {
    if let Some((start, duration)) = script.walk
        && (start..start + duration).contains(&now)
    {
        cmd.forward = 1.0;
    }
    if let Some((start, duration)) = script.hold_fire
        && (start..start + duration).contains(&now)
    {
        cmd.buttons = cmd.buttons | Buttons::ATTACK;
    }
    if !script.progress.fired && script.fire_at.is_some_and(|at| now >= at) {
        script.progress.fired = true;
        cmd.buttons = cmd.buttons | Buttons::ATTACK;
    }
}

/// Adjusts `cmd` for app time `now` with every step, acting on the hosted
/// game's simulation `sim`.
pub(crate) fn apply(
    script: &mut InputScript,
    now: f32,
    player: PlayerId,
    sim: &mut Simulation,
    cmd: &mut UserCmd,
) {
    if !script.progress.armed {
        script.progress.armed = true;
        if let Some(name) = &script.weapon {
            cmd.select = arm(player, sim, name);
        }
    }
    if let Some((at, look)) = script.teleport {
        // Held there every tick, so nothing (gravity, water) moves the view.
        sim.teleport_player(player, Vec3::from(at));
        (cmd.pitch, cmd.yaw) = look;
    }
    if script.grab_at.is_some_and(|at| now >= at) {
        grab(script, player, sim, cmd);
    }
    apply_input(script, now, cmd);
    if let Some((start, duration)) = script.shoot
        && (start..start + duration).contains(&now)
    {
        shoot(script, player, sim, cmd);
    }
    if script.visit_at.is_some_and(|at| now >= at)
        && let Some((pitch, yaw)) = script
            .progress
            .sight
            .find_once(|| go_to_nearest_pickup(player, sim))
    {
        cmd.pitch = pitch;
        cmd.yaw = yaw;
    }
    if let Some((at, class)) = &script.use_mover
        && now >= *at
        && let Some((pitch, yaw)) = script
            .progress
            .lever
            .find_once(|| go_to_nearest_mover(player, sim, class))
    {
        cmd.pitch = pitch;
        cmd.yaw = yaw;
        // Face it first: press use once the turn has reached the server.
        if !script.progress.used && now >= at + USE_DELAY {
            script.progress.used = true;
            cmd.buttons = cmd.buttons | Buttons::USE;
        }
    }
}

/// Moves the player in front of the nearest mover of class `class` and
/// returns the view angles looking at its centre.
fn go_to_nearest_mover(player: PlayerId, sim: &mut Simulation, class: &str) -> Option<(f32, f32)> {
    let origin = sim.player(player)?.state.origin;
    let entities = sim
        .logic()
        .movers()
        .iter()
        .map(|mover| mover.model)
        .collect::<Vec<_>>();
    let center = entities
        .into_iter()
        .filter(|&model| sim.logic().mover_class_name(model) == Some(class))
        .filter_map(|model| {
            let (mins, maxs) = sim.world().mover_bounds(model)?;
            Some((mins + maxs) * 0.5)
        })
        .min_by(|a, b| {
            a.distance_squared(origin)
                .total_cmp(&b.distance_squared(origin))
        })?;
    // Stand on the floor below the mover's centre.
    let floor = sim
        .world()
        .trace_ray(center, center - Vec3::Z * 256.0, MASK_PLAYER_SOLID)
        .end;
    let stood = [USE_STAND_OFF, USE_STAND_OFF * 1.5, USE_STAND_OFF * 2.0]
        .into_iter()
        .any(|distance| stand_near(player, sim, origin, floor, distance, center));
    if !stood {
        warn!("script: no room to stand by the {class} at {center}");
        return None;
    }
    info!("script: using the {class} at {center}");
    Some(angles_to(sim.eye_position(player)?, center))
}

/// Moves the player a short way off the pickup nearest them and returns
/// the view angles looking at it.
fn go_to_nearest_pickup(player: PlayerId, sim: &mut Simulation) -> Option<(f32, f32)> {
    let origin = sim.player(player)?.state.origin;
    let at = sim
        .pickups()
        .iter()
        .map(|pickup| pickup.origin)
        .min_by(|a, b| {
            a.distance_squared(origin)
                .total_cmp(&b.distance_squared(origin))
        })?;
    let look = at + Vec3::Z * 4.0;
    if !stand_near(player, sim, origin, at, VISIT_STAND_OFF, look) {
        warn!("script: no room to stand by the pickup at {at}");
        return None;
    }
    info!("script: looking at the pickup at {at}");
    Some(angles_to(sim.eye_position(player)?, look))
}

/// Gives `player` the weapon with id `name` and plenty of its ammo;
/// returns it, to be taken in hand.
fn arm(player: PlayerId, sim: &mut Simulation, name: &str) -> Option<WeaponId> {
    let arsenal = sim.arsenal().clone();
    let Some(weapon) = arsenal.find_weapon(name) else {
        warn!("script: no weapon {name:?}");
        return None;
    };
    let ammo = arsenal
        .weapon(weapon)
        .and_then(rl_data::WeaponConfig::ammo_type)
        .and_then(|ammo| arsenal.find_ammo(ammo));
    let inventory = &mut sim.player_mut(player)?.inventory;
    inventory.give_weapon(&arsenal, weapon);
    if let Some(ammo) = ammo {
        inventory.give_ammo(&arsenal, ammo, SCRIPT_AMMO);
    }
    Some(weapon)
}

/// The grab step: Manipulator in hand, at the nearest prop, secondary fire
/// held until it is picked up.
fn grab(script: &mut InputScript, player: PlayerId, sim: &mut Simulation, cmd: &mut UserCmd) {
    let manipulator = sim.arsenal().find_weapon("manipulator");
    let in_hand = sim
        .player(player)
        .and_then(|player| player.inventory.active());
    if in_hand != manipulator {
        cmd.select = manipulator;
    }
    let target = script
        .progress
        .target
        .find_once(|| go_to_nearest_prop(player, sim));
    if let Some((body, pitch, yaw)) = target {
        // Keep aiming at the target: the player settles onto the floor
        // after being moved, and a grabbed prop moves in front.
        let (pitch, yaw) = match (sim.eye_position(player), sim.physics().center_of_mass(body)) {
            (Some(eye), Some(center)) => angles_to(eye, center),
            _ => (pitch, yaw),
        };
        cmd.pitch = pitch;
        cmd.yaw = yaw;
        let held = sim
            .player(player)
            .and_then(|player| player.manipulator.held());
        if held != Some(body) && !script.progress.fired {
            cmd.buttons = cmd.buttons | Buttons::ATTACK2;
        }
    }
}

/// The shoot step: stand off from the nearest living player, aim at them
/// and hold primary fire.
fn shoot(script: &mut InputScript, player: PlayerId, sim: &mut Simulation, cmd: &mut UserCmd) {
    let victim = script
        .progress
        .victim
        .find_once(|| go_to_nearest_player(player, sim));
    if let Some((victim, pitch, yaw)) = victim {
        // Keep aiming at a target that moves (or is thrown).
        let (pitch, yaw) = aim_at_player(player, victim, sim).unwrap_or((pitch, yaw));
        cmd.pitch = pitch;
        cmd.yaw = yaw;
        cmd.buttons = cmd.buttons | Buttons::ATTACK;
    }
}

/// Moves `player` `distance` units from `at` (on its level), facing back
/// towards it: the first of several directions — starting from the side
/// the player came from — with room to stand, a floor underfoot and a clear
/// view of `look`. Returns whether such a place was found.
fn stand_near(
    player: PlayerId,
    sim: &mut Simulation,
    from: Vec3,
    at: Vec3,
    distance: f32,
    look: Vec3,
) -> bool {
    let away = (from - at).truncate().normalize_or(Vec2::X);
    for step in 0..STAND_DIRECTIONS {
        let turn = std::f32::consts::TAU * step as f32 / STAND_DIRECTIONS as f32;
        let direction = Vec2::from_angle(turn).rotate(away);
        let stand = at.truncate() + direction * distance;
        if !sim.teleport_player(player, Vec3::new(stand.x, stand.y, at.z + 1.0)) {
            continue;
        }
        let Some(feet) = sim.player(player).map(|player| player.state.origin) else {
            return false;
        };
        let world = sim.world();
        let floor = world
            .trace_ray(feet, feet - Vec3::Z * FLOOR_REACH, MASK_PLAYER_SOLID)
            .hit();
        let seen = sim
            .eye_position(player)
            .is_some_and(|eye| !world.trace_ray(eye, look, MASK_SHOT).hit());
        if floor && seen {
            return true;
        }
    }
    sim.teleport_player(player, from);
    false
}

/// View angles from `player`'s eyes to `target`'s chest, if both live.
fn aim_at_player(player: PlayerId, target: PlayerId, sim: &Simulation) -> Option<(f32, f32)> {
    let eye = sim.eye_position(player)?;
    let target = sim.player(target).filter(|target| target.alive())?;
    Some(angles_to(eye, target.state.origin + Vec3::Z * 40.0))
}

/// Pitch and yaw (degrees, Source convention) looking from `eye` at
/// `point`.
fn angles_to(eye: Vec3, point: Vec3) -> (f32, f32) {
    let to = point - eye;
    let yaw = to.y.atan2(to.x).to_degrees();
    let pitch = -to.z.atan2(to.truncate().length()).to_degrees();
    (pitch, yaw)
}

/// Moves the player a short way off the living player nearest them, facing
/// them, and returns that player with the view angles aimed at them.
fn go_to_nearest_player(player: PlayerId, sim: &mut Simulation) -> Option<(PlayerId, f32, f32)> {
    let origin = sim.player(player)?.state.origin;
    let (target, at) = sim
        .players()
        .filter(|&(id, other)| id != player && other.alive())
        .map(|(id, other)| (id, other.state.origin))
        .min_by(|a, b| {
            a.1.distance_squared(origin)
                .total_cmp(&b.1.distance_squared(origin))
        })?;
    if !stand_near(
        player,
        sim,
        origin,
        at,
        SHOOT_STAND_OFF,
        at + Vec3::Z * 40.0,
    ) {
        warn!("script: no room to stand by {target:?}");
        return None;
    }
    let (pitch, yaw) = aim_at_player(player, target, sim)?;
    info!("script: shooting {target:?}");
    Some((target, pitch, yaw))
}

/// Moves the player near the grabbable prop closest to them, facing it, and
/// returns the prop with the view angles aimed at its centre.
fn go_to_nearest_prop(player: PlayerId, sim: &mut Simulation) -> Option<(BodyId, f32, f32)> {
    let origin = sim.player(player)?.state.origin;
    let physics = sim.physics();
    let (body, center) = physics
        .props()
        .filter(|&body| physics.is_grabbable(body))
        .filter_map(|body| Some((body, physics.center_of_mass(body)?)))
        .min_by(|a, b| {
            a.1.distance_squared(origin)
                .total_cmp(&b.1.distance_squared(origin))
        })?;
    let floor = center - Vec3::Z * 16.0;
    if !stand_near(player, sim, origin, floor, GRAB_STAND_OFF, center) {
        warn!("script: no room to stand by prop {body:?}");
        return None;
    }
    let feet = sim.player(player)?.state.origin;
    let eye = sim.eye_position(player)?;
    let (pitch, yaw) = angles_to(eye, center);
    info!("script: grabbing prop {body:?} from {feet}");
    Some((body, pitch, yaw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_point_at_their_target() {
        let (pitch, yaw) = angles_to(Vec3::ZERO, Vec3::new(0.0, 10.0, -10.0));
        assert!((yaw - 90.0).abs() < 1e-4);
        assert!((pitch - 45.0).abs() < 1e-4, "looking down is positive");
    }
}
