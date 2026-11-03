//! Tests of pose evaluation on a two-bone model: a root and an arm resting
//! ten units above it.

use source_formats::anim::{Bone, BoneTrack, PoseParameter};

use super::*;

fn bone(name: &str, parent: Option<usize>) -> Bone {
    Bone {
        name: name.to_owned(),
        parent,
        position: [0.0, 0.0, 10.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        euler: [0.0; 3],
        position_scale: [1.0; 3],
        rotation_scale: [1.0; 3],
    }
}

/// A quarter turn is `angle` radians about the Source Z axis, as a
/// quaternion.
fn about_z(angle: f32) -> [f32; 4] {
    [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()]
}

fn about_x(angle: f32) -> [f32; 4] {
    [(angle / 2.0).sin(), 0.0, 0.0, (angle / 2.0).cos()]
}

/// A one-frame animation turning the arm to `rotation` and moving the
/// model `movement`.
fn animation(rotation: [f32; 4], delta: bool, movement: [f32; 3]) -> Animation {
    Animation {
        name: "a".to_owned(),
        fps: 30.0,
        frames: 2,
        looping: false,
        delta,
        tracks: vec![
            BoneTrack {
                bone: 0,
                positions: vec![[0.0; 3]],
                rotations: vec![[0.0, 0.0, 0.0, 1.0]],
            },
            BoneTrack {
                bone: 1,
                positions: vec![if delta { [0.0; 3] } else { [0.0, 0.0, 10.0] }],
                rotations: vec![rotation],
            },
        ],
        movement,
    }
}

fn sequence(label: &str, blends: Vec<usize>) -> Sequence {
    Sequence {
        label: label.to_owned(),
        activity: String::new(),
        looping: false,
        delta: false,
        post: false,
        weights: Vec::new(),
        layers: Vec::new(),
        blends,
        group: [1, 1],
        parameters: [None, None],
        ranges: [(0.0, 0.0); 2],
        fade: (0.2, 0.2),
        events: Vec::new(),
    }
}

fn model(animations: Vec<Animation>, sequences: Vec<Sequence>) -> ModelAnimation {
    ModelAnimation {
        bones: vec![bone("root", None), bone("arm", Some(0))],
        pose_parameters: vec![PoseParameter {
            name: "Move_Yaw".to_owned(),
            start: -180.0,
            end: 180.0,
            loop_range: 0.0,
        }],
        animations,
        sequences,
        ..ModelAnimation::default()
    }
}

fn rig(model: ModelAnimation) -> PoseRig {
    PoseRig::build(&Skeleton::of(&model), &[model])
}

/// The angle the arm is turned by, radians.
fn arm_angle(pose: &[Transform], rest: &PoseRig) -> f32 {
    pose[1].rotation.angle_between(rest.rest[1].rotation)
}

fn play(sequence: usize, cycle: f32, weight: f32) -> PlayTrack {
    PlayTrack {
        sequence,
        cycle,
        weight,
    }
}

#[test]
fn nothing_playing_leaves_the_rest_pose() {
    let rig = rig(model(Vec::new(), Vec::new()));
    let pose = rig.evaluate(&[], &[], &[]);
    assert_eq!(pose[1], rig.rest[1]);
}

#[test]
fn a_sequence_sets_the_pose_it_holds() {
    let rig = rig(model(
        vec![animation(about_z(1.0), false, [0.0; 3])],
        vec![sequence("turn", vec![0])],
    ));
    let pose = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[]);
    assert!((arm_angle(&pose, &rig) - 1.0).abs() < 1e-4);
}

#[test]
fn blends_follow_the_pose_parameter() {
    let mut turn = sequence("turn", vec![0, 1]);
    turn.group = [2, 1];
    turn.parameters = [Some(0), None];
    turn.ranges = [(-180.0, 180.0), (0.0, 0.0)];
    let rig = rig(model(
        vec![
            animation(about_z(0.0), false, [0.0; 3]),
            animation(about_z(1.0), false, [0.0; 3]),
        ],
        vec![turn],
    ));
    let yaw = rig.parameter("move_yaw").expect("parameter");
    for (value, expected) in [(-180.0, 0.0), (0.0, 0.5), (90.0, 0.75), (500.0, 1.0)] {
        let mut parameters = vec![0.0; rig.parameter_count()];
        parameters[yaw] = value;
        let pose = rig.evaluate(&parameters, &[play(0, 0.0, 1.0)], &[]);
        assert!(
            (arm_angle(&pose, &rig) - expected).abs() < 1e-3,
            "{value}: {}",
            arm_angle(&pose, &rig)
        );
    }
}

#[test]
fn a_sequence_fading_in_blends_over_the_one_before() {
    let rig = rig(model(
        vec![
            animation(about_z(0.0), false, [0.0; 3]),
            animation(about_z(1.0), false, [0.0; 3]),
        ],
        vec![sequence("old", vec![0]), sequence("new", vec![1])],
    ));
    let pose = rig.evaluate(&[], &[play(0, 0.0, 1.0), play(1, 0.0, 0.25)], &[]);
    assert!((arm_angle(&pose, &rig) - 0.25).abs() < 1e-3);
}

