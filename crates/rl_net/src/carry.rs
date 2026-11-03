//! The prop the local player holds, run ahead of the server with them.
//!
//! Everything but the local player is drawn from the interpolated past. A prop
//! they carry cannot be: it would trail them by that delay plus half a round
//! trip. So the client steers it as the server does, from where the newest
//! snapshot has it, with the commands the server has not run yet, and again with
//! each command it issues (the formulas are the server's: [`hold_target`] for
//! where it goes, [`hold_velocity`] for the Manipulator's pace and
//! [`carry_velocity`] for the hands'). A snapshot that disagrees leaves an
//! error that fades away, like the view's. The server's prop stops at walls: the
//! prediction sweeps a box against the map and the other props, so it does not
//! go through what the server's will not. It cannot know a prop will be shoved
//! aside, so it waits for the snapshot that says so.
//!
//! On letting go the prop goes back to the interpolated past, a long way behind
//! the prediction on a player on the move: 32 units at 320 units/s, just for the
//! delay. Rather than snap back it carries on as it was going while easing there
//! over [`RELEASE_SECS`]. A thrown prop does not ease: it is really far ahead
//! of where the prediction was going, and goes straight to where it is drawn.

use glam::{Quat, Vec3};
use rl_sim::collision::contents::MASK_PROP_SOLID;
use rl_sim::collision::{CollisionQuery, Hull};
use rl_sim::movement::UserCmd;
use rl_sim::physics::{BodyId, BodyPose};
use rl_sim::scene::Scene;
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::tools::hands::carry_velocity;
use rl_sim::tools::hold::{Carrier, hold_target};
use rl_sim::tools::manipulator::{Aim, hold_rotation, hold_velocity};

/// Seconds a prop let go of takes to ease back into the interpolated past.
const RELEASE_SECS: f32 = 0.2;
/// An error bigger than this is not one to smooth away, units: more than a
/// fresh grab's prop can travel to the hold point.
const MAX_SMOOTHED_ERROR: f32 = 256.0;
/// How much smaller than the prop's bounds (which have a skin of most of a unit)
/// its prediction's box is on each side,
/// units. Props rest slightly in what they touch, and a box that starts stuck
/// does not move at all.
const BOX_MARGIN: f32 = 1.0;
/// Thinnest half a prediction's box gets, units.
const MIN_BOX: f32 = 0.5;
/// Surfaces a held prop's prediction may slide along in one tick.
const MAX_SLIDES: usize = 3;

/// A prop's course over the last tick, and any error left to fade away.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Course {
    /// Where its centre of mass was...
    previous: Vec3,
    /// ...and is.
    current: Vec3,
    /// The view's yaw then...
    previous_yaw: f32,
    /// ...and now, degrees.
    current_yaw: f32,
    /// Its orientation relative to the view's yaw when grabbed.
    relative_rotation: Quat,
    /// Where its centre of mass is, in its own space.
    local_centre: Vec3,
    /// How far from the prediction to draw it, fading away.
    error: Vec3,
}

impl Course {
    /// Where the prop is `ticks` ticks after the previous one; past one, as it
    /// was going.
    fn pose_at(&self, ticks: f32) -> BodyPose {
        let centre = self.previous.lerp(self.current, ticks) + self.error;
        let turn = wrapped_degrees(self.current_yaw - self.previous_yaw);
        let rotation = hold_rotation(self.previous_yaw + turn * ticks, self.relative_rotation);
        BodyPose {
            position: centre - rotation * self.local_centre,
            rotation,
        }
    }

    /// Moves on a tick, to `centre` with the view at `yaw`.
    fn advance(&mut self, centre: Vec3, yaw: f32) {
        self.previous = self.current;
        self.previous_yaw = self.current_yaw;
        self.current = centre;
        self.current_yaw = yaw;
    }

