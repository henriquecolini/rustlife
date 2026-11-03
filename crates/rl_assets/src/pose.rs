//! Poses of skeletal models, evaluated on the CPU as the engine does.
//!
//! Bevy's animation graph blends whole clips, which is all a prop or a
//! view model needs. A player's model needs the engine's own way of
//! building a pose (`CBoneSetup::AccumulatePose`), because its animations
//! lean on three things the graph has no place for:
//!
//! - **Bone weights.** Every sequence carries a weight for each bone: a
//!   reload moves the arms and hardly the hips, an aim layer leaves the
//!   legs alone.
//! - **Deltas.** An aim matrix or a firing gesture is not a pose but an
//!   offset: each bone's rotation is turned by it, scaled by the weight,
//!   after the rotation it already has (`STUDIO_POST`) or before.
//! - **Autolayers.** A weapon's idle and run sequences name the aim matrix
//!   that goes with them, played along at the same time, so a player
//!   points the weapon where they look just by setting the aim pose
//!   parameters.
//!
//! A [`PoseRig`] is built once per model from its decoded sequences (and
//! those of the models it includes). [`PoseRig::evaluate`] then turns what
//! is playing — the main sequences (a new one fading in over the old),
//! overlays such as gestures, and the pose parameters — into a transform
//! for each bone, relative to its parent, in Bevy space.
//!
//! Poses are blended in this order, as the engine does: the main
//! sequences first, each blending over the pose so far by its weight; each
//! sequence's autolayers right after it; then the overlays, in order.

use std::sync::Arc;

use bevy_math::{Quat, Vec2, Vec3};
use bevy_transform::components::Transform;
use source_formats::anim::{Animation, AutoLayer, ModelAnimation, Sequence};

use crate::coords;
use crate::skeleton::{Skeleton, local_transform};

/// How deep autolayers may nest (a guard against a model whose sequences
/// name each other).
const MAX_LAYER_DEPTH: usize = 4;

/// `STUDIO_AL_SPLINE`: the layer's weight eases in and out.
const LAYER_SPLINE: i32 = 0x20;
/// `STUDIO_AL_XFADE`: the layer cross-fades with the sequence it lays over.
const LAYER_CROSS_FADE: i32 = 0x40;
/// `STUDIO_AL_NOBLEND`: the layer's weight ignores its sequence's.
const LAYER_NO_BLEND: i32 = 0x80;

/// A sequence playing: which, how far along it, and how strongly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayTrack {
    /// The sequence, by index into [`PoseRig::sequence`].
    pub sequence: usize,
    /// How far along it is, `0..=1`.
    pub cycle: f32,
    /// Its share of the pose, `0..=1`.
    pub weight: f32,
}

/// One bone's keys in a clip, in Bevy space.
#[derive(Debug, Clone)]
struct BoneKeys {
    positions: Vec<Vec3>,
    rotations: Vec<Quat>,
}

impl BoneKeys {
    /// The bone's transform `frame` frames in (fractional), the keys held
    /// still if there is only one.
    fn at(&self, frame: f32) -> (Vec3, Quat) {
        (
            sample(&self.positions, frame, Vec3::lerp),
            sample(&self.rotations, frame, Quat::slerp),
        )
    }
}

/// The value `frame` frames into `keys`: a single key holds still, more are
/// interpolated and held at the last.
fn sample<T: Copy>(keys: &[T], frame: f32, blend: impl Fn(T, T, f32) -> T) -> T {
    let last = keys.len() - 1;
    if last == 0 {
        return keys[0];
    }
    let frame = frame.clamp(0.0, last as f32);
    let low = frame.floor() as usize;
    let high = (low + 1).min(last);
    blend(keys[low], keys[high], frame - low as f32)
}

/// One animation, in Bevy space.
#[derive(Debug)]
struct RigClip {
    frames: usize,
    /// Per bone of the rig; `None` where the clip's model has no such
    /// bone, which then stays as it is.
    bones: Vec<Option<BoneKeys>>,
    /// How far it carries the model, Source space (`+X` forward).
    movement: [f32; 3],
    /// Its length, seconds.
    duration: f32,
}