#[test]
fn deltas_turn_by_the_weight_and_the_bones_share() {
    let mut aim = sequence("aim", vec![1]);
    aim.delta = true;
    aim.weights = vec![0.0, 0.5];
    let rig = rig(model(
        vec![
            animation(about_z(0.0), false, [0.0; 3]),
            animation(about_z(1.0), true, [0.0; 3]),
        ],
        vec![sequence("base", vec![0]), aim],
    ));
    let pose = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[play(1, 0.0, 1.0)]);
    assert!(
        (arm_angle(&pose, &rig) - 0.5).abs() < 1e-3,
        "half the bone's share"
    );
    assert!(
        pose[0].rotation.abs_diff_eq(rig.rest[0].rotation, 1e-6),
        "the root is weighted out"
    );
    let pose = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[play(1, 0.0, 0.5)]);
    assert!(
        (arm_angle(&pose, &rig) - 0.25).abs() < 1e-3,
        "half the weight"
    );
}

#[test]
fn a_post_delta_turns_in_the_bones_own_frame() {
    let mut aim = sequence("aim", vec![1]);
    aim.delta = true;
    aim.post = true;
    let mut before = aim_clone(&aim);
    before.post = false;
    let rig = rig(model(
        vec![
            animation(about_x(0.8), false, [0.0; 3]),
            animation(about_z(0.6), true, [0.0; 3]),
        ],
        vec![sequence("base", vec![0]), aim, before],
    ));
    let base = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[])[1].rotation;
    let post = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[play(1, 0.0, 1.0)])[1].rotation;
    let pre = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[play(2, 0.0, 1.0)])[1].rotation;
    let turn = local_transform([0.0; 3], about_z(0.6)).rotation;
    assert!(post.abs_diff_eq(base * turn, 1e-4));
    assert!(pre.abs_diff_eq(turn * base, 1e-4));
    assert!(!post.abs_diff_eq(pre, 1e-3), "the order matters");
}

fn aim_clone(sequence: &Sequence) -> Sequence {
    Sequence {
        label: "aim_pre".to_owned(),
        ..sequence.clone()
    }
}

#[test]
fn autolayers_play_along_with_their_sequence() {
    let mut aim = sequence("aim", vec![1]);
    aim.delta = true;
    aim.post = true;
    let mut idle = sequence("idle", vec![0]);
    idle.layers = vec![AutoLayer {
        sequence: 1,
        flags: 0,
        cycle: [0.0; 4],
    }];
    let rig = rig(model(
        vec![
            animation(about_z(0.0), false, [0.0; 3]),
            animation(about_z(0.4), true, [0.0; 3]),
        ],
        vec![idle, aim],
    ));
    let pose = rig.evaluate(&[], &[play(0, 0.0, 1.0)], &[]);
    assert!((arm_angle(&pose, &rig) - 0.4).abs() < 1e-3);
    // Faded to half, the layer turns half as much.
    let pose = rig.evaluate(&[], &[play(1, 0.0, 1.0), play(0, 0.0, 0.5)], &[]);
    assert!(arm_angle(&pose, &rig) < 0.4);
}

#[test]
fn a_layer_weight_ramps_between_its_marks() {
    let layer = Layer {
        sequence: 0,
        flags: 0,
        cycle: [0.0, 0.25, 0.5, 1.0],
    };
    assert_eq!(layer.weight(0.125, 0.8), Some(0.4));
    assert_eq!(layer.weight(0.4, 0.8), Some(0.8));
    assert!((layer.weight(0.75, 1.0).expect("fading out") - 0.5).abs() < 1e-6);
    assert_eq!(layer.weight(1.0, 1.0), None, "over");
    let always = Layer {
        cycle: [0.0; 4],
        ..layer
    };
    assert_eq!(always.weight(0.9, 0.3), Some(0.3));
    let no_blend = Layer {
        flags: LAYER_NO_BLEND,
        ..layer
    };
    assert_eq!(no_blend.weight(0.125, 0.8), Some(0.5));
}

#[test]
fn ground_speed_follows_the_blend_and_the_length() {
    let mut run = sequence("run", vec![0, 1]);
    run.group = [2, 1];
    run.parameters = [Some(0), None];
    run.ranges = [(-180.0, 180.0), (0.0, 0.0)];
    let mut forward = animation(about_z(0.0), false, [60.0, 0.0, 0.0]);
    forward.frames = 31;
    let mut back = animation(about_z(0.0), false, [-30.0, 0.0, 0.0]);
    back.frames = 31;
    let rig = rig(model(vec![forward, back], vec![run]));
    let run = rig.sequence(0).expect("run");
    let yaw = rig.parameter("move_yaw").expect("parameter");
    let mut parameters = vec![0.0; rig.parameter_count()];
    parameters[yaw] = -180.0;
    assert!(
        (run.ground_speed(&parameters) - 60.0).abs() < 1e-3,
        "60 units a second"
    );
    parameters[yaw] = 180.0;
    assert!((run.ground_speed(&parameters) - 30.0).abs() < 1e-3);
    parameters[yaw] = 0.0;
    assert!(
        (run.ground_speed(&parameters) - 15.0).abs() < 1e-3,
        "they cancel part out"
    );
}

#[test]
fn sequences_are_found_by_label_and_parameters_by_name() {
    let rig = rig(model(
        vec![animation(about_z(0.0), false, [0.0; 3])],
        vec![sequence("Idle_SMG1", vec![0])],
    ));
    assert_eq!(rig.find("idle_smg1"), Some(0));
    assert_eq!(rig.find("idle_ar2"), None);
    assert_eq!(rig.parameter("MOVE_YAW"), None, "no sequence uses it");
}
