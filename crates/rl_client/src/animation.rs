//! Animated models: skeletons, and sequences played and blended on them.
//!
//! A skeletal model (`rl_assets::skeleton`) is spawned with an entity per
//! joint, at rest, which its meshes are skinned to (`spawn_skeleton`).
//! Its root then gets an [`AnimationPlayer`] over a graph holding every clip
//! of the model ([`Graphs`]) and a [`SequencePlayer`], through which the
//! game says what to play: a sequence by index, a value for its blend axis
//! (a run's direction), a playback rate. The sequence player keeps the
//! sequence's time itself, weighs the clips of its blends, and fades from
//! one sequence to the next, writing all of that into the animation player
//! every frame.
//!
//! Models nothing plays a sequence on (most skeletal props) stay at rest,
//! as they were drawn before animation.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::animation::AnimatedBy;
use bevy::prelude::*;
use rl_assets::model::ModelAnimationAsset;
use rl_assets::{ModelAsset, SequenceClips};

/// Plays sequences on animated models.
pub struct ModelAnimationPlugin;

impl Plugin for ModelAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Graphs>()
            .add_systems(Update, (prepare, drive).chain());
    }
}

/// The root of an animated model: its skeleton and sequences.
#[derive(Component, Debug, Clone)]
pub struct Animated {
    /// The model it draws.
    pub model: Handle<ModelAsset>,
    /// Its animation.
    pub animation: Arc<ModelAnimationAsset>,
}

impl Animated {
    /// The index of the first sequence labelled `label` (any case).
    #[must_use]
    pub fn sequence(&self, label: &str) -> Option<usize> {
        self.animation
            .sequences
            .iter()
            .position(|sequence| sequence.label.eq_ignore_ascii_case(label))
    }

    /// The index of the first sequence of activity `activity` (any case).
    #[must_use]
    pub fn activity(&self, activity: &str) -> Option<usize> {
        self.animation
            .sequences
            .iter()
            .position(|sequence| sequence.activity.eq_ignore_ascii_case(activity))
    }
}

/// A named point on a model's bone (`muzzle`, `eyes`), which follows the
/// bone as it moves: effects start from these.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Attachment(pub String);

/// Spawns `animation`'s joints at rest under `root`, each animated by
/// `root`'s player, and its attachments under their joints; returns the
/// joints in order, for skinning.
pub(crate) fn spawn_skeleton(
    commands: &mut Commands,
    root: Entity,
    animation: &ModelAnimationAsset,
) -> Vec<Entity> {
    let skeleton = &animation.skeleton;
    let mut joints: Vec<Entity> = Vec::with_capacity(skeleton.joints.len());
    for (index, joint) in skeleton.joints.iter().enumerate() {
        let parent = joint
            .parent
            .and_then(|parent| joints.get(parent).copied())
            .unwrap_or(root);
        let entity = commands
            .spawn((
                Name::new(joint.name.clone()),
                joint.rest,
                Visibility::default(),
                skeleton.target(index),
                AnimatedBy(root),
                ChildOf(parent),
            ))
            .id();
        joints.push(entity);
    }
    for (name, joint, offset) in &animation.attachments {
        if let Some(&joint) = joints.get(*joint) {
            commands.spawn((Attachment(name.clone()), *offset, ChildOf(joint)));
        }
    }
    joints
}

/// One model's animation graph: every clip of its sequences as a node.
struct ModelGraph {
    graph: Handle<AnimationGraph>,
    nodes: HashMap<AssetId<AnimationClip>, AnimationNodeIndex>,
}

/// Each animated model's graph, built once per model.
#[derive(Resource, Default)]
pub struct Graphs(HashMap<AssetId<ModelAsset>, ModelGraph>);

/// A sequence playing (or fading out).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Track {
    sequence: usize,
    /// Seconds into it.
    time: f32,
    /// Its share of the pose, rising as it fades in and falling as it fades
    /// out.
    weight: f32,
    /// The value of its blend axis.
    blend: f32,
    /// Playback rate.
    rate: f32,
    /// Holds its last frame even if the sequence loops.
    hold: bool,
}

/// What a model plays: set by the game, carried out every frame.
#[derive(Component, Debug, Clone, Default)]
pub struct SequencePlayer {
    current: Option<Track>,
    fading: Vec<Track>,
    /// Seconds a change of sequence fades over.
    pub fade: f32,
}

impl SequencePlayer {
    /// Plays sequence `sequence` (looping as the sequence does), blended at
    /// `blend` along its axis, at `rate`. The same sequence carries on
    /// where it is; another fades in from its start.
    pub fn play(&mut self, sequence: usize, blend: f32, rate: f32) {
        match &mut self.current {
            Some(track) if track.sequence == sequence => {
                track.blend = blend;
                track.rate = rate;
            }
            _ => self.start(sequence, blend, rate),
        }
    }