/// A pose parameter axis of a sequence's blend grid.
#[derive(Debug, Clone, Copy)]
struct Axis {
    /// The parameter, by index into [`PoseRig::parameter`].
    parameter: usize,
    /// Its value at the grid's first and last column or row.
    start: f32,
    end: f32,
}

impl Axis {
    /// Where `value` falls along the axis, `0..=1`.
    fn fraction(&self, parameters: &[f32]) -> f32 {
        let value = parameters.get(self.parameter).copied().unwrap_or(0.0);
        let span = self.end - self.start;
        if span.abs() < f32::EPSILON {
            0.0
        } else {
            ((value - self.start) / span).clamp(0.0, 1.0)
        }
    }
}

/// A sequence another plays along with itself.
#[derive(Debug, Clone, Copy)]
struct Layer {
    /// The layer's sequence, by index into the rig's sequences.
    sequence: usize,
    flags: i32,
    /// Where along the cycle it fades in, peaks, fades out and ends.
    cycle: [f32; 4],
}

impl Layer {
    /// The layer's weight when its sequence, at `weight`, is `cycle` along;
    /// `None` when it is not playing there.
    fn weight(&self, cycle: f32, weight: f32) -> Option<f32> {
        let [start, peak, tail, end] = self.cycle;
        if (start - end).abs() < f32::EPSILON {
            return Some(weight);
        }
        if cycle < start || cycle >= end {
            return None;
        }
        let mut share = 1.0;
        if cycle < peak && (peak - start).abs() > f32::EPSILON {
            share = (cycle - start) / (peak - start);
        } else if cycle > tail && (end - tail).abs() > f32::EPSILON {
            share = (end - cycle) / (end - tail);
        }
        if self.flags & LAYER_SPLINE != 0 {
            share = share * share * (3.0 - 2.0 * share);
        }
        Some(if self.flags & LAYER_CROSS_FADE != 0 && cycle > tail {
            share * weight / (1.0 - weight + share * weight)
        } else if self.flags & LAYER_NO_BLEND != 0 {
            share
        } else {
            weight * share
        })
    }
}

/// A sequence of a rig.
#[derive(Debug)]
pub struct RigSequence {
    /// Its label, lower case.
    pub label: String,
    /// Its activity, upper case; possibly empty.
    pub activity: String,
    /// Whether it loops.
    pub looping: bool,
    /// Seconds over which it fades in and out.
    pub fade: (f32, f32),
    delta: bool,
    post: bool,
    /// Per bone; empty: every bone fully.
    weights: Vec<f32>,
    layers: Vec<Layer>,
    axes: [Option<Axis>; 2],
    group: [usize; 2],
    clips: Vec<Arc<RigClip>>,
}

