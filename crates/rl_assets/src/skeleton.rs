//! Skeletons and animation clips of skeletal models, in Bevy terms.
//!
//! `source_formats::anim` decodes a model's bones and sequences in Source
//! space. Here they become what Bevy animates with:
//!
//! - a [`Skeleton`] of joints, each with its rest transform relative to its
//!   parent in Bevy space, and the inverse bind poses its skinned meshes
//!   need;
//! - one [`AnimationClip`] per blend of every sequence ([`SequenceClips`]),
//!   with a translation and a rotation curve for each joint, keyed by the
//!   joint's name ([`Skeleton::target`]).
//!
//! Sequences of included models are matched to this model's skeleton by
//! bone name, as the engine matches them; their bones this model lacks are
//! dropped.
//!
//! Source space maps to Bevy space by a fixed rotation and scale ([`coords`]),
//! so each local transform converts on its own: positions as positions,
//! rotations conjugated by the axis change.

use std::collections::HashMap;

use bevy_animation::AnimationTargetId;
use bevy_animation::animated_field;
use bevy_animation::prelude::{AnimatableCurve, AnimatableKeyframeCurve, AnimationClip};
use bevy_asset::Handle;
use bevy_math::{Mat4, Quat, Vec3};
use bevy_transform::components::Transform;
use source_formats::anim::{Animation, ModelAnimation, Sequence};

use crate::coords;

/// One joint of a skeleton.
#[derive(Debug, Clone, PartialEq)]
pub struct Joint {
    /// The bone's name.
    pub name: String,
    /// The joint it hangs from.
    pub parent: Option<usize>,
    /// Its rest transform relative to the parent, Bevy space.
    pub rest: Transform,
}

/// A skeleton in Bevy space.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Skeleton {
    /// Its joints, parents before children.
    pub joints: Vec<Joint>,
}

impl Skeleton {
    /// The skeleton of a decoded model.
    #[must_use]
    pub fn of(model: &ModelAnimation) -> Self {
        Self {
            joints: model
                .bones
                .iter()
                .map(|bone| Joint {
                    name: bone.name.clone(),
                    parent: bone.parent,
                    rest: local_transform(bone.position, bone.rotation),
                })
                .collect(),
        }
    }

    /// The animation target of joint `index`: its name, hashed. The client
    /// gives each joint entity this id.
    #[must_use]
    pub fn target(&self, index: usize) -> AnimationTargetId {
        target_of(
            self.joints
                .get(index)
                .map_or("", |joint| joint.name.as_str()),
        )
    }

    /// Each joint's rest transform relative to the model's origin.
    #[must_use]
    pub fn rest_globals(&self) -> Vec<Mat4> {
        let mut globals: Vec<Mat4> = Vec::with_capacity(self.joints.len());
        for joint in &self.joints {
            let local = joint.rest.to_matrix();
            let global = match joint.parent.and_then(|parent| globals.get(parent)) {
                Some(parent) => *parent * local,
                None => local,
            };
            globals.push(global);
        }
        globals
    }

    /// The inverse bind poses of skinned meshes: each joint's rest global
    /// transform, inverted.
    #[must_use]
    pub fn inverse_bindposes(&self) -> Vec<Mat4> {
        self.rest_globals().iter().map(Mat4::inverse).collect()
    }

    /// The joint named `name`.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<usize> {
        self.joints
            .iter()
            .position(|joint| joint.name.eq_ignore_ascii_case(name))
    }
}

/// The animation target of a joint named `name`.
#[must_use]
pub fn target_of(name: &str) -> AnimationTargetId {
    AnimationTargetId::from_iter([name.to_ascii_lowercase()])
}

/// A Source-space local transform as a Bevy one.
fn local_transform(position: [f32; 3], rotation: [f32; 4]) -> Transform {
    Transform {
        translation: coords::position(position),
        rotation: coords::rotation_from_source(quat(rotation)),
        scale: Vec3::ONE,
    }
}

fn quat([x, y, z, w]: [f32; 4]) -> Quat {
    Quat::from_xyzw(x, y, z, w).normalize()
}

/// What playing a sequence takes: its clips and how to blend them.
#[derive(Debug, Clone)]
pub struct SequenceClips {
    /// Its label (`run_smg1`), lower case.
    pub label: String,
    /// Its activity (`ACT_HL2MP_RUN_SMG1`), upper case; possibly empty.
    pub activity: String,
    /// Whether it loops.
    pub looping: bool,
    /// Whether it is a delta, layered over whatever else plays (a weapon's
    /// prongs opening) rather than played by itself.
    pub delta: bool,
    /// One clip per blend, in the sequence's grid order.
    pub clips: Vec<Handle<AnimationClip>>,
    /// The blend grid's size.
    pub group: [usize; 2],
    /// Each grid axis' pose parameter, by name (lower case), with the
    /// values at its first and last column or row.
    pub axes: [Option<(String, f32, f32)>; 2],
    /// Its length in seconds.
    pub duration: f32,
    /// Its events: cycle and options (sounds, effects), as the model has
    /// them.
    pub events: Vec<(f32, i32, String)>,
}

