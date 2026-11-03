//! Animated models: skeletons, and sequences played and blended on them.
//!
//! A skeletal model (`rl_assets::skeleton`) is spawned with an entity per
//! joint, in the model's idle pose, which its meshes are skinned to
//! (`spawn_skeleton`).
//! Its root then gets an [`AnimationPlayer`] over a graph holding every clip
//! of the model ([`Graphs`]) and a [`SequencePlayer`], through which the
//! game says what to play: a sequence by index, a value for its blend axis
//! (a run's direction), a playback rate. The sequence player keeps the
//! sequence's time itself, weighs the clips of its blends, and fades from
//! one sequence to the next, writing all of that into the animation player
//! every frame.
//!
//! Models nothing plays a sequence on (most skeletal props) stay in that
//! idle pose, as the game draws them.
//!
//! A sequence player can also note the events its sequence passes (a reload's
//! clicks, `AE_CL_PLAYSOUND`) for whoever listens ([`SequencePlayer::listen`]).

use std::collections::HashMap;
use std::sync::Arc;

use bevy::animation::AnimatedBy;
use bevy::prelude::*;
use rl_assets::model::ModelAnimationAsset;
use rl_assets::{ModelAsset, PoseRig, SequenceClips};
use source_formats::anim::SequenceEvent;

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
        self.animation.sequences.iter().position(|sequence| {
            sequence.layer.is_none() && sequence.label.eq_ignore_ascii_case(label)
        })
    }

    /// The index of the first sequence of activity `activity` (any case).
    #[must_use]
    pub fn activity(&self, activity: &str) -> Option<usize> {
        self.animation.sequences.iter().position(|sequence| {
            sequence.layer.is_none() && sequence.activity.eq_ignore_ascii_case(activity)
        })
    }

    /// The index of the layer labelled `label` (any case): a pose laid over
    /// whatever plays, on the joints it moves, by [`SequencePlayer::set_layer`].
    #[must_use]
    pub fn layer(&self, label: &str) -> Option<usize> {
        self.animation.sequences.iter().position(|sequence| {
            sequence.layer.is_some() && sequence.label.eq_ignore_ascii_case(label)
        })
    }
}

/// The joints of a skeletal model, in the model's bone order.
#[derive(Component, Debug, Clone)]
pub struct Joints(pub Vec<Entity>);

/// A model whose pose the game works out itself, joint by joint, from
/// what its sequences lay over each other (a player's weapon hold and
/// aim): no animation player drives it. See [`PoseRig`].
#[derive(Component, Debug, Clone)]
pub struct Posed(pub Arc<PoseRig>);

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
                joint.pose,
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

/// The mask group of the joints a model's layers animate, and of the rest.
const LAYER_JOINTS: u32 = 1;
const OTHER_JOINTS: u32 = 2;

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
    /// The cycle up to which its events have been passed: below zero before
    /// its first frame, so that events at its very start count too.
    passed: f32,
}

/// The cycle a track that has not played yet has passed.
const NOT_STARTED: f32 = -1.0;
/// The most events a player keeps for a listener that has not taken them.
const MAX_HELD_EVENTS: usize = 64;

/// What a model plays: set by the game, carried out every frame.
#[derive(Component, Debug, Clone, Default)]
pub struct SequencePlayer {
    current: Option<Track>,
    fading: Vec<Track>,
    /// Layers laid over the pose: the sequence, its blend value and how
    /// strongly.
    layers: Vec<(usize, f32, f32)>,
    /// Seconds a change of sequence fades over.
    pub fade: f32,
    /// The events passed and not yet taken, as (sequence, index into its
    /// events), if anyone listens.
    events: Option<Vec<(usize, usize)>>,
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

    /// Lays the layer `sequence` over the pose, blended at `blend` along its
    /// axis and `weight` strong (a weapon's prongs, opened by the pose
    /// parameter `active`). Lasts until set again.
    pub fn set_layer(&mut self, sequence: usize, blend: f32, weight: f32) {
        match self.layers.iter_mut().find(|layer| layer.0 == sequence) {
            Some(layer) => *layer = (sequence, blend, weight),
            None => self.layers.push((sequence, blend, weight)),
        }
    }

    /// Starts noting the events the sequence playing passes (not those
    /// fading out), for [`take_events`](Self::take_events).
    pub fn listen(&mut self) {
        self.events.get_or_insert_with(Vec::new);
    }

    /// The events passed since they were last taken, in order: (sequence,
    /// index into its [`events`](SequenceClips::events)).
    pub fn take_events(&mut self) -> Vec<(usize, usize)> {
        self.events.as_mut().map(std::mem::take).unwrap_or_default()
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
            passed: NOT_STARTED,
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
            let before = track.time;
            tick(track);
            track.weight = (track.weight + step).min(1.0);
            if let (Some(events), Some(sequence)) =
                (&mut self.events, sequences.get(track.sequence))
            {
                let duration = sequence.duration.max(f32::EPSILON);
                let cycle = track.time / duration;
                // A looping sequence that came round to its start (once,
                // however many laps a long frame took).
                let wrapped =
                    sequence.looping && !track.hold && before + dt * track.rate >= duration;
                let passed = passed_events(&sequence.events, track.passed, cycle, wrapped);
                events.extend(passed.map(|event| (track.sequence, event)));
                let overflow = events.len().saturating_sub(MAX_HELD_EVENTS);
                events.drain(..overflow);
                track.passed = cycle;
            }
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
        let layers = self
            .layers
            .iter()
            .map(|&(sequence, blend, weight)| (sequence, blend, weight, 0.0));
        let tracks = self
            .fading
            .iter()
            .chain(&self.current)
            .map(|track| (track.sequence, track.blend, track.weight, track.time));
        for (index, blend_value, track_weight, time) in tracks.chain(layers) {
            let Some(sequence) = sequences.get(index) else {
                continue;
            };
            for (blend, share) in sequence.blend_1d(blend_value) {
                let Some(clip) = sequence.clips.get(blend) else {
                    continue;
                };
                let weight = track_weight * share;
                if weight <= 0.0 {
                    continue;
                }
                match weights.iter_mut().find(|(id, ..)| *id == clip.id()) {
                    Some(entry) => {
                        entry.1 += weight;
                        entry.2 = time;
                    }
                    None => weights.push((clip.id(), weight, time)),
                }
            }
        }
        weights
    }
}

