//! Tests of the animation decisions on a stand-in model.

use super::*;

fn config() -> PlayerAnimationConfig {
    rl_data::GameData::builtin().expect("data").player.animation
}

/// A model with a few sequences and the pose parameters of a player's.
struct Stub {
    /// Label, length (seconds), loops, ground speed.
    sequences: Vec<(&'static str, f32, bool, f32)>,
}

const PARAMETERS: [&str; 5] = ["aim_pitch", "aim_yaw", "move_yaw", "head_yaw", "head_pitch"];

impl Sequences for Stub {
    fn find(&self, label: &str) -> Option<usize> {
        self.sequences
            .iter()
            .position(|(known, ..)| known.eq_ignore_ascii_case(label))
    }

    fn parameter(&self, name: &str) -> Option<usize> {
        PARAMETERS.iter().position(|known| *known == name)
    }

    fn parameter_count(&self) -> usize {
        PARAMETERS.len()
    }

    fn parameter_range(&self, parameter: usize) -> Option<(f32, f32)> {
        match PARAMETERS.get(parameter)? {
            &"head_yaw" => Some((-45.0, 45.0)),
            &"head_pitch" => Some((-20.0, 23.0)),
            _ => Some((-180.0, 180.0)),
        }
    }

    fn duration(&self, sequence: usize) -> f32 {
        self.sequences[sequence].1
    }

    fn looping(&self, sequence: usize) -> bool {
        self.sequences[sequence].2
    }