impl SequenceClips {
    /// The blends to mix for a value of the first axis' parameter, with
    /// their weights: the two columns either side of it (for a sequence
    /// blended in one dimension, such as a run by `move_yaw`). Sequences
    /// with one blend give it alone.
    #[must_use]
    pub fn blend_1d(&self, value: f32) -> Vec<(usize, f32)> {
        let columns = self.group[0];
        let Some((_, start, end)) = &self.axes[0] else {
            return vec![(0, 1.0)];
        };
        if columns < 2 || (end - start).abs() < f32::EPSILON {
            return vec![(0, 1.0)];
        }
        let position = ((value - start) / (end - start)).clamp(0.0, 1.0) * (columns - 1) as f32;
        let low = (position.floor() as usize).min(columns - 2);
        let fraction = position - low as f32;
        vec![(low, 1.0 - fraction), (low + 1, fraction)]
    }
}

/// Builds the clips of `sequences` (from `source`, whose bones are matched
/// to `skeleton`'s by name), adding each clip with `add`. Delta sequences
/// (layers such as aim matrices and gestures) are skipped unless
/// `with_deltas`: a model's own few (a weapon's prongs) are wanted, the many
/// of a player's animation set are not.
pub fn sequence_clips(
    skeleton: &Skeleton,
    source: &ModelAnimation,
    with_deltas: bool,
    add: &mut dyn FnMut(AnimationClip) -> Handle<AnimationClip>,
) -> Vec<SequenceClips> {
    // Each of the source's bones' joint in this skeleton.
    let joints: Vec<Option<usize>> = source
        .bones
        .iter()
        .map(|bone| skeleton.find(&bone.name))
        .collect();
    let mut cache: HashMap<usize, Handle<AnimationClip>> = HashMap::new();
    source
        .sequences
        .iter()
        .filter(|sequence| with_deltas || !sequence.delta)
        .map(|sequence| {
            let clips = sequence
                .blends
                .iter()
                .map(|&animation| {
                    cache
                        .entry(animation)
                        .or_insert_with(|| {
                            add(clip(skeleton, &joints, &source.animations[animation]))
                        })
                        .clone()
                })
                .collect();
            let duration = sequence
                .blends
                .first()
                .map_or(0.0, |&animation| duration(&source.animations[animation]));
            sequence_of(sequence, source, clips, duration)
        })
        .collect()
}

fn sequence_of(
    sequence: &Sequence,
    source: &ModelAnimation,
    clips: Vec<Handle<AnimationClip>>,
    duration: f32,
) -> SequenceClips {
    let axis = |axis: usize| {
        let parameter = source.pose_parameters.get(sequence.parameters[axis]?)?;
        let (start, end) = sequence.ranges[axis];
        Some((parameter.name.to_ascii_lowercase(), start, end))
    };
    SequenceClips {
        label: sequence.label.to_ascii_lowercase(),
        activity: sequence.activity.to_ascii_uppercase(),
        looping: sequence.looping,
        delta: sequence.delta,
        clips,
        group: sequence.group,
        axes: [axis(0), axis(1)],
        duration,
        events: sequence
            .events
            .iter()
            .map(|event| (event.cycle, event.event, event.options.clone()))
            .collect(),
    }
}

/// An animation's length: from its first frame to its last.
fn duration(animation: &Animation) -> f32 {
    (animation.frames.max(2) - 1) as f32 / animation.fps.max(1.0)
}

/// The clip of one animation: a translation and a rotation curve for every
/// bone it has a joint for.
fn clip(skeleton: &Skeleton, joints: &[Option<usize>], animation: &Animation) -> AnimationClip {
    let mut clip = AnimationClip::default();
    let length = duration(animation);
    let step = 1.0 / animation.fps.max(1.0);
    let times = |count: usize| -> Vec<f32> {
        if count < 2 {
            // A held value: one keyframe at each end.
            vec![0.0, length]
        } else {
            (0..count).map(|frame| frame as f32 * step).collect()
        }
    };
    for track in &animation.tracks {
        let Some(joint) = joints.get(track.bone).copied().flatten() else {
            continue;
        };
        let target = skeleton.target(joint);
        let positions: Vec<Vec3> = track
            .positions
            .iter()
            .map(|&p| coords::position(p))
            .collect();
        let rotations: Vec<Quat> = track
            .rotations
            .iter()
            .map(|&r| coords::rotation_from_source(quat(r)))
            .collect();
        let keyed = |values: Vec<Vec3>| {
            let times = times(values.len());
            let values = if values.len() < 2 {
                vec![values[0]; 2]
            } else {
                values
            };
            times.into_iter().zip(values)
        };
        if let Ok(curve) = AnimatableKeyframeCurve::new(keyed(positions)) {
            clip.add_curve_to_target(
                target,
                AnimatableCurve::new(animated_field!(Transform::translation), curve),
            );
        }
        let rotation_times = times(rotations.len());
        let rotations = if rotations.len() < 2 {
            vec![rotations[0]; 2]
        } else {
            rotations
        };
        if let Ok(curve) = AnimatableKeyframeCurve::new(rotation_times.into_iter().zip(rotations)) {
            clip.add_curve_to_target(
                target,
                AnimatableCurve::new(animated_field!(Transform::rotation), curve),
            );
        }
    }
    clip
}