/// Animated models nothing plays yet, and that the game does not pose itself.
type Unplayed = (Without<AnimationPlayer>, Without<Posed>);

/// Gives each new animated model its player and graph.
fn prepare(
    mut commands: Commands,
    roots: Query<(Entity, &Animated), Unplayed>,
    mut graphs: ResMut<Graphs>,
    mut assets: ResMut<Assets<AnimationGraph>>,
) {
    for (entity, animated) in &roots {
        let model = graphs.0.entry(animated.model.id()).or_insert_with(|| {
            let mut graph = AnimationGraph::new();
            let mut nodes = HashMap::new();
            // A layer's joints are animated by its clips alone, and by
            // nothing else: joints are put in a mask group, and the clips
            // that must not touch them mask it out.
            let sequences = &animated.animation.sequences;
            let layered: Vec<usize> = sequences
                .iter()
                .filter_map(|sequence| sequence.layer.as_deref())
                .flatten()
                .copied()
                .collect();
            let skeleton = &animated.animation.skeleton;
            if !layered.is_empty() {
                for joint in 0..skeleton.joints.len() {
                    let group = if layered.contains(&joint) {
                        LAYER_JOINTS
                    } else {
                        OTHER_JOINTS
                    };
                    graph.add_target_to_mask_group(skeleton.target(joint), group);
                }
            }
            for sequence in sequences {
                let mask = if sequence.layer.is_some() {
                    1 << OTHER_JOINTS
                } else if layered.is_empty() {
                    0
                } else {
                    1 << LAYER_JOINTS
                };
                for clip in &sequence.clips {
                    nodes.entry(clip.id()).or_insert_with(|| {
                        graph.add_clip_with_mask(clip.clone(), mask, 1.0, graph.root)
                    });
                }
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

/// The indices of the events a sequence passes going from cycle `from`
/// (what it had passed already) to cycle `to`: those after `from` up to and
/// including `to`, or, when it came round to its start in between
/// (`wrapped`), those after `from` and those up to `to`.
fn passed_events(
    events: &[SequenceEvent],
    from: f32,
    to: f32,
    wrapped: bool,
) -> impl Iterator<Item = usize> + '_ {
    events
        .iter()
        .enumerate()
        .filter(move |(_, event)| {
            if wrapped {
                event.cycle > from || event.cycle <= to
            } else {
                event.cycle > from && event.cycle <= to
            }
        })
        .map(|(index, _)| index)
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
            layer: None,
            looping,
            clips: (0..clips).map(|_| Handle::default()).collect(),
            group: [clips, 1],
            axes: [Some(("move_yaw".to_owned(), -180.0, 180.0)), None],
            duration: 1.0,
            events: Vec::new(),
        };
        vec![sequence(true, 1), sequence(false, 1)]
    }

    fn sound_at(cycle: f32) -> SequenceEvent {
        SequenceEvent {
            cycle,
            event: 0,
            name: "AE_CL_PLAYSOUND".to_owned(),
            options: "Weapon_AR2.Reload_Rotate".to_owned(),
        }
    }

    #[test]
    fn a_listener_hears_each_event_once_as_the_sequence_passes_it() {
        let mut all = sequences();
        all[1].events = vec![sound_at(0.0), sound_at(0.4), sound_at(1.0)];
        let mut player = SequencePlayer::default();
        player.listen();
        player.restart(1, 1.0);
        player.advance(0.1, &all);
        assert_eq!(player.take_events(), [(1, 0)], "the one at its start");
        player.advance(0.2, &all);
        assert!(player.take_events().is_empty());
        player.advance(0.2, &all);
        assert_eq!(player.take_events(), [(1, 1)]);
        player.advance(1.0, &all);
        assert_eq!(player.take_events(), [(1, 2)], "the one at its end");
        player.advance(1.0, &all);
        assert!(player.take_events().is_empty(), "held on its last frame");
        // Played again, it is heard again.
        player.restart(1, 1.0);
        player.advance(0.5, &all);
        assert_eq!(player.take_events(), [(1, 0), (1, 1)]);
    }

    #[test]
    fn a_loop_is_heard_every_time_round() {
        let mut all = sequences();
        all[0].events = vec![sound_at(0.5)];
        let mut player = SequencePlayer::default();
        player.listen();
        player.play(0, 0.0, 1.0);
        player.advance(0.6, &all);
        assert_eq!(player.take_events(), [(0, 0)]);
        player.advance(0.6, &all);
        assert!(player.take_events().is_empty(), "round to 0.2 only");
        player.advance(0.4, &all);
        assert_eq!(player.take_events(), [(0, 0)]);
        // A whole lap in one go still passes it once.
        player.advance(1.0, &all);
        assert_eq!(player.take_events(), [(0, 0)]);
    }

    #[test]
    fn nothing_is_kept_when_no_one_listens() {
        let mut all = sequences();
        all[1].events = vec![sound_at(0.0)];
        let mut player = SequencePlayer::default();
        player.restart(1, 1.0);
        player.advance(0.5, &all);
        assert!(player.take_events().is_empty());
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