    /// Takes over from `old`, this prop's course before a snapshot: what is
    /// different between them is an error to fade, and the last tick stays the
    /// step it was, so that the drawn course does not jump.
    fn continue_from(&mut self, old: &Self) {
        self.error = smoothed_error(old.error + old.current - self.current);
        self.previous = self.current - (old.current - old.previous);
        self.previous_yaw = self.current_yaw - wrapped_degrees(old.current_yaw - old.previous_yaw);
    }
}

/// `degrees` as the shorter way round.
fn wrapped_degrees(degrees: f32) -> f32 {
    (degrees + 180.0).rem_euclid(360.0) - 180.0
}

/// `error`, if it is not too big to smooth away.
fn smoothed_error(error: Vec3) -> Vec3 {
    if error.length() < MAX_SMOOTHED_ERROR {
        error
    } else {
        Vec3::ZERO
    }
}

/// `from` moved towards `to` by `t` of the way (`0..=1`), easing in and out.
fn blend(from: BodyPose, to: BodyPose, t: f32) -> BodyPose {
    let t = t.clamp(0.0, 1.0);
    let eased = t * t * (3.0 - 2.0 * t);
    BodyPose {
        position: from.position.lerp(to.position, eased),
        rotation: from.rotation.slerp(to.rotation, eased),
    }
}

/// The prediction of `motion` from `from`, for a box `hull` around the prop's
/// centre: as far as what is in its way lets it, sliding along it.
fn sweep(scene: &Scene<'_>, from: Vec3, motion: Vec3, hull: &Hull) -> Vec3 {
    let (mut at, mut left) = (from, motion);
    for _ in 0..MAX_SLIDES {
        let trace = scene.trace_hull(at, at + left, hull, MASK_PROP_SOLID);
        if trace.all_solid {
            return at;
        }
        at = trace.end;
        if !trace.hit() {
            break;
        }
        left *= 1.0 - trace.fraction;
        left -= trace.normal * left.dot(trace.normal);
    }
    at
}

/// The box a prop with these bounds is swept as, around its centre `centre`.
fn prediction_box((mins, maxs): (Vec3, Vec3), centre: Vec3) -> Hull {
    Hull {
        mins: (mins - centre + BOX_MARGIN).min(Vec3::splat(-MIN_BOX)),
        maxs: (maxs - centre - BOX_MARGIN).max(Vec3::splat(MIN_BOX)),
    }
}

/// What holds a prop.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grip {
    /// The Manipulator.
    Manipulator,
    /// The hands.
    Hands,
}

/// The prop the local player holds, predicted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HeldProp {
    body: BodyId,
    course: Course,
    /// What holds it.
    grip: Grip,
    /// What its prediction is swept as.
    hull: Hull,
}

impl HeldProp {
    /// The prop `player` holds in `sim`, where `sim` has it.
    pub(crate) fn of(sim: &Simulation, player: PlayerId) -> Option<Self> {
        let holder = sim.player(player)?;
        let (body, relative_rotation, grip) = if let Some(body) = holder.manipulator.held() {
            let rotation = holder.manipulator.held_rotation()?;
            (body, rotation, Grip::Manipulator)
        } else {
            let body = holder.hands.held()?;
            (body, holder.hands.held_rotation()?, Grip::Hands)
        };
        let physics = sim.physics();
        let pose = physics.pose(body)?;
        let centre = physics.center_of_mass(body)?;
        let yaw = holder.look.1;
        Some(Self {
            body,
            course: Course {
                previous: centre,
                current: centre,
                previous_yaw: yaw,
                current_yaw: yaw,
                relative_rotation,
                local_centre: pose.rotation.inverse() * (centre - pose.position),
                error: Vec3::ZERO,
            },
            grip,
            hull: prediction_box(physics.bounds(body)?, centre),
        })
    }

