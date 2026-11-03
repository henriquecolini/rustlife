//! What a player's model is doing, decided as HL2DM's client decides it.
//!
//! Every frame each other player's model is told where its owner looks,
//! how they move and what they hold, and works out from that:
//!
//! - **the body's turn.** While moving, the feet face where the player
//!   looks. Standing still, the body twists to follow the view up to 45
//!   degrees before the feet turn to catch up (at most 360 degrees a
//!   second, easing out), or catch up after three seconds of looking
//!   aside. The model is drawn turned to its *feet*; the rest of the turn
//!   is the `aim_yaw` pose parameter, which the aim matrix of the weapon's
//!   hold turns into a twist of the torso;
//! - **the aim.** `aim_pitch` is the view's pitch, so the same matrix tips
//!   the torso, arms and weapon up and down;
//! - **the run.** `move_yaw` is the direction of travel relative to the
//!   view, blending a run's forward, strafing and backward animations; a
//!   player who stops lets the direction come round to the view;
//! - **the pace.** A moving player's sequences play at the speed over
//!   the ground divided by the ground speed the animation covers;
//! - **the sequence.** Idle, run, crouched or crouch-walking, with the
//!   weapon in hand's hold (`run_smg1`), fading into one another; a jump
//!   (leaving the ground upwards, not merely falling) plays its own until
//!   landing; a shot or a reload plays the weapon's gesture over the stance;
//! - **the head.** It turns, at 130 degrees a second, to look at another
//!   player close by and in front, else where the player looks.
//!
//! None of it needs the models themselves: [`Sequences`] says what the
//! decisions need to know of them, so the logic is tested on its own.

use bevy::math::Vec3;
use rl_assets::{PlayTrack, PoseRig, RigSequence};
use rl_data::PlayerAnimationConfig;

/// The most `aim_pitch` is given, degrees either way.
const MAX_AIM_PITCH: f32 = 90.0;
/// How near a stationary player's velocity must be to nothing for them to
/// count as standing still when turning their feet, squared (units²/s²).
const STILL_SPEED_SQUARED: f32 = 1.000_000_1;

/// `angle` in degrees brought into `-180..=180`.
#[must_use]
pub fn normalize_angle(angle: f32) -> f32 {
    let wrapped = angle.rem_euclid(360.0);
    if wrapped > 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

/// Turns `value` towards `target` by at most `step` degrees, the short way
/// round.
#[must_use]
pub fn approach_angle(target: f32, value: f32, step: f32) -> f32 {
    let delta = normalize_angle(target - value);
    let step = step.abs();
    if delta > step {
        normalize_angle(value + step)
    } else if delta < -step {
        normalize_angle(value - step)
    } else {
        normalize_angle(target)
    }
}

/// What the decisions need to know of a model's sequences and pose
/// parameters.
pub trait Sequences {
    /// The sequence labelled `label`.
    fn find(&self, label: &str) -> Option<usize>;
    /// The pose parameter named `name`.
    fn parameter(&self, name: &str) -> Option<usize>;
    /// How many pose parameters there are.
    fn parameter_count(&self) -> usize;
    /// The range of values a pose parameter takes.
    fn parameter_range(&self, parameter: usize) -> Option<(f32, f32)>;
    /// A sequence's length, seconds.
    fn duration(&self, sequence: usize) -> f32;
    /// Whether a sequence loops.
    fn looping(&self, sequence: usize) -> bool;
    /// The speed over the ground a sequence covers at these pose
    /// parameters, units a second; zero for one that stays in place.
    fn ground_speed(&self, sequence: usize, parameters: &[f32]) -> f32;
}

impl Sequences for PoseRig {
    fn find(&self, label: &str) -> Option<usize> {
        PoseRig::find(self, label)
    }

    fn parameter(&self, name: &str) -> Option<usize> {
        PoseRig::parameter(self, name)
    }

    fn parameter_count(&self) -> usize {
        PoseRig::parameter_count(self)
    }

    fn parameter_range(&self, parameter: usize) -> Option<(f32, f32)> {
        PoseRig::parameter_range(self, parameter)
    }

    fn duration(&self, sequence: usize) -> f32 {
        self.sequence(sequence).map_or(0.0, RigSequence::duration)
    }

    fn looping(&self, sequence: usize) -> bool {
        self.sequence(sequence).is_some_and(|s| s.looping)
    }

    fn ground_speed(&self, sequence: usize, parameters: &[f32]) -> f32 {
        self.sequence(sequence)
            .map_or(0.0, |s| s.ground_speed(parameters))
    }
}

/// Moves `current` towards `goal` as the feet turn: fast, easing out over
/// the last `ease_zone` degrees (never slower than a hundredth of the
/// rate), snapping the last of the way. Returns whether it is still
/// turning.
fn converge_yaw(goal: f32, rate: f32, ease_zone: f32, dt: f32, current: &mut f32) -> bool {
    let gap = normalize_angle(goal - *current);
    let ease = if gap.abs() > ease_zone {
        1.0
    } else {
        (gap.abs() / ease_zone).clamp(0.01, 1.0)
    };
    let step = ease * dt * rate;
    let (value, turning) = if step <= gap.abs() {
        (*current + step.copysign(gap), true)
    } else {
        (goal, false)
    };
    *current = normalize_angle(value);
    turning
}

/// Where a body faces and how it leans, from one frame to the next.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Body {
    started: bool,
    /// Where the feet are turning to.
    goal_feet: f32,
    /// Where the feet face now.
    feet: f32,
    /// When the view or the feet last turned; unset while moving.
    last_turn: Option<f32>,
    /// The view's yaw then.
    last_eye_yaw: f32,
    /// The direction of travel, or the last one, eased round to the view
    /// once still.
    heading: f32,
}