impl RigSequence {
    /// Its length in seconds (that of its first blend).
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.clips.first().map_or(0.0, |clip| clip.duration)
    }

    /// Whether it is a delta, added to the pose rather than blended in.
    #[must_use]
    pub fn is_delta(&self) -> bool {
        self.delta
    }

    fn weight(&self, bone: usize) -> f32 {
        self.weights.get(bone).copied().unwrap_or(1.0)
    }

    /// The blends to mix for these pose parameters, with their shares
    /// (which sum to 1): the grid cells around the point the parameters
    /// pick, bilinearly.
    fn blends(&self, parameters: &[f32]) -> Vec<(usize, f32)> {
        let fraction = |axis: usize| self.axes[axis].map_or(0.0, |axis| axis.fraction(parameters));
        let (columns, rows) = (self.group[0].max(1), self.group[1].max(1));
        let position = |fraction: f32, size: usize| -> (usize, f32) {
            if size < 2 {
                return (0, 0.0);
            }
            let at = fraction * (size - 1) as f32;
            let low = (at.floor() as usize).min(size - 2);
            (low, at - low as f32)
        };
        let (column, across) = position(fraction(0), columns);
        let (row, down) = position(fraction(1), rows);
        let mut blends = Vec::with_capacity(4);
        for (dx, dy, share) in [
            (0, 0, (1.0 - across) * (1.0 - down)),
            (1, 0, across * (1.0 - down)),
            (0, 1, (1.0 - across) * down),
            (1, 1, across * down),
        ] {
            let (x, y) = (column + dx, row + dy);
            if share > 0.0 && x < columns && y < rows {
                blends.push((x + y * columns, share));
            }
        }
        if blends.is_empty() {
            blends.push((0, 1.0));
        }
        blends
    }

    /// How fast the sequence carries the model over the ground, units a
    /// second, for these pose parameters: what the engine calls its ground
    /// speed, which a run's playback rate is measured against.
    #[must_use]
    pub fn ground_speed(&self, parameters: &[f32]) -> f32 {
        let moved = self
            .blends(parameters)
            .into_iter()
            .filter_map(|(blend, share)| {
                let clip = self.clips.get(blend)?;
                Some(Vec2::new(clip.movement[0], clip.movement[1]) * share)
            })
            .sum::<Vec2>();
        let duration = self.duration();
        if duration > 0.0 {
            moved.length() / duration
        } else {
            0.0
        }
    }
}

/// A model's animation, ready to evaluate.
#[derive(Debug)]
pub struct PoseRig {
    rest: Vec<Transform>,
    /// Pose parameters: name (lower case) and the range of values the
    /// model gives it.
    parameters: Vec<(String, f32, f32)>,
    sequences: Vec<RigSequence>,
}

/// The bones' transforms while a pose is built.
struct Pose {
    positions: Vec<Vec3>,
    rotations: Vec<Quat>,
}

impl PoseRig {
    /// The rig of `skeleton` with the sequences of `models`: the model
    /// itself first, then those it includes, each in the order the engine
    /// merges them. A model's sequences take the bones of its own file by
    /// name, as the engine matches them to the model's.
    #[must_use]
    pub fn build(skeleton: &Skeleton, models: &[ModelAnimation]) -> Self {
        let mut rig = Self {
            rest: skeleton.joints.iter().map(|joint| joint.rest).collect(),
            parameters: Vec::new(),
            sequences: Vec::new(),
        };
        for model in models {
            rig.add(skeleton, model);
        }
        rig
    }

    fn add(&mut self, skeleton: &Skeleton, model: &ModelAnimation) {
        let joints: Vec<Option<usize>> = model
            .bones
            .iter()
            .map(|bone| skeleton.find(&bone.name))
            .collect();
        let clips: Vec<Arc<RigClip>> = model
            .animations
            .iter()
            .map(|animation| Arc::new(self.clip(&joints, animation)))
            .collect();
        let offset = self.sequences.len();
        for sequence in &model.sequences {
            let rig_sequence = self.sequence_of(sequence, model, &joints, &clips, offset);
            self.sequences.push(rig_sequence);
        }
    }