    fn ground_speed(&self, sequence: usize, _: &[f32]) -> f32 {
        self.sequences[sequence].3
    }
}

fn stub() -> Stub {
    Stub {
        sequences: vec![
            ("idle_smg1", 1.0, true, 0.0),
            ("run_smg1", 0.6, true, 200.0),
            ("cidle_smg1", 1.0, true, 0.0),
            ("cwalk_smg1", 1.0, true, 60.0),
            ("jump_smg1", 0.8, false, 0.0),
            ("range_smg1", 0.1, false, 0.0),
            ("reload_smg1", 2.0, false, 0.0),
            ("idle_gravgun", 1.0, true, 0.0),
        ],
    }
}

const FRAME: f32 = 1.0 / 60.0;

fn body(eye: (f32, f32), velocity: Vec3, now: f32) -> BodyInput {
    BodyInput {
        eye,
        velocity,
        now,
        dt: FRAME,
    }
}

/// Runs a body for `seconds` from `start` with the same input, returning
/// the last pose and the time reached.
fn run(
    state: &mut Body,
    config: &PlayerAnimationConfig,
    eye: (f32, f32),
    velocity: Vec3,
    start: f32,
    seconds: f32,
) -> (BodyPose, f32) {
    let mut now = start;
    let mut pose = None;
    while now < start + seconds {
        now += FRAME;
        pose = Some(state.update(config, &body(eye, velocity, now)));
    }
    (pose.expect("ran a frame"), now)
}

#[test]
fn angles_wrap_and_approach_the_short_way() {
    assert!((normalize_angle(190.0) + 170.0).abs() < 1e-4);
    assert!((normalize_angle(-190.0) - 170.0).abs() < 1e-4);
    assert!((normalize_angle(720.0)).abs() < 1e-4);
    assert!(
        (approach_angle(10.0, 350.0, 5.0) + 5.0).abs() < 1e-4,
        "across zero"
    );
    assert!(
        (approach_angle(90.0, 0.0, 200.0) - 90.0).abs() < 1e-4,
        "no overshoot"
    );
}

#[test]
fn a_moving_player_faces_the_view_with_the_run_blended_by_direction() {
    let config = config();
    let mut body = Body::default();
    // Running along +Y (yaw 90) while looking along +X.
    let (pose, _) = run(
        &mut body,
        &config,
        (10.0, 0.0),
        Vec3::new(0.0, 190.0, 0.0),
        1.0,
        0.2,
    );
    assert!((pose.feet_yaw).abs() < 1e-4, "the feet face the view");
    assert!((pose.move_yaw - 90.0).abs() < 1e-3, "{}", pose.move_yaw);
    assert!((pose.aim_pitch - 10.0).abs() < 1e-4);
    assert!(pose.aim_yaw.abs() < 1e-4);
}

#[test]
fn a_still_player_twists_before_the_feet_follow() {
    let config = config();
    let mut body = Body::default();
    let (_, now) = run(
        &mut body,
        &config,
        (0.0, 0.0),
        Vec3::new(190.0, 0.0, 0.0),
        1.0,
        0.1,
    );
    let (pose, now) = run(&mut body, &config, (0.0, 0.0), Vec3::ZERO, now, 0.1);
    assert!(pose.feet_yaw.abs() < 1e-3 && pose.aim_yaw.abs() < 1e-3);
    // Looking 30 degrees aside: the torso turns, the feet stay.
    let (pose, now) = run(&mut body, &config, (0.0, 30.0), Vec3::ZERO, now, 0.1);
    assert!(pose.feet_yaw.abs() < 1e-3, "{}", pose.feet_yaw);
    assert!((pose.aim_yaw - 30.0).abs() < 1e-3);
    // 60 degrees is past the twist the body allows: the feet come round.
    let (pose, now) = run(&mut body, &config, (0.0, 60.0), Vec3::ZERO, now, 0.05);
    assert!(
        pose.feet_yaw > 0.0 && pose.feet_yaw < 60.0,
        "turning: {}",
        pose.feet_yaw
    );
    let (pose, _) = run(&mut body, &config, (0.0, 60.0), Vec3::ZERO, now, 1.5);
    assert!((pose.feet_yaw - 60.0).abs() < 0.05, "{}", pose.feet_yaw);
    assert!(pose.aim_yaw.abs() < 0.05);
}

#[test]
fn the_body_twists_no_further_than_the_limit() {
    let config = config();
    let mut body = Body::default();
    let (_, now) = run(&mut body, &config, (0.0, 0.0), Vec3::ZERO, 1.0, 0.1);
    // A sudden half turn: the feet have barely moved a frame later.
    let pose = body.update(&config, &body_input_at((0.0, 120.0), now + FRAME));
    assert!(
        (pose.aim_yaw - config.aim_yaw_limit).abs() < 1e-3,
        "{}",
        pose.aim_yaw
    );
}

fn body_input_at(eye: (f32, f32), now: f32) -> BodyInput {
    body(eye, Vec3::ZERO, now)
}

#[test]
fn feet_catch_up_to_a_view_held_aside_after_the_face_front_time() {
    let config = config();
    let mut body = Body::default();
    let (_, now) = run(&mut body, &config, (0.0, 0.0), Vec3::ZERO, 1.0, 0.1);
    let (pose, now) = run(&mut body, &config, (0.0, 30.0), Vec3::ZERO, now, 2.5);
    assert!(
        pose.feet_yaw.abs() < 1e-3,
        "still at the old heading after 2.5 s"
    );
    let (pose, _) = run(&mut body, &config, (0.0, 30.0), Vec3::ZERO, now, 1.5);
    assert!((pose.feet_yaw - 30.0).abs() < 0.05, "{}", pose.feet_yaw);
}

#[test]
fn feet_turn_at_most_at_the_rate_and_ease_out() {
    let mut feet = 0.0;
    assert!(converge_yaw(170.0, 360.0, 45.0, 0.01, &mut feet));
    assert!((feet - 3.6).abs() < 1e-4, "far away: the full rate");
    let mut feet = 0.0;
    converge_yaw(9.0, 360.0, 45.0, 0.01, &mut feet);
    assert!(
        (feet - 0.72).abs() < 1e-4,
        "{feet}: a fifth of the rate near the goal"
    );
    let mut feet = 9.99;
    assert!(!converge_yaw(10.0, 360.0, 45.0, 0.01, &mut feet));
    assert!((feet - 10.0).abs() < 1e-6, "snaps the last of the way");
}

#[test]
fn a_player_who_stops_lets_their_heading_come_round_to_the_view() {
    let config = config();
    let mut body = Body::default();
    let (pose, now) = run(
        &mut body,
        &config,
        (0.0, 0.0),
        Vec3::new(0.0, 190.0, 0.0),
        1.0,
        0.1,
    );
    assert!((pose.move_yaw - 90.0).abs() < 1e-3);
    let (pose, _) = run(&mut body, &config, (0.0, 0.0), Vec3::ZERO, now, 0.3);
    assert!(
        pose.move_yaw.abs() < 90.0 && pose.move_yaw > 0.0,
        "{}",
        pose.move_yaw
    );
    let (pose, _) = run(&mut body, &config, (0.0, 0.0), Vec3::ZERO, now + 0.3, 2.0);
    assert!(pose.move_yaw.abs() < 0.5, "{}", pose.move_yaw);
}

#[test]
fn heads_turn_at_a_limited_rate_within_their_range() {
    let config = config();
    let range = HeadRange {
        yaw: (-45.0, 45.0),
        pitch: (-20.0, 23.0),
    };
    let mut head = Head::default();
    // Someone off to the side, 90 degrees: the head is held to 45 and
    // gets there at 130 degrees a second.
    let to = Vec3::new(0.0, 100.0, 0.0);
    let (yaw, pitch) = head.update(&config, range, 0.0, to, 0.1);
    assert!((yaw - 13.0).abs() < 1e-3, "{yaw}");
    assert!(pitch.abs() < 1e-3);
    let mut yaw = 0.0;
    for _ in 0..60 {
        yaw = head.update(&config, range, 0.0, to, FRAME).0;
    }
    assert!((yaw - 45.0).abs() < 1e-3, "{yaw}");
    // Looking up at 60 degrees: held to the head's range.
    let up = Vec3::new(50.0, 0.0, 86.6);
    let mut pitch = 0.0;
    for _ in 0..60 {
        pitch = head.update(&config, range, 0.0, up, FRAME).1;
    }
    assert!((pitch + 20.0).abs() < 1e-3, "{pitch}: up is negative");
}

#[test]
fn a_turning_body_carries_the_head_which_turns_back() {
    let config = config();
    let range = HeadRange {
        yaw: (-45.0, 45.0),
        pitch: (-20.0, 23.0),
    };
    let mut head = Head::default();
    let to = Vec3::new(100.0, 0.0, 0.0);
    head.update(&config, range, 0.0, to, FRAME);
    // The body turns 10 degrees in a frame, carrying the head with it. The
    // head is turned back by as much (and sets out for its target, which
    // is now 10 degrees to the side, at 130 degrees a second).
    let (yaw, _) = head.update(&config, range, 10.0, to, FRAME);
    let step = config.head_turn_rate * FRAME;
    assert!((yaw - (-10.0 - step)).abs() < 1e-3, "{yaw}");
}

fn frame_input<'a>(hold: &'a str, velocity: Vec3, now: f32) -> FrameInput<'a> {
    FrameInput {
        body: body((0.0, 0.0), velocity, now),
        on_ground: true,
        ducked: false,
        hold,
        doing: Doing::default(),
        look_at: None,
        eye_height: 64.0,
    }
}