/// What a body is told each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyInput {
    /// Where the player looks: pitch (down is positive) and yaw, degrees.
    pub eye: (f32, f32),
    /// Their velocity, Source units a second.
    pub velocity: Vec3,
    /// Seconds since the game began.
    pub now: f32,
    /// Seconds since the last frame.
    pub dt: f32,
}

/// The body's pose parameters, degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BodyPose {
    /// The way the model faces: where the feet point.
    pub feet_yaw: f32,
    /// The way the player is moving, relative to the view.
    pub move_yaw: f32,
    /// How far the view is above or below the horizon (down positive).
    pub aim_pitch: f32,
    /// How far the view is turned from the feet.
    pub aim_yaw: f32,
}

impl Body {
    /// Decides the body's pose for this frame.
    pub fn update(&mut self, config: &PlayerAnimationConfig, input: &BodyInput) -> BodyPose {
        let eye_yaw = normalize_angle(input.eye.1);
        if !self.started {
            *self = Self {
                started: true,
                goal_feet: eye_yaw,
                feet: eye_yaw,
                last_eye_yaw: eye_yaw,
                heading: eye_yaw,
                last_turn: None,
            };
        }
        self.update_heading(config, input.velocity, eye_yaw, input.dt);
        let move_yaw = normalize_angle(self.heading - eye_yaw);
        let aim_pitch = normalize_angle(input.eye.0).clamp(-MAX_AIM_PITCH, MAX_AIM_PITCH);
        let aim_delta = self.update_feet(config, input, eye_yaw);
        BodyPose {
            feet_yaw: self.feet,
            move_yaw,
            aim_pitch,
            aim_yaw: aim_delta.clamp(-config.aim_yaw_limit, config.aim_yaw_limit),
        }
    }

    /// The direction of travel: that of the velocity while there is one;
    /// once still, it comes round to the view.
    fn update_heading(
        &mut self,
        config: &PlayerAnimationConfig,
        velocity: Vec3,
        eye_yaw: f32,
        dt: f32,
    ) {
        if dt <= 0.0 {
            return;
        }
        if velocity.x == 0.0 && velocity.y == 0.0 {
            let delta = normalize_angle(eye_yaw - self.heading);
            // A long frame takes the whole way at once.
            let share = if dt < 0.25 {
                dt * config.heading_ease
            } else {
                dt
            };
            self.heading = normalize_angle(self.heading + delta * share);
        } else {
            self.heading = velocity.y.atan2(velocity.x).to_degrees();
        }
    }