#[cfg(test)]
mod tests {
    use source_formats::anim::{BoneTrack, PoseParameter};

    use super::*;

    fn model() -> ModelAnimation {
        let bone = |name: &str, parent| source_formats::anim::Bone {
            name: name.to_owned(),
            parent,
            position: [0.0, 0.0, 10.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            euler: [0.0; 3],
            position_scale: [1.0; 3],
            rotation_scale: [1.0; 3],
        };
        ModelAnimation {
            bones: vec![bone("root", None), bone("Arm", Some(0))],
            pose_parameters: vec![PoseParameter {
                name: "move_yaw".to_owned(),
                start: -180.0,
                end: 180.0,
                loop_range: 360.0,
            }],
            animations: vec![Animation {
                name: "run".to_owned(),
                fps: 10.0,
                frames: 3,
                looping: true,
                delta: false,
                tracks: vec![
                    BoneTrack {
                        bone: 1,
                        positions: vec![[0.0, 0.0, 10.0]],
                        rotations: vec![[0.0, 0.0, 0.0, 1.0]; 3],
                    },
                    BoneTrack {
                        bone: 7,
                        positions: vec![[0.0; 3]],
                        rotations: vec![[0.0, 0.0, 0.0, 1.0]],
                    },
                ],
            }],
            sequences: vec![Sequence {
                label: "Run_All".to_owned(),
                activity: "act_run".to_owned(),
                looping: true,
                delta: false,
                blends: vec![0, 0, 0],
                group: [3, 1],
                parameters: [Some(0), None],
                ranges: [(-180.0, 180.0), (0.0, 0.0)],
                fade: (0.2, 0.2),
                events: Vec::new(),
            }],
            ..ModelAnimation::default()
        }
    }

    #[test]
    fn skeletons_convert_rest_poses_and_invert_them() {
        let skeleton = Skeleton::of(&model());
        let up = 10.0 * coords::METERS_PER_UNIT;
        assert!((skeleton.joints[1].rest.translation - Vec3::new(0.0, up, 0.0)).length() < 1e-6);
        let globals = skeleton.rest_globals();
        assert!(
            (globals[1].w_axis.y - 2.0 * up).abs() < 1e-6,
            "parent's offset adds up"
        );
        let inverse = skeleton.inverse_bindposes();
        assert!((inverse[1] * globals[1]).abs_diff_eq(Mat4::IDENTITY, 1e-5));
        assert_eq!(skeleton.find("arm"), Some(1), "by name, any case");
        assert_eq!(skeleton.target(1), target_of("ARM"));
    }

    #[test]
    fn sequences_get_one_shared_clip_per_animation() {
        let model = model();
        let skeleton = Skeleton::of(&model);
        let mut added = Vec::new();
        let sequences = sequence_clips(&skeleton, &model, false, &mut |clip| {
            added.push(clip);
            Handle::default()
        });
        assert_eq!(
            added.len(),
            1,
            "three blends of one animation share its clip"
        );
        let run = &sequences[0];
        assert_eq!(
            (run.label.as_str(), run.activity.as_str()),
            ("run_all", "ACT_RUN")
        );
        assert_eq!(run.clips.len(), 3);
        assert!((run.duration - 0.2).abs() < 1e-6);
        assert_eq!(run.axes[0], Some(("move_yaw".to_owned(), -180.0, 180.0)));
        // The arm's curves; the track for a bone the skeleton lacks is
        // dropped.
        assert_eq!(added[0].curves().len(), 1);
    }

    #[test]
    fn one_dimensional_blends_split_between_neighbours() {
        let model = model();
        let skeleton = Skeleton::of(&model);
        let run = &sequence_clips(&skeleton, &model, false, &mut |_| Handle::default())[0];
        assert_eq!(run.blend_1d(-180.0), [(0, 1.0), (1, 0.0)]);
        assert_eq!(run.blend_1d(0.0), [(1, 1.0), (2, 0.0)]);
        let [(low, a), (high, b)] = run.blend_1d(90.0)[..] else {
            panic!("two blends");
        };
        assert_eq!((low, high), (1, 2));
        assert!((a - 0.5).abs() < 1e-6 && (b - 0.5).abs() < 1e-6);
        assert_eq!(run.blend_1d(500.0), [(1, 0.0), (2, 1.0)], "clamped");
    }
}
