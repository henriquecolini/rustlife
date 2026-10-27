//! The view punch: a kick of the view that springs back, as the engine's
//! `CBasePlayer::ViewPunch` gives it.
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
use rl_sim::rng::Rng;

use crate::AppState;
use crate::physcannon_impact::PhyscannonImpact;

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

/// Kicks the view of the player who throws something with the Gravity Gun.
pub(crate) struct ViewPunchPlugin;

impl Plugin for ViewPunchPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ViewPunch>().add_systems(
            Update,
            (kick_on_launch, settle)
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
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