    fn sequence_of(
        &mut self,
        sequence: &Sequence,
        model: &ModelAnimation,
        joints: &[Option<usize>],
        clips: &[Arc<RigClip>],
        offset: usize,
    ) -> RigSequence {
        let mut axes = [None; 2];
        for (axis, slot) in axes.iter_mut().enumerate() {
            let Some(parameter) =
                sequence.parameters[axis].and_then(|index| model.pose_parameters.get(index))
            else {
                continue;
            };
            let (start, end) = sequence.ranges[axis];
            *slot = Some(Axis {
                parameter: self.parameter_index(&parameter.name, parameter.start, parameter.end),
                start,
                end,
            });
        }
        // Weights are by bone of the sequence's file; the rig's by joint.
        let weights = if sequence.weights.is_empty() {
            Vec::new()
        } else {
            let mut weights = vec![0.0; self.rest.len()];
            for (bone, &weight) in sequence.weights.iter().enumerate() {
                if let Some(joint) = joints.get(bone).copied().flatten() {
                    weights[joint] = weight;
                }
            }
            weights
        };
        RigSequence {
            label: sequence.label.to_ascii_lowercase(),
            activity: sequence.activity.to_ascii_uppercase(),
            looping: sequence.looping,
            fade: sequence.fade,
            delta: sequence.delta,
            post: sequence.post,
            weights,
            layers: sequence
                .layers
                .iter()
                .map(
                    |&AutoLayer {
                         sequence,
                         flags,
                         cycle,
                     }| Layer {
                        sequence: offset + sequence,
                        flags,
                        cycle,
                    },
                )
                .collect(),
            axes,
            group: sequence.group,
            clips: sequence
                .blends
                .iter()
                .filter_map(|&blend| clips.get(blend).cloned())
                .collect(),
        }
    }

    fn parameter_index(&mut self, name: &str, start: f32, end: f32) -> usize {
        let name = name.to_ascii_lowercase();
        self.parameters
            .iter()
            .position(|(known, ..)| *known == name)
            .unwrap_or_else(|| {
                self.parameters.push((name, start, end));
                self.parameters.len() - 1
            })
    }

    /// An animation in Bevy space, its tracks moved to the rig's joints.
    fn clip(&self, joints: &[Option<usize>], animation: &Animation) -> RigClip {
        let mut bones = vec![None; self.rest.len()];
        for track in &animation.tracks {
            let Some(joint) = joints.get(track.bone).copied().flatten() else {
                continue;
            };
            bones[joint] = Some(BoneKeys {
                positions: track
                    .positions
                    .iter()
                    .map(|&p| coords::position(p))
                    .collect(),
                rotations: track
                    .rotations
                    .iter()
                    .map(|&r| local_transform([0.0; 3], r).rotation)
                    .collect(),
            });
        }
        RigClip {
            frames: animation.frames.max(2),
            bones,
            movement: animation.movement,
            duration: (animation.frames.max(2) - 1) as f32 / animation.fps.max(1.0),
        }
    }

    /// The index of the pose parameter named `name` (any case), the
    /// position of its value in the slice [`PoseRig::evaluate`] takes.
    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<usize> {
        self.parameters
            .iter()
            .position(|(known, ..)| known.eq_ignore_ascii_case(name))
    }

    /// The values pose parameter `index` ranges over, as its model gives
    /// them: what a head's turn is held to.
    #[must_use]
    pub fn parameter_range(&self, index: usize) -> Option<(f32, f32)> {
        self.parameters
            .get(index)
            .map(|&(_, start, end)| (start, end))
    }

    /// How many pose parameters sequences use.
    #[must_use]
    pub fn parameter_count(&self) -> usize {
        self.parameters.len()
    }

    /// The sequence labelled `label` (any case): the first that names an
    /// activity, as the engine picks sequences by activity, else the first
    /// of that label (hidden helper sequences have none).
    #[must_use]
    pub fn find(&self, label: &str) -> Option<usize> {
        let mut labelled = self
            .sequences
            .iter()
            .enumerate()
            .filter(|(_, sequence)| sequence.label.eq_ignore_ascii_case(label));
        let first = labelled.next()?;
        std::iter::once(first)
            .chain(labelled)
            .find(|(_, sequence)| !sequence.activity.is_empty())
            .map_or(Some(first.0), |(index, _)| Some(index))
    }

    /// The sequence at `index`.
    #[must_use]
    pub fn sequence(&self, index: usize) -> Option<&RigSequence> {
        self.sequences.get(index)
    }

