//! The view punch: a kick of the view that springs back, as the engine's
//! `CBasePlayer::ViewPunch` gives it, and the rest of what a shot with a
//! [`ViewKick`] does to its shooter's view: the aim jolted (the engine's
//! `SnapEyeAngles`, which turns the aim itself and leaves it turned) and a
//! flash of the screen (see [`crate::screen_fade`]). The Pulse Rifle's energy
//! ball does all three.
//!
//! A punch does not move the view by an angle at once: it gives the angle a
//! velocity (twenty times the punch, in degrees per second), and a damped
//! spring then pulls the angle back to nothing, so the view jumps and settles
//! over about half a second. The Gravity Gun's `PrimaryFireEffect` punches
//! by -6 degrees of pitch (up) and a whole number of degrees of yaw between
//! -2 and 2 (see [`crate::physcannon_impact`]).
//!
//! The spring is the engine's `DecayPunchAngle`, stepped sixty times a second
//! however fast the frames come: its damping is 9 per second and its spring
//! constant 65.

use bevy::prelude::*;
use rl_data::ViewKick;
use rl_sim::rng::Rng;

use crate::AppState;
use crate::net::Game;
use crate::physcannon_impact::PhyscannonImpact;
use crate::player::{LocalPlayer, PITCH_LIMIT, Ticked, ViewAngles};
use crate::screen_fade::ScreenFade;

/// How hard a punch is thrown: degrees per second of velocity per degree
/// punched.
const THROW: f32 = 20.0;
/// How fast the velocity dies away, per second.
const DAMPING: f32 = 9.0;
/// How hard the spring pulls the angle back.
const SPRING: f32 = 65.0;
/// The engine's step, seconds.
const STEP: f32 = 1.0 / 60.0;
/// Below this the punch is taken as over, degrees (and degrees per second).
const REST: f32 = 0.001;
/// The Gravity Gun's punch: pitch (negative is up) and the most yaw either
/// way, degrees.
pub(crate) const GRAVITY_GUN: (f32, i32) = (-6.0, 2);

/// How far the view is kicked, degrees: pitch (positive is down), yaw and
/// roll, and how fast each is moving.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct ViewPunch {
    angle: Vec3,
    velocity: Vec3,
    /// Time not yet spent in whole steps, seconds.
    pending: f32,
}

impl ViewPunch {
    /// Kicks the view by `degrees` (pitch, yaw, roll).
    pub(crate) fn punch(&mut self, degrees: Vec3) {
        self.velocity += degrees * THROW;
    }

    /// Springs the punch back for `dt` seconds.
    pub(crate) fn step(&mut self, dt: f32) {
        self.pending += dt;
        while self.pending >= STEP {
            self.pending -= STEP;
            if self.angle.length_squared() <= REST * REST
                && self.velocity.length_squared() <= REST * REST
            {
                self.angle = Vec3::ZERO;
                self.velocity = Vec3::ZERO;
                continue;
            }
            self.angle += self.velocity * STEP;
            self.velocity *= (1.0 - DAMPING * STEP).max(0.0);
            self.velocity -= self.angle * (SPRING * STEP).clamp(0.0, 2.0);
        }
    }

    /// The angles the view is kicked by now: pitch (positive is down) and
    /// yaw, degrees.
    pub(crate) fn pitch_yaw(&self) -> (f32, f32) {
        (self.angle.x, self.angle.y)
    }
}

/// Kicks the view of the player who throws something with the Gravity Gun,
/// or fires a shot with a kick.
pub(crate) struct ViewPunchPlugin;

impl Plugin for ViewPunchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewPunch>().add_systems(
            Update,
            (kick_on_launch, kick_on_fire, settle)
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// What one shot's kick comes to: the punch (pitch, yaw, roll, degrees),
/// the turn of the aim (pitch, yaw, degrees) and the flash, if any.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Kicked {
    punch: Vec3,
    jolt: (f32, f32),
    flash: Option<ScreenFade>,
}