    /// Turns the feet as the player's view and movement ask; returns how
    /// far the view is turned from them.
    fn update_feet(
        &mut self,
        config: &PlayerAnimationConfig,
        input: &BodyInput,
        eye_yaw: f32,
    ) -> f32 {
        if input.velocity.length_squared() > STILL_SPEED_SQUARED {
            // Moving, the feet face the view straight away.
            self.last_turn = None;
            self.goal_feet = eye_yaw;
            self.feet = eye_yaw;
            self.last_eye_yaw = eye_yaw;
            return 0.0;
        }
        if self.last_turn.is_none() {
            // Just stopped: the feet stay as the view had them.
            self.last_turn = Some(input.now);
            self.last_eye_yaw = eye_yaw;
            self.goal_feet = eye_yaw;
            self.feet = eye_yaw;
        } else {
            if (self.last_eye_yaw - eye_yaw).abs() > f32::EPSILON {
                self.last_turn = Some(input.now);
                self.last_eye_yaw = eye_yaw;
            }
            if (self.goal_feet - self.feet).abs() > f32::EPSILON {
                self.last_turn = Some(input.now);
            }
        }
        converge_yaw(
            self.goal_feet,
            config.feet_turn_rate,
            config.feet_ease_zone,
            input.dt,
            &mut self.feet,
        );
        let aim_delta = normalize_angle(eye_yaw - self.feet);
        let idle_for = self.last_turn.map_or(0.0, |turned| input.now - turned);
        if aim_delta.abs() > config.max_twist || idle_for > config.face_front_time {
            self.goal_feet = eye_yaw;
            self.last_turn = Some(input.now);
        }
        aim_delta
    }
}

/// The ranges a head can turn through, as the model gives them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadRange {
    /// Yaw: left and right of the body.
    pub yaw: (f32, f32),
    /// Pitch: up and down.
    pub pitch: (f32, f32),
}

/// The head, turning to look.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Head {
    yaw: f32,
    pitch: f32,
    last_body_yaw: Option<f32>,
}

impl Head {
    /// Turns the head towards `to_target` (the vector from the eyes to what
    /// it looks at, Source space) for a body facing `body_yaw`; returns its
    /// yaw and pitch pose parameters.
    pub fn update(
        &mut self,
        config: &PlayerAnimationConfig,
        range: HeadRange,
        body_yaw: f32,
        to_target: Vec3,
        dt: f32,
    ) -> (f32, f32) {
        let horizontal = to_target.truncate().length();
        let target_yaw = to_target.y.atan2(to_target.x).to_degrees();
        // Down is positive, as for the view.
        let target_pitch = (-to_target.z).atan2(horizontal).to_degrees();
        let step = config.head_turn_rate * dt;
        let previous_body_yaw = self.last_body_yaw.replace(body_yaw).unwrap_or(body_yaw);
        let wanted_yaw = normalize_angle(target_yaw - body_yaw).clamp(range.yaw.0, range.yaw.1);
        let turned = approach_angle(wanted_yaw, self.yaw, step);
        // The body turning carries the head with it: undo that, so the
        // head does not turn past what it looks at.
        self.yaw = normalize_angle(turned + previous_body_yaw - body_yaw);
        let wanted_pitch = normalize_angle(target_pitch).clamp(range.pitch.0, range.pitch.1);
        self.pitch = normalize_angle(approach_angle(wanted_pitch, self.pitch, step));
        (self.yaw, self.pitch)
    }
}

/// A sequence of the stance, playing or fading.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Playing {
    sequence: usize,
    /// Seconds into it.
    time: f32,
    /// Its share of the pose.
    weight: f32,
}

impl Playing {
    fn cycle(&self, rig: &impl Sequences) -> f32 {
        let duration = rig.duration(self.sequence);
        if duration <= 0.0 {
            return 0.0;
        }
        let cycle = self.time / duration;
        if rig.looping(self.sequence) {
            cycle.rem_euclid(1.0)
        } else {
            cycle.min(1.0)
        }
    }

    fn track(&self, rig: &impl Sequences) -> PlayTrack {
        PlayTrack {
            sequence: self.sequence,
            cycle: self.cycle(rig),
            weight: self.weight,
        }
    }
}

/// What a player did since the last frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Doing {
    /// Pulled the trigger.
    pub fired: bool,
    /// Began a reload.
    pub reloaded: bool,
}

/// How a stance is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stance {
    Idle,
    Run,
    Crouch,
    CrouchWalk,
    Jump,
}