    /// Plays sequence `sequence` from its start, even if it is playing
    /// (a gun's firing sequence, each shot).
    pub fn restart(&mut self, sequence: usize, rate: f32) {
        self.start(sequence, 0.0, rate);
    }

    /// Plays sequence `sequence` from its start and stays on its last
    /// frame, whether or not it loops (a grenade drawn back).
    pub fn restart_held(&mut self, sequence: usize, rate: f32) {
        self.start(sequence, 0.0, rate);
        if let Some(track) = &mut self.current {
            track.hold = true;
        }
    }

    /// The sequence playing, and how far into it (seconds).
    #[must_use]
    pub fn current(&self) -> Option<(usize, f32)> {
        self.current.map(|track| (track.sequence, track.time))
    }

    fn start(&mut self, sequence: usize, blend: f32, rate: f32) {
        if let Some(old) = self.current.take() {
            self.fading.push(old);
        }
        let weight = if self.fade > 0.0 && !self.fading.is_empty() {
            0.0
        } else {
            1.0
        };
        self.current = Some(Track {
            sequence,
            time: 0.0,
            weight,
            blend,
            rate,
            hold: false,
        });
    }

    /// Advances time by `dt`: the sequences move on (looping ones wrap,
    /// others hold their last frame), the current one fades in and the old
    /// ones out.
    fn advance(&mut self, dt: f32, sequences: &[SequenceClips]) {
        let step = if self.fade > 0.0 { dt / self.fade } else { 1.0 };
        let tick = |track: &mut Track| {
            let Some(sequence) = sequences.get(track.sequence) else {
                return;
            };
            track.time += dt * track.rate;
            let duration = sequence.duration.max(f32::EPSILON);
            track.time = if sequence.looping && !track.hold {
                track.time.rem_euclid(duration)
            } else {
                track.time.min(duration)
            };
        };
        if let Some(track) = &mut self.current {
            tick(track);
            track.weight = (track.weight + step).min(1.0);
        }
        for track in &mut self.fading {
            tick(track);
            track.weight -= step;
        }
        self.fading.retain(|track| track.weight > 0.0);
    }

    /// Whether the current sequence has played to its end (never, for a
    /// looping one).
    #[must_use]
    pub fn finished(&self, sequences: &[SequenceClips]) -> bool {
        self.ends_within(sequences, 0.0)
    }

    /// Whether the current sequence ends within `margin` seconds (never, for
    /// a looping one): the moment to start what follows, so that its fade
    /// in overlaps the end of this one rather than waiting for it to end
    /// and cutting in.
    #[must_use]
    pub fn ends_within(&self, sequences: &[SequenceClips], margin: f32) -> bool {
        self.current.is_some_and(|track| {
            sequences.get(track.sequence).is_some_and(|sequence| {
                !sequence.looping && track.time + margin * track.rate >= sequence.duration
            })
        })
    }

    /// Each clip's weight and time: every track's blends, by their weights.
    fn clip_weights(&self, sequences: &[SequenceClips]) -> Vec<(AssetId<AnimationClip>, f32, f32)> {
        let mut weights: Vec<(AssetId<AnimationClip>, f32, f32)> = Vec::new();
        for track in self.fading.iter().chain(&self.current) {
            let Some(sequence) = sequences.get(track.sequence) else {
                continue;
            };
            for (blend, share) in sequence.blend_1d(track.blend) {
                let Some(clip) = sequence.clips.get(blend) else {
                    continue;
                };
                let weight = track.weight * share;
                if weight <= 0.0 {
                    continue;
                }
                match weights.iter_mut().find(|(id, ..)| *id == clip.id()) {
                    Some(entry) => {
                        entry.1 += weight;
                        entry.2 = track.time;
                    }
                    None => weights.push((clip.id(), weight, track.time)),
                }
            }
        }
        weights
    }
}

/// Gives each new animated model its player and graph.
fn prepare(
    mut commands: Commands,
    roots: Query<(Entity, &Animated), Without<AnimationPlayer>>,
    mut graphs: ResMut<Graphs>,
    mut assets: ResMut<Assets<AnimationGraph>>,
) {
    for (entity, animated) in &roots {
        let model = graphs.0.entry(animated.model.id()).or_insert_with(|| {
            let mut graph = AnimationGraph::new();
            let mut nodes = HashMap::new();
            for clip in animated
                .animation
                .sequences
                .iter()
                .flat_map(|sequence| &sequence.clips)
            {
                nodes
                    .entry(clip.id())
                    .or_insert_with(|| graph.add_clip(clip.clone(), 1.0, graph.root));
            }
            ModelGraph {
                graph: assets.add(graph),
                nodes,
            }
        });
        // The root may be despawned (a player leaving, a prop going) before
        // this is applied.
        commands.entity(entity).try_insert((
            AnimationPlayer::default(),
            AnimationGraphHandle(model.graph.clone()),
        ));
    }
}