/// Picks the numbers of one shot's `kick`.
fn roll_kick(kick: &ViewKick, rng: &mut Rng) -> Kicked {
    let mut pick = |(low, high): (i32, i32)| {
        let span = usize::try_from(high - low).unwrap_or(0);
        // A whole number of degrees, a few dozen at most: exact as a float.
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
        let picked = (low + rng.below(span + 1) as i32) as f32;
        picked
    };
    let punch = Vec3::new(pick(kick.pitch), pick(kick.yaw), 0.0);
    let jolt = (pick((-kick.jolt, kick.jolt)), pick((-kick.jolt, kick.jolt)));
    let flash = kick.flash.map(|flash| ScreenFade {
        color: [1.0; 3],
        alpha: flash.alpha,
        seconds: flash.seconds,
    });
    Kicked { punch, jolt, flash }
}

/// Kicks the local player's view for each of their shots whose fire mode
/// has a [`ViewKick`]. The jolt turns the aim the player sends the server,
/// as the engine's does.
fn kick_on_fire(
    mut ticks: MessageReader<Ticked>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    mut punch: ResMut<ViewPunch>,
    mut angles: ResMut<ViewAngles>,
    mut fades: MessageWriter<ScreenFade>,
    mut rng: Local<Option<Rng>>,
) {
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        ticks.clear();
        return;
    };
    let rng = rng.get_or_insert_with(|| Rng::seeded(&[0x6b69_636b]));
    for Ticked(events) in ticks.read() {
        for &(who, weapon, mode) in &events.fired {
            let kick = sim
                .arsenal()
                .fire(weapon, mode)
                .and_then(|fire| fire.kick.as_ref());
            let Some(kick) = kick.filter(|_| who == local.id) else {
                continue;
            };
            let kicked = roll_kick(kick, rng);
            punch.punch(kicked.punch);
            angles.pitch = (angles.pitch + kicked.jolt.0).clamp(-PITCH_LIMIT, PITCH_LIMIT);
            angles.yaw = (angles.yaw + kicked.jolt.1).rem_euclid(360.0);
            if let Some(flash) = kicked.flash {
                fades.write(flash);
            }
        }
    }
}

fn kick_on_launch(
    mut impacts: MessageReader<PhyscannonImpact>,
    mut punch: ResMut<ViewPunch>,
    mut rng: Local<Option<Rng>>,
) {
    let rng = rng.get_or_insert_with(|| Rng::seeded(&[0x7068_7973_6361]));
    for impact in impacts.read() {
        if !impact.local {
            continue;
        }
        let (pitch, most) = GRAVITY_GUN;
        let yaw = rng.below((2 * most + 1) as usize) as i32 - most;
        punch.punch(Vec3::new(pitch, yaw as f32, 0.0));
    }
}