    /// Steers it for `player` having just run `cmd`, as the server will.
    pub(crate) fn step(&mut self, sim: &Simulation, player: PlayerId, cmd: &UserCmd) {
        let (Some(eye), Some(holder)) = (sim.eye_position(player), sim.player(player)) else {
            return;
        };
        let carrier = Carrier {
            origin: holder.state.origin,
            hull: holder.state.hull(&sim.data().movement),
            aim: Aim {
                eye,
                pitch: cmd.pitch,
                yaw: cmd.yaw,
            },
        };
        let data = sim.data();
        let point = match self.grip {
            Grip::Manipulator => &data.manipulator.hold.point,
            Grip::Hands => &data.hands.hold.point,
        };
        let error = hold_target(&carrier, self.body, sim.world(), sim.physics(), point)
            .map_or(Vec3::ZERO, |target| target - self.course.current);
        let velocity = match self.grip {
            Grip::Manipulator => hold_velocity(error, &data.manipulator.hold),
            Grip::Hands => carry_velocity(error, &data.hands.hold),
        };
        let scene = Scene {
            world: sim.world(),
            physics: sim.physics(),
            ignore: Some(self.body),
            others: &[],
        };
        let motion = velocity * sim.dt();
        let centre = sweep(&scene, self.course.current, motion, &self.hull);
        self.course.advance(centre, cmd.yaw);
    }
}

/// A prop let go of, easing back into the interpolated past.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Released {
    body: BodyId,
    course: Course,
    /// Ticks since its course's previous tick, less the one that takes to get
    /// to where it was let go.
    ticks: u32,
    /// Seconds per tick.
    dt: f32,
}

impl Released {
    /// Where to draw it, `alpha` into the tick, with the interpolated past at
    /// `interpolated`.
    fn pose(&self, interpolated: BodyPose, alpha: f32) -> BodyPose {
        let ticks = self.ticks as f32 + alpha;
        let going = self.course.pose_at(ticks);
        blend(going, interpolated, ticks * self.dt / RELEASE_SECS)
    }

    /// Whether the interpolated past has it by now.
    fn is_done(&self) -> bool {
        self.ticks as f32 * self.dt >= RELEASE_SECS
    }
}

/// The local player's prop on the way, and those they just let go of.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Carry {
    held: Option<HeldProp>,
    released: Vec<Released>,
}

impl Carry {
    /// Where to draw `body`, `alpha` into the tick, if it is held or just let
    /// go of, with `interpolated` the pose it has otherwise.
    pub(crate) fn pose(
        &self,
        body: BodyId,
        interpolated: Option<BodyPose>,
        alpha: f32,
    ) -> Option<BodyPose> {
        if let Some(held) = self.held.filter(|held| held.body == body) {
            return Some(held.course.pose_at(alpha));
        }
        let released = self
            .released
            .iter()
            .find(|released| released.body == body)?;
        Some(released.pose(interpolated?, alpha))
    }

    /// The local player ran `cmd`: the prop goes along.
    pub(crate) fn step(&mut self, sim: &Simulation, player: PlayerId, cmd: &UserCmd) {
        if let Some(held) = &mut self.held {
            held.step(sim, player, cmd);
        }
        for released in &mut self.released {
            released.ticks += 1;
        }
        self.released.retain(|released| !released.is_done());
    }

    /// Multiplies what errors are left by `fade`.
    pub(crate) fn fade_error(&mut self, fade: f32) {
        if let Some(held) = &mut self.held {
            held.course.error *= fade;
        }
        for released in &mut self.released {
            released.course.error *= fade;
        }
    }

