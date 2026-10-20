//! Scripted input for non-interactive runs: screenshots of the player
//! moving and of the Manipulator at work, and tests of the running client.
//!
//! An [`InputScript`] adjusts the local player's command each tick by app
//! time:
//!
//! - **walk**: hold forward for a while;
//! - **grab**: go to the grabbable prop nearest the player — the player is
//!   moved to a free spot a short way from it, facing it — and hold
//!   secondary fire at it until it is picked up;
//! - **fire**: press primary fire once (throwing whatever is held).
//!
//! It is a debugging aid only; normal play never inserts one.

use bevy::prelude::*;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::physics::BodyId;
use rl_sim::simulation::{PlayerId, Simulation};

/// How far from the target prop the grab step places the player, units.
const GRAB_STAND_OFF: f32 = 110.0;

/// Scripted actions by app time, in seconds.
#[derive(Resource, Debug, Clone, Default, PartialEq)]
pub struct InputScript {
    /// Hold forward from `.0` for `.1` seconds.
    pub walk: Option<(f32, f32)>,
    /// From this time, go to the nearest grabbable prop and pick it up.
    pub grab_at: Option<f32>,
    /// At this time, press primary fire once.
    pub fire_at: Option<f32>,
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
            progress: Progress::default(),
        }
    }
}

/// How far the script has got.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Progress {
    /// The prop being grabbed and the view angles aimed at it.
    target: Option<(BodyId, f32, f32)>,
    /// Whether the target was found (or found missing) already.
    went: bool,
    /// Whether primary fire was pressed.
    fired: bool,
}

/// Adjusts `cmd` for app time `now`.
pub(crate) fn apply(
    script: &mut InputScript,
    now: f32,
    player: PlayerId,
    sim: &mut Simulation,
    cmd: &mut UserCmd,
) {
    if let Some((start, duration)) = script.walk
        && (start..start + duration).contains(&now)
    {
        cmd.forward = 1.0;
    }
    if script.grab_at.is_some_and(|at| now >= at) {
        if !script.progress.went {
            script.progress.went = true;
            script.progress.target = go_to_nearest_prop(player, sim);
        }
        if let Some((body, pitch, yaw)) = script.progress.target {
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
    if !script.progress.fired && script.fire_at.is_some_and(|at| now >= at) {
        script.progress.fired = true;
        cmd.buttons = cmd.buttons | Buttons::ATTACK;
    }
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
    let away = (origin - center).truncate().normalize_or(Vec2::X);
    let stand = center.truncate() + away * GRAB_STAND_OFF;
    let feet = Vec3::new(stand.x, stand.y, center.z - 16.0);
    if !sim.teleport_player(player, feet) {
        warn!("script: no room to stand by prop {body:?}");
        return None;
    }
    let eye = sim.eye_position(player)?;
    let to = center - eye;
    let yaw = to.y.atan2(to.x).to_degrees();
    let pitch = -to.z.atan2(to.truncate().length()).to_degrees();
    info!("script: grabbing prop {body:?} from {feet}");
    Some((body, pitch, yaw))
}