    /// The pose of a model playing `main` (the first as it is, each of the
    /// others blended over what came before by its weight: a sequence
    /// fading in over the one it replaces) with `overlays` laid on top, at
    /// pose parameter values `parameters`: each bone's transform relative
    /// to its parent.
    #[must_use]
    pub fn evaluate(
        &self,
        parameters: &[f32],
        main: &[PlayTrack],
        overlays: &[PlayTrack],
    ) -> Vec<Transform> {
        let mut pose = Pose {
            positions: self.rest.iter().map(|rest| rest.translation).collect(),
            rotations: self.rest.iter().map(|rest| rest.rotation).collect(),
        };
        for (index, track) in main.iter().enumerate() {
            let weight = if index == 0 { 1.0 } else { track.weight };
            self.accumulate(&mut pose, parameters, *track, weight, 0);
        }
        for track in overlays {
            self.accumulate(&mut pose, parameters, *track, track.weight, 0);
        }
        pose.positions
            .into_iter()
            .zip(pose.rotations)
            .map(|(translation, rotation)| Transform {
                translation,
                rotation: rotation.normalize(),
                scale: Vec3::ONE,
            })
            .collect()
    }

    /// Adds sequence `track.sequence` at `track.cycle` to `pose` with
    /// `weight`, then its autolayers.
    fn accumulate(
        &self,
        pose: &mut Pose,
        parameters: &[f32],
        track: PlayTrack,
        weight: f32,
        depth: usize,
    ) {
        let Some(sequence) = self.sequences.get(track.sequence) else {
            return;
        };
        if weight <= 0.0 || depth > MAX_LAYER_DEPTH {
            return;
        }
        let sampled = self.sample(sequence, parameters, track.cycle);
        for (bone, &(position, rotation)) in sampled.iter().enumerate() {
            let share = weight * sequence.weight(bone);
            if share <= 0.0 {
                continue;
            }
            if sequence.delta {
                let scaled = Quat::IDENTITY.slerp(rotation, share);
                pose.rotations[bone] = if sequence.post {
                    pose.rotations[bone] * scaled
                } else {
                    scaled * pose.rotations[bone]
                };
                pose.positions[bone] += position * share;
            } else {
                pose.rotations[bone] = pose.rotations[bone].slerp(rotation, share);
                pose.positions[bone] = pose.positions[bone].lerp(position, share);
            }
        }
        for layer in &sequence.layers {
            if let Some(weight) = layer.weight(track.cycle, weight) {
                let layer_track = PlayTrack {
                    sequence: layer.sequence,
                    cycle: track.cycle,
                    weight,
                };
                self.accumulate(pose, parameters, layer_track, weight, depth + 1);
            }
        }
    }

    /// Every bone's transform in `sequence` at `cycle`, its blends mixed by
    /// the pose parameters. A bone no clip has, in a delta, is unmoved;
    /// otherwise it rests.
    fn sample(&self, sequence: &RigSequence, parameters: &[f32], cycle: f32) -> Vec<(Vec3, Quat)> {
        let unmoved = |bone: usize| {
            if sequence.delta {
                (Vec3::ZERO, Quat::IDENTITY)
            } else {
                (self.rest[bone].translation, self.rest[bone].rotation)
            }
        };
        let mut mixed: Vec<(Vec3, Quat)> = (0..self.rest.len()).map(unmoved).collect();
        let mut total = 0.0;
        let mut first = true;
        for (blend, share) in sequence.blends(parameters) {
            let Some(clip) = sequence.clips.get(blend) else {
                continue;
            };
            total += share;
            let blend_in = share / total;
            let frame = cycle.clamp(0.0, 1.0) * (clip.frames - 1) as f32;
            for (bone, entry) in mixed.iter_mut().enumerate() {
                let (position, rotation) = clip.bones[bone]
                    .as_ref()
                    .map_or_else(|| unmoved(bone), |keys| keys.at(frame));
                *entry = if first {
                    (position, rotation)
                } else {
                    (
                        entry.0.lerp(position, blend_in),
                        entry.1.slerp(rotation, blend_in),
                    )
                };
            }
            first = false;
        }
        mixed
    }
}

#[cfg(test)]
mod tests;