#[test]
fn stances_follow_movement_and_the_weapon_held() {
    let config = config();
    let rig = stub();
    let mut anim = PlayerAnim::default();
    let idle = anim.update(&config, &rig, &frame_input("smg1", Vec3::ZERO, 1.0));
    assert_eq!(idle.main.len(), 1);
    assert_eq!(idle.main[0].sequence, rig.find("idle_smg1").expect("idle"));
    let run = anim.update(
        &config,
        &rig,
        &frame_input("smg1", Vec3::new(190.0, 0.0, 0.0), 1.02),
    );
    assert_eq!(run.main.len(), 2, "the run fades in over the idle");
    assert_eq!(run.main[1].sequence, rig.find("run_smg1").expect("run"));
    assert!(run.main[1].weight < 1.0);
    // Holding something else changes the idle too; a hold the model lacks
    // falls back to the default's.
    let mut anim = PlayerAnim::default();
    let held = anim.update(&config, &rig, &frame_input("gravgun", Vec3::ZERO, 1.0));
    assert_eq!(
        held.main[0].sequence,
        rig.find("idle_gravgun").expect("idle")
    );
    let mut anim = PlayerAnim::default();
    let held = anim.update(&config, &rig, &frame_input("rpg", Vec3::ZERO, 1.0));
    assert_eq!(
        held.main[0].sequence,
        rig.find("idle_smg1").expect("fallback")
    );
}

#[test]
fn the_fade_ends_with_only_the_new_sequence_left() {
    let config = config();
    let rig = stub();
    let mut anim = PlayerAnim::default();
    let mut now = 1.0;
    anim.update(&config, &rig, &frame_input("smg1", Vec3::ZERO, now));
    let mut frame = None;
    for _ in 0..30 {
        now += FRAME;
        frame = Some(anim.update(
            &config,
            &rig,
            &frame_input("smg1", Vec3::new(190.0, 0.0, 0.0), now),
        ));
    }
    let frame = frame.expect("frames");
    assert_eq!(frame.main.len(), 1, "{:?}", frame.main);
    assert!((frame.main[0].weight - 1.0).abs() < 1e-6);
}