/// Everything a model is told each frame.
#[derive(Debug, Clone, Copy)]
pub struct FrameInput<'a> {
    /// Where they look, how they move.
    pub body: BodyInput,
    /// Whether they stand on the ground (or a ladder).
    pub on_ground: bool,
    /// Whether they are crouched.
    pub ducked: bool,
    /// How the weapon in hand is held (`smg1`).
    pub hold: &'a str,
    /// What they just did.
    pub doing: Doing,
    /// Where they look at, relative to their eyes, if at another player;
    /// else straight ahead.
    pub look_at: Option<Vec3>,
    /// The height of their eyes above their feet.
    pub eye_height: f32,
}

/// What to draw this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// The way the model faces.
    pub feet_yaw: f32,
    /// Pose parameter values, by the rig's parameter index.
    pub parameters: Vec<f32>,
    /// The sequences of the stance, the first as it is and each of the
    /// others blended over what comes before.
    pub main: Vec<PlayTrack>,
    /// The gesture over them, if one is playing.
    pub overlays: Vec<PlayTrack>,
}

/// A player model's whole animation state.
#[derive(Debug, Clone, Default)]
pub struct PlayerAnim {
    body: Body,
    head: Head,
    /// The stance's sequences: the one fading in last. Earlier ones are
    /// those it is replacing.
    main: Vec<Playing>,
    gesture: Option<Playing>,
    /// When the current jump began.
    jumping_since: Option<f32>,
    was_on_ground: bool,
    /// Whether the jump sequence must start over (a jump began).
    restart: bool,
}

impl PlayerAnim {
    /// Moves the state on one frame and says what to draw.
    pub fn update(
        &mut self,
        config: &PlayerAnimationConfig,
        rig: &impl Sequences,
        input: &FrameInput,
    ) -> Frame {
        let body_input = &input.body;
        let pose = self.body.update(config, body_input);
        let mut parameters = vec![0.0; rig.parameter_count()];
        let mut set = |name: &str, value: f32| {
            if let Some(slot) = rig
                .parameter(name)
                .and_then(|index| parameters.get_mut(index))
            {
                *slot = value;
            }
        };
        set("move_yaw", pose.move_yaw);
        set("aim_pitch", pose.aim_pitch);
        set("aim_yaw", pose.aim_yaw);
        self.update_head(config, rig, input, &mut set);

        self.track_jump(config, input);
        let stance = self.stance(config, input);
        let speed = body_input.velocity.truncate().length();
        if let Some(sequence) = Self::stance_sequence(config, rig, input, stance) {
            self.play(config, sequence);
        }
        // The pace follows the ground, once the pose parameters are known.
        let rate = self
            .main
            .last()
            .map(|playing| rig.ground_speed(playing.sequence, &parameters))
            .filter(|&ground| speed * speed > 0.25 && ground > 0.0)
            .map_or(1.0, |ground| speed / ground);
        self.advance(config, body_input.dt, rate);
        self.update_gesture(config, rig, input);

        Frame {
            feet_yaw: pose.feet_yaw,
            parameters,
            main: self.main.iter().map(|playing| playing.track(rig)).collect(),
            overlays: self
                .gesture
                .iter()
                .map(|playing| playing.track(rig))
                .collect(),
        }
    }

    fn update_head(
        &mut self,
        config: &PlayerAnimationConfig,
        rig: &impl Sequences,
        input: &FrameInput,
        set: &mut impl FnMut(&str, f32),
    ) {
        let range = |name: &str| {
            rig.parameter(name)
                .and_then(|index| rig.parameter_range(index))
        };
        let (Some(yaw), Some(pitch)) = (range("head_yaw"), range("head_pitch")) else {
            return;
        };
        if input.body.dt <= 0.0 {
            return;
        }
        let (eye_pitch, eye_yaw) = input.body.eye;
        // Straight ahead along the view, from the feet, seen from the eyes.
        let to_target = input.look_at.unwrap_or_else(|| {
            let (pitch, yaw) = (eye_pitch.to_radians(), eye_yaw.to_radians());
            let ahead = Vec3::new(
                pitch.cos() * yaw.cos(),
                pitch.cos() * yaw.sin(),
                -pitch.sin(),
            ) * config.look_ahead;
            ahead - Vec3::Z * input.eye_height
        });
        let (head_yaw, head_pitch) = self.head.update(
            config,
            HeadRange {
                yaw: ordered(yaw),
                pitch: ordered(pitch),
            },
            normalize_angle(eye_yaw),
            to_target,
            input.body.dt,
        );
        set("head_yaw", head_yaw);
        set("head_pitch", head_pitch);
    }