fn settle(time: Res<Time>, mut punch: ResMut<ViewPunch>) {
    punch.step(time.delta_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(punch: &mut ViewPunch, seconds: f32) -> Vec<(f32, f32)> {
        let frames = (seconds * 60.0).round() as usize;
        (0..frames)
            .map(|_| {
                punch.step(STEP);
                punch.pitch_yaw()
            })
            .collect()
    }

    #[test]
    fn a_punch_up_kicks_the_view_up_then_settles_back() {
        let mut punch = ViewPunch::default();
        punch.punch(Vec3::new(-6.0, 0.0, 0.0));
        let path = run(&mut punch, 1.5);
        let peak = path.iter().map(|p| p.0).fold(0.0, f32::min);
        // The spring carries a six degree punch a little past it.
        assert!((-8.5..=-7.0).contains(&peak), "kicked up by {peak}");
        // Underdamped (a damping ratio near 0.56): it swings back a little
        // past where it was before it settles.
        let rebound = path.iter().map(|p| p.0).fold(0.0, f32::max);
        assert!((0.3..1.5).contains(&rebound), "rebounds by {rebound}");
        let end = path.last().unwrap().0;
        assert!(end.abs() < 0.05, "settled to {end}");
    }

    #[test]
    fn it_does_not_jump_it_starts_from_nothing_and_builds() {
        let mut punch = ViewPunch::default();
        punch.punch(Vec3::new(-6.0, 0.0, 0.0));
        assert_eq!(punch.pitch_yaw(), (0.0, 0.0));
        punch.step(STEP);
        let first = punch.pitch_yaw().0;
        assert!(first < 0.0 && first > -3.0, "{first}");
    }

    #[test]
    fn yaw_goes_the_way_it_is_punched() {
        let mut left = ViewPunch::default();
        left.punch(Vec3::new(0.0, 2.0, 0.0));
        let mut right = ViewPunch::default();
        right.punch(Vec3::new(0.0, -2.0, 0.0));
        let (l, r) = (run(&mut left, 0.2), run(&mut right, 0.2));
        assert!(l.iter().all(|p| p.1 > 0.0) && r.iter().all(|p| p.1 < 0.0));
        for (a, b) in l.iter().zip(&r) {
            assert!((a.1 + b.1).abs() < 1e-5, "mirror images");
        }
    }

    #[test]
    fn it_is_the_same_however_the_frames_come() {
        let mut slow = ViewPunch::default();
        slow.punch(Vec3::new(-6.0, 1.0, 0.0));
        let mut fast = slow;
        for _ in 0..30 {
            slow.step(1.0 / 30.0);
        }
        for _ in 0..120 {
            fast.step(1.0 / 120.0);
        }
        let (a, b) = (slow.pitch_yaw(), fast.pitch_yaw());
        assert!(
            (a.0 - b.0).abs() < 0.05 && (a.1 - b.1).abs() < 0.05,
            "{a:?} {b:?}"
        );
    }

    #[test]
    fn the_energy_balls_kick_is_the_games() {
        let data = rl_data::GameData::builtin().expect("data");
        let rifle = data.weapons.get("rifle").expect("rifle");
        let kick = rifle
            .secondary
            .as_ref()
            .and_then(|fire| fire.kick)
            .expect("the energy ball kicks");
        let mut rng = Rng::seeded(&[1]);
        let rolls: Vec<Kicked> = (0..400).map(|_| roll_kick(&kick, &mut rng)).collect();
        let pitches: Vec<f32> = rolls.iter().map(|kicked| kicked.punch.x).collect();
        let yaws: Vec<f32> = rolls.iter().map(|kicked| kicked.punch.y).collect();
        let jolts: Vec<f32> = rolls
            .iter()
            .flat_map(|kicked| [kicked.jolt.0, kicked.jolt.1])
            .collect();
        let every = |values: &[f32], wanted: std::ops::RangeInclusive<i32>| {
            let mut seen: Vec<i32> = values.iter().map(|&value| value as i32).collect();
            seen.sort_unstable();
            seen.dedup();
            seen == wanted.collect::<Vec<_>>()
        };
        // Up by 8 to 12 degrees, 1 or 2 to the side, the aim off by up to 4.
        assert!(every(&pitches, -12..=-8), "{pitches:?}");
        assert!(every(&yaws, 1..=2), "{yaws:?}");
        assert!(every(&jolts, -4..=4), "{jolts:?}");
        let flash = rolls[0].flash.expect("a flash");
        assert_eq!(flash.color, [1.0; 3]);
        assert!((flash.alpha - 64.0 / 255.0).abs() < 1e-3);
        assert!((flash.seconds - 0.1).abs() < 1e-6);
    }

    #[test]
    fn nothing_punched_stays_still() {
        let mut punch = ViewPunch::default();
        let path = run(&mut punch, 0.5);
        assert!(path.iter().all(|p| *p == (0.0, 0.0)));
    }

    #[test]
    fn the_gravity_guns_punch_is_the_games() {
        assert_eq!(GRAVITY_GUN, (-6.0, 2));
    }
}