#[test]
fn a_run_plays_at_the_pace_of_the_ground_it_covers() {
    let config = config();
    let rig = stub();
    let mut anim = PlayerAnim::default();
    let mut now = 1.0;
    // 100 units a second against a run that covers 200: half speed.
    let mut last = 0.0;
    for _ in 0..30 {
        now += FRAME;
        let frame = anim.update(
            &config,
            &rig,
            &frame_input("smg1", Vec3::new(100.0, 0.0, 0.0), now),
        );
        let cycle = frame.main.last().expect("track").cycle;
        last = cycle;
    }
    // Half a second at half speed: 0.25 s of a 0.6 s loop.
    assert!((last - 0.25 / 0.6).abs() < 0.02, "{last}");
}

#[test]
fn only_a_jump_plays_the_jump_not_a_fall() {
    let config = config();
    let rig = stub();
    let mut anim = PlayerAnim::default();
    let mut now = 1.0;
    anim.update(&config, &rig, &frame_input("smg1", Vec3::ZERO, now));
    // Rising fast off the ground: a jump.
    now += FRAME;
    let mut input = frame_input("smg1", Vec3::new(0.0, 0.0, 268.0), now);
    input.on_ground = false;
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(
        frame.main.last().expect("track").sequence,
        rig.find("jump_smg1").expect("jump")
    );
    // It goes on until they land, though not before the grace.
    now += 0.1;
    let mut input = frame_input("smg1", Vec3::ZERO, now);
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(
        frame.main.last().expect("track").sequence,
        rig.find("jump_smg1").expect("jump")
    );
    now += 0.5;
    input.body.now = now;
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(
        frame.main.last().expect("track").sequence,
        rig.find("idle_smg1").expect("idle"),
        "landed"
    );

    // Walking off a ledge is a fall: the stance plays on.
    let mut anim = PlayerAnim::default();
    anim.update(
        &config,
        &rig,
        &frame_input("smg1", Vec3::new(150.0, 0.0, 0.0), 1.0),
    );
    let mut input = frame_input("smg1", Vec3::new(150.0, 0.0, -50.0), 1.02);
    input.on_ground = false;
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(
        frame.main.last().expect("track").sequence,
        rig.find("run_smg1").expect("run")
    );
}

#[test]
fn a_shot_or_a_reload_plays_a_gesture_over_the_stance_until_it_ends() {
    let config = config();
    let rig = stub();
    let mut anim = PlayerAnim::default();
    let mut now = 1.0;
    let frame = anim.update(&config, &rig, &frame_input("smg1", Vec3::ZERO, now));
    assert!(frame.overlays.is_empty());
    now += FRAME;
    let mut input = frame_input("smg1", Vec3::ZERO, now);
    input.doing.fired = true;
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(frame.overlays.len(), 1);
    assert_eq!(
        frame.overlays[0].sequence,
        rig.find("range_smg1").expect("range")
    );
    assert_eq!(frame.overlays[0].cycle, 0.0);
    // A tenth of a second later it is over.
    for _ in 0..7 {
        now += FRAME;
        anim.update(&config, &rig, &frame_input("smg1", Vec3::ZERO, now));
    }
    let frame = anim.update(&config, &rig, &frame_input("smg1", Vec3::ZERO, now + FRAME));
    assert!(frame.overlays.is_empty(), "{:?}", frame.overlays);
    // A reload replaces the gesture.
    now += FRAME;
    let mut input = frame_input("smg1", Vec3::ZERO, now);
    input.doing.reloaded = true;
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(
        frame.overlays[0].sequence,
        rig.find("reload_smg1").expect("reload")
    );
    now += FRAME;
    let mut input = frame_input("smg1", Vec3::ZERO, now);
    input.doing.fired = true;
    let frame = anim.update(&config, &rig, &input);
    assert_eq!(
        frame.overlays[0].sequence,
        rig.find("range_smg1").expect("range"),
        "a shot restarts it"
    );
}

#[test]
fn the_pose_parameters_carry_the_view_and_the_head() {
    let config = config();
    let rig = stub();
    let mut anim = PlayerAnim::default();
    let mut input = frame_input("smg1", Vec3::ZERO, 1.0);
    input.body.eye = (-30.0, 0.0);
    let mut frame = anim.update(&config, &rig, &input);
    for step in 1..40 {
        input.body.now = 1.0 + step as f32 * FRAME;
        frame = anim.update(&config, &rig, &input);
    }
    let value = |name: &str| frame.parameters[rig.parameter(name).expect("parameter")];
    assert!((value("aim_pitch") + 30.0).abs() < 1e-4);
    assert!(value("aim_yaw").abs() < 1e-4);
    // The head looks up with the view, held to its range.
    assert!(
        value("head_pitch") < 0.0 && value("head_pitch") >= -20.0,
        "{}",
        value("head_pitch")
    );
}