    /// Notices a jump, and its end.
    fn track_jump(&mut self, config: &PlayerAnimationConfig, input: &FrameInput) {
        let now = input.body.now;
        if self.was_on_ground
            && !input.on_ground
            && input.body.velocity.z > config.jump_speed
            && self.jumping_since.is_none()
        {
            self.jumping_since = Some(now);
            self.restart = true;
        }
        if let Some(since) = self.jumping_since
            && now - since > config.jump_grace
            && input.on_ground
        {
            self.jumping_since = None;
        }
        self.was_on_ground = input.on_ground;
    }

    fn stance(&self, config: &PlayerAnimationConfig, input: &FrameInput) -> Stance {
        let moving = input.body.velocity.truncate().length() > config.still_speed;
        match (self.jumping_since.is_some(), input.ducked, moving) {
            (true, ..) => Stance::Jump,
            (_, true, true) => Stance::CrouchWalk,
            (_, true, false) => Stance::Crouch,
            (_, false, true) => Stance::Run,
            (_, false, false) => Stance::Idle,
        }
    }

    /// The sequence for the stance with the weapon in hand, falling back to
    /// the default hold's.
    fn stance_sequence(
        config: &PlayerAnimationConfig,
        rig: &impl Sequences,
        input: &FrameInput,
        stance: Stance,
    ) -> Option<usize> {
        let prefix = match stance {
            Stance::Idle => &config.idle,
            Stance::Run => &config.run,
            Stance::Crouch => &config.crouch,
            Stance::CrouchWalk => &config.crouch_walk,
            Stance::Jump => &config.jump,
        };
        rig.find(&format!("{prefix}_{}", input.hold))
            .or_else(|| rig.find(&format!("{prefix}_{}", config.default_hold)))
    }

    /// Starts `sequence` if it is not what is playing (or a jump starts
    /// over), fading in over what plays.
    fn play(&mut self, config: &PlayerAnimationConfig, sequence: usize) {
        let restart = std::mem::take(&mut self.restart);
        let same = self
            .main
            .last()
            .is_some_and(|playing| playing.sequence == sequence);
        if same && !restart {
            return;
        }
        let first = self.main.is_empty();
        self.main.push(Playing {
            sequence,
            time: 0.0,
            weight: if first || config.fade <= 0.0 {
                1.0
            } else {
                0.0
            },
        });
        if self
            .main
            .last()
            .is_some_and(|playing| playing.weight >= 1.0)
        {
            self.main.drain(..self.main.len() - 1);
        }
    }

    /// Moves the stance's sequences on, fading the newest in and dropping
    /// what it has replaced.
    fn advance(&mut self, config: &PlayerAnimationConfig, dt: f32, rate: f32) {
        for playing in &mut self.main {
            playing.time += dt * rate;
        }
        if let Some(newest) = self.main.last_mut() {
            newest.weight = if config.fade > 0.0 {
                (newest.weight + dt / config.fade).min(1.0)
            } else {
                1.0
            };
            if newest.weight >= 1.0 {
                let keep = self.main.len() - 1;
                self.main.drain(..keep);
            }
        }
    }

    /// Starts the gesture of what the player just did and moves it on.
    fn update_gesture(
        &mut self,
        config: &PlayerAnimationConfig,
        rig: &impl Sequences,
        input: &FrameInput,
    ) {
        let gesture = |prefix: &str| {
            rig.find(&format!("{prefix}_{}", input.hold))
                .or_else(|| rig.find(&format!("{prefix}_{}", config.default_hold)))
        };
        let wanted = if input.doing.reloaded {
            gesture(&config.reload)
        } else if input.doing.fired {
            gesture(&config.attack)
        } else {
            None
        };
        if let Some(sequence) = wanted {
            self.gesture = Some(Playing {
                sequence,
                time: 0.0,
                weight: 1.0,
            });
        } else if let Some(playing) = &mut self.gesture {
            playing.time += input.body.dt;
        }
        // A gesture ends when its sequence does.
        if let Some(playing) = &self.gesture
            && playing.time >= rig.duration(playing.sequence)
        {
            self.gesture = None;
        }
    }
}

/// `range` with its smaller end first.
fn ordered((a, b): (f32, f32)) -> (f32, f32) {
    (a.min(b), a.max(b))
}

#[cfg(test)]
mod tests;