    /// A snapshot came, and `held` is the prop the player holds, predicted anew
    /// from it (`None` if they hold nothing). The snapshot's events say which
    /// props were `thrown`; `seen` has where the interpolated past has a prop.
    /// `dt` is the seconds per tick.
    pub(crate) fn settle(
        &mut self,
        mut held: Option<HeldProp>,
        dt: f32,
        thrown: impl Fn(BodyId) -> bool,
        seen: impl Fn(BodyId) -> Option<BodyPose>,
    ) {
        let old = self.held.take();
        match (old, &mut held) {
            (Some(old), Some(new)) if old.body == new.body => {
                new.course.continue_from(&old.course);
            }
            (old, new) => {
                if let Some(old) = old.filter(|old| !thrown(old.body)) {
                    self.released.push(Released {
                        body: old.body,
                        course: old.course,
                        ticks: 0,
                        dt,
                    });
                }
                if let Some(new) = new {
                    // A grab: from where it was being drawn.
                    self.released.retain(|released| released.body != new.body);
                    if let Some(pose) = seen(new.body) {
                        let from = pose.position + pose.rotation * new.course.local_centre;
                        new.course.error = smoothed_error(from - new.course.current);
                    }
                }
            }
        }
        self.held = held;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 0.015;

    fn course() -> Course {
        Course {
            previous: Vec3::new(100.0, 0.0, 0.0),
            current: Vec3::new(110.0, 0.0, 0.0),
            previous_yaw: 350.0,
            current_yaw: 10.0,
            relative_rotation: Quat::IDENTITY,
            local_centre: Vec3::ZERO,
            error: Vec3::ZERO,
        }
    }

    fn held(body: usize) -> HeldProp {
        HeldProp {
            body: BodyId::from_index(body),
            course: course(),
            grip: Grip::Manipulator,
            hull: Hull::POINT,
        }
    }

    fn holding() -> Carry {
        Carry {
            held: Some(held(3)),
            released: Vec::new(),
        }
    }

    fn at(x: f32) -> BodyPose {
        BodyPose {
            position: Vec3::new(x, 0.0, 0.0),
            rotation: Quat::IDENTITY,
        }
    }

    fn assert_same_turn(a: Quat, b: Quat) {
        assert!(a.angle_between(b) < 1e-3, "{a} and {b}");
    }

    #[test]
    fn a_course_is_drawn_between_its_ticks_turning_the_shorter_way() {
        let pose = course().pose_at(0.5);
        assert!((pose.position.x - 105.0).abs() < 1e-3, "{pose:?}");
        assert_same_turn(pose.rotation, Quat::IDENTITY);
    }

    #[test]
    fn a_course_runs_on_past_its_last_tick_as_it_was_going() {
        let pose = course().pose_at(3.0);
        assert!((pose.position.x - 130.0).abs() < 1e-3, "{pose:?}");
        assert_same_turn(pose.rotation, Quat::from_rotation_z(50f32.to_radians()));
    }

    #[test]
    fn a_courses_pose_is_the_models_origin_not_its_centre_of_mass() {
        let mut course = course();
        course.previous_yaw = 90.0;
        course.current_yaw = 90.0;
        course.local_centre = Vec3::new(0.0, 0.0, 12.0);
        course.relative_rotation = Quat::from_rotation_y(90f32.to_radians());
        let pose = course.pose_at(1.0);
        let centre = pose.position + pose.rotation * course.local_centre;
        assert!(centre.distance(course.current) < 1e-3, "{centre}");
        assert!(pose.position.distance(course.current) > 11.0, "{pose:?}");
    }

    #[test]
    fn a_course_shows_its_error() {
        let mut course = course();
        course.error = Vec3::new(0.0, 4.0, 0.0);
        assert!((course.pose_at(1.0).position.y - 4.0).abs() < 1e-3);
    }

    #[test]
    fn a_snapshots_correction_does_not_move_the_drawn_course() {
        let old = course();
        let mut new = course();
        new.current = Vec3::new(107.0, 2.0, 0.0);
        new.previous = new.current;
        new.continue_from(&old);
        for ticks in [0.0, 0.25, 0.5, 1.0] {
            let (before, after) = (old.pose_at(ticks), new.pose_at(ticks));
            assert!(
                before.position.distance(after.position) < 1e-3,
                "{before:?} {after:?}"
            );
            assert_same_turn(before.rotation, after.rotation);
        }
    }

    #[test]
    fn a_correction_too_big_to_smooth_is_taken_at_once() {
        let old = course();
        let mut new = course();
        new.current = Vec3::new(4000.0, 0.0, 0.0);
        new.continue_from(&old);
        assert_eq!(new.error, Vec3::ZERO);
    }

    #[test]
    fn the_blend_starts_at_one_pose_ends_at_the_other_and_eases() {
        assert_eq!(blend(at(0.0), at(10.0), 0.0).position.x, 0.0);
        assert_eq!(blend(at(0.0), at(10.0), 1.0).position.x, 10.0);
        assert_eq!(blend(at(0.0), at(10.0), 2.0).position.x, 10.0);
        assert!((blend(at(0.0), at(10.0), 0.5).position.x - 5.0).abs() < 1e-5);
        assert!(
            blend(at(0.0), at(10.0), 0.1).position.x < 1.0,
            "slow to start"
        );
        assert!(
            blend(at(0.0), at(10.0), 0.9).position.x > 9.0,
            "slow to end"
        );
    }

    #[test]
    fn the_blend_turns_too() {
        let (from, to) = (
            at(0.0),
            BodyPose {
                rotation: Quat::from_rotation_z(1.0),
                ..at(0.0)
            },
        );
        assert_same_turn(blend(from, to, 1.0).rotation, to.rotation);
        assert_same_turn(blend(from, to, 0.5).rotation, Quat::from_rotation_z(0.5));
    }

    #[test]
    fn letting_go_starts_where_the_hold_was_drawn_and_ends_in_the_past() {
        let mut carry = holding();
        let body = BodyId::from_index(3);
        let past = at(50.0);
        let drawn = carry.pose(body, Some(past), 0.5).expect("held");
        carry.settle(None, DT, |_| false, |_| None);
        let eased = carry.pose(body, Some(past), 0.5).expect("let go");
        assert!(drawn.position.distance(eased.position) < 1.0, "{eased:?}");
        // The first ticks run on as it was going, then ease.
        let sim_ticks = (RELEASE_SECS / DT).ceil() as usize;
        for _ in 0..sim_ticks {
            carry.released[0].ticks += 1;
        }
        let last = carry.released[0];
        assert!(last.is_done());
        assert!((last.pose(past, 0.0).position.x - 50.0).abs() < 1e-3);
    }

    #[test]
    fn a_thrown_prop_is_not_eased() {
        let mut carry = holding();
        let body = BodyId::from_index(3);
        carry.settle(None, DT, |thrown| thrown == body, |_| None);
        assert_eq!(carry.pose(body, Some(at(50.0)), 0.5), None);
    }

    #[test]
    fn other_props_are_nothing_to_it() {
        let carry = holding();
        assert_eq!(carry.pose(BodyId::from_index(4), Some(at(0.0)), 0.5), None);
    }

    #[test]
    fn a_grab_starts_from_where_the_prop_was_being_drawn() {
        let mut carry = Carry::default();
        let body = BodyId::from_index(3);
        carry.settle(Some(held(3)), DT, |_| false, |_| Some(at(60.0)));
        let pose = carry.pose(body, None, 1.0).expect("held");
        assert!((pose.position.x - 60.0).abs() < 1e-3, "{pose:?}");
    }

    #[test]
    fn grabbing_a_prop_being_let_go_of_stops_easing_it() {
        let mut carry = holding();
        carry.settle(None, DT, |_| false, |_| None);
        assert_eq!(carry.released.len(), 1);
        carry.settle(Some(held(3)), DT, |_| false, |_| None);
        assert!(carry.released.is_empty());
    }

    #[test]
    fn angles_wrap_the_shorter_way() {
        assert_eq!(wrapped_degrees(20.0), 20.0);
        assert_eq!(wrapped_degrees(-340.0), 20.0);
        assert_eq!(wrapped_degrees(340.0), -20.0);
    }

    #[test]
    fn the_box_is_a_little_smaller_than_the_bounds_and_never_inside_out() {
        let hull = prediction_box((Vec3::splat(-12.0), Vec3::splat(12.0)), Vec3::ZERO);
        assert_eq!(
            (hull.mins, hull.maxs),
            (Vec3::splat(-11.0), Vec3::splat(11.0))
        );
        let thin = prediction_box(
            (Vec3::new(0.0, 0.0, 0.0), Vec3::splat(1.0)),
            Vec3::splat(0.5),
        );
        assert_eq!(thin.mins, Vec3::splat(-0.5));
        assert_eq!(thin.maxs, Vec3::splat(0.5));
    }
}