/// How far short of a clip's end the last frame is sampled, seconds.
const CLIP_END_MARGIN: f32 = 1e-3;

/// Carries out every sequence player: advances it and sets its clips'
/// weights and times in the animation player.
fn drive(
    time: Res<Time>,
    graphs: Res<Graphs>,
    clips: Res<Assets<AnimationClip>>,
    mut roots: Query<(&Animated, &mut SequencePlayer, &mut AnimationPlayer)>,
) {
    let dt = time.delta_secs();
    for (animated, mut sequences, mut player) in &mut roots {
        let Some(model) = graphs.0.get(&animated.model.id()) else {
            continue;
        };
        let all = &animated.animation.sequences;
        sequences.advance(dt, all);
        let wanted = sequences.clip_weights(all);
        let playing: Vec<AnimationNodeIndex> =
            player.playing_animations().map(|(&node, _)| node).collect();
        for node in playing {
            let still = wanted
                .iter()
                .any(|(clip, ..)| model.nodes.get(clip) == Some(&node));
            if !still {
                player.stop(node);
            }
        }
        for (clip, weight, at) in wanted {
            if let Some(&node) = model.nodes.get(&clip) {
                // A clip sought to its very end counts as played out and
                // snaps to its first frame: stop just short of it.
                let at = clips.get(clip).map_or(at, |clip| {
                    at.min((clip.duration() - CLIP_END_MARGIN).max(0.0))
                });
                player
                    .play(node)
                    .set_speed(0.0)
                    .seek_to(at)
                    .set_weight(weight);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequences() -> Vec<SequenceClips> {
        let sequence = |looping, clips: usize| SequenceClips {
            label: "s".to_owned(),
            activity: String::new(),
            looping,
            clips: (0..clips).map(|_| Handle::default()).collect(),
            group: [clips, 1],
            axes: [Some(("move_yaw".to_owned(), -180.0, 180.0)), None],
            duration: 1.0,
            events: Vec::new(),
        };
        vec![sequence(true, 1), sequence(false, 1)]
    }

    #[test]
    fn a_held_sequence_stays_on_its_last_frame_even_if_it_loops() {
        let all = sequences();
        let mut player = SequencePlayer::default();
        player.restart_held(0, 1.0);
        player.advance(3.5, &all);
        assert_eq!(player.current(), Some((0, 1.0)));
    }

    #[test]
    fn looping_sequences_wrap_and_others_hold_their_end() {
        let all = sequences();
        let mut player = SequencePlayer::default();
        player.play(0, 0.0, 1.0);
        player.advance(1.25, &all);
        assert!((player.current().expect("playing").1 - 0.25).abs() < 1e-5);
        player.restart(1, 2.0);
        player.advance(0.75, &all);
        assert_eq!(player.current(), Some((1, 1.0)));
        assert!(player.finished(&all));
    }

    #[test]
    fn the_idle_fades_in_over_the_end_of_a_swing_without_a_gap_or_a_cut() {
        let all = sequences();
        let mut player = SequencePlayer {
            fade: 0.2,
            ..SequencePlayer::default()
        };
        player.play(0, 0.0, 1.0);
        // The swing (sequence 1, one second long) runs; at 0.2 s from its end
        // the idle begins.
        player.restart(1, 1.0);
        let mut begun = false;
        let mut totals = Vec::new();
        for _ in 0..80 {
            if !begun && player.ends_within(&all, 0.2) {
                player.play(0, 0.0, 1.0);
                begun = true;
            }
            player.advance(1.0 / 60.0, &all);
            // Every frame's pose is wholly made of some sequence.
            let weights = player.clip_weights(&all);
            totals.push(weights.iter().map(|(_, weight, _)| weight).sum::<f32>());
        }
        assert!(begun, "the idle was started");
        assert!(
            totals.iter().all(|total| (total - 1.0).abs() < 1e-3),
            "{totals:?}"
        );
        assert!(player.fading.is_empty(), "the swing faded out");
        assert_eq!(player.current().map(|(sequence, _)| sequence), Some(0));
    }

    #[test]
    fn a_new_sequence_fades_in_as_the_old_fades_out() {
        let all = sequences();
        let mut player = SequencePlayer {
            fade: 0.5,
            ..SequencePlayer::default()
        };
        player.play(0, 0.0, 1.0);
        assert_eq!(
            player.current.expect("playing").weight,
            1.0,
            "nothing to fade from"
        );
        player.play(1, 0.0, 1.0);
        player.advance(0.25, &all);
        assert!((player.current.expect("new").weight - 0.5).abs() < 1e-5);
        assert!((player.fading[0].weight - 0.5).abs() < 1e-5);
        player.advance(0.5, &all);
        assert!(player.fading.is_empty(), "faded out");
        // Playing the same sequence again carries on.
        let before = player.current().expect("playing").1;
        player.play(1, 0.0, 1.0);
        assert_eq!(player.current().expect("playing").1, before);
    }
}
