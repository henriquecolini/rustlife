//! The mixer: every playing sound ("voice"), placed relative to the
//! listener, mixed to stereo with the room effect.
//!
//! Each block of [`BLOCK`] frames, a voice's level in each ear is worked
//! out afresh, as Source's `SND_Spatialize` does for a channel: the gain
//! of its distance ([`falloff`]), its direction
//! ([`pan`]), and the share that goes to the room
//! ([`room_mix`]), which grows from the preset's `mix_min` next to the
//! listener to its `mix_max` at 1440 units. Levels slide from one block's
//! to the next so nothing clicks. The room's share of every voice is summed
//! and run through the [`RoomEffect`]; the rest is heard directly.
//!
//! The local player's own sounds are placed as Source places them: 12
//! units ahead of the listener.

use glam::Vec3;
use source_formats::dsp_presets::PresetDef;
use source_formats::soundscript::WaveFlags;

use crate::SAMPLE_RATE;
use crate::dsp::RoomEffect;
use crate::falloff::{self, OBSCURED_DB};
use crate::pan::{self, Frame, Speakers};
use crate::pcm::Pcm;

/// Frames mixed per block (5.8 ms).
pub const BLOCK: usize = 256;
/// Distance (units) at which sounds of the listener's own are heard.
pub const OWN_DISTANCE: f32 = 12.0;
/// `dsp_dist_max`: the distance at which a sound gets the room's largest
/// share.
pub const ROOM_DISTANCE: f32 = 1440.0;
/// Distance-variant (`^`) waves are all "near" up to here...
const NEAR: f32 = 240.0;
/// ...and all "far" from here.
const FAR: f32 = 1320.0;
/// Seconds a voice takes to go behind or come out from a wall.
const OBSCURE_SECS: f32 = 0.25;

/// Names a voice of a [`Mixer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VoiceId(u64);

/// Where a voice is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Place {
    /// With the listener: the local player's own sounds, the menus'.
    Listener,
    /// At a point, Source units.
    World(Vec3),
}

/// What a voice does at its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Looping {
    /// Stops.
    Once,
    /// Starts over (at the sound's loop point, if it has one).
    Always,
    /// Starts over if the sound has a loop point, else stops.
    AsRecorded,
}

/// How a voice plays; change it at any time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceParams {
    /// Volume, `0..=1` (any volume settings applied).
    pub volume: f32,
    /// Playback speed (1: as recorded).
    pub speed: f32,
    /// Where it is.
    pub place: Place,
    /// Sound level, decibels (0: heard everywhere at full volume).
    pub soundlevel: f32,
    /// Its wave's mixer hints.
    pub flags: WaveFlags,
    /// Whether the room effect applies (not to the menus' sounds).
    pub room: bool,
    /// Whether a wall stands between it and the listener.
    pub obscured: bool,
    /// What it does at its end.
    pub looping: Looping,
}

impl Default for VoiceParams {
    fn default() -> Self {
        Self {
            volume: 1.0,
            speed: 1.0,
            place: Place::Listener,
            soundlevel: 0.0,
            flags: WaveFlags::default(),
            room: true,
            obscured: false,
            looping: Looping::Once,
        }
    }
}

/// Where the listener is: Source units and axes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Listener {
    /// Its position.
    pub position: Vec3,
    /// Which way it faces.
    pub frame: Frame,
}

/// A voice's levels for one block: (left, right) heard directly and sent
/// to the room, and how "far" a distance-variant wave plays.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Levels {
    dry: [f32; 2],
    wet: [f32; 2],
    far: f32,
}

#[derive(Debug, Clone)]
struct Voice {
    id: VoiceId,
    pcm: Pcm,
    params: VoiceParams,
    /// Frame being played, with the fraction between it and the next.
    cursor: f64,
    /// The levels the last block ended at; `None` before the first.
    levels: Option<Levels>,
    /// How much of it gets past walls, sliding towards 1 or the obscured
    /// level.
    clearance: f32,
}

/// Every playing voice, the listener and the room effect.
#[derive(Debug, Default)]
pub struct Mixer {
    voices: Vec<Voice>,
    next_id: u64,
    listener: Listener,
    speakers: Speakers,
    room: RoomEffect,
    finished: Vec<VoiceId>,
    dry: Vec<[f32; 2]>,
    wet: Vec<[f32; 2]>,
}

impl Mixer {
    /// A mixer with nothing playing and no room effect.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts `pcm` playing as `params` say.
    pub fn play(&mut self, pcm: Pcm, params: VoiceParams) -> VoiceId {
        let id = VoiceId(self.next_id);
        self.next_id += 1;
        self.voices.push(Voice {
            id,
            pcm,
            clearance: if params.obscured {
                obscured_level()
            } else {
                1.0
            },
            params,
            cursor: 0.0,
            levels: None,
        });
        id
    }

    /// How voice `id` plays, to change; `None` once it has stopped.
    pub fn params_mut(&mut self, id: VoiceId) -> Option<&mut VoiceParams> {
        self.voices
            .iter_mut()
            .find(|voice| voice.id == id)
            .map(|voice| &mut voice.params)
    }

    /// Stops voice `id`.
    pub fn stop(&mut self, id: VoiceId) {
        self.voices.retain(|voice| voice.id != id);
    }

    /// Stops every voice.
    pub fn stop_all(&mut self) {
        self.voices.clear();
    }

    /// Whether voice `id` is still playing.
    #[must_use]
    pub fn is_playing(&self, id: VoiceId) -> bool {
        self.voices.iter().any(|voice| voice.id == id)
    }

    /// How many voices are playing.
    #[must_use]
    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }

    /// The voices that reached their end since the last call.
    pub fn take_finished(&mut self) -> Vec<VoiceId> {
        std::mem::take(&mut self.finished)
    }

    /// Moves the listener.
    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = listener;
    }

    /// Sets how the player listens.
    pub fn set_speakers(&mut self, speakers: Speakers) {
        self.speakers = speakers;
    }

    /// Switches the room effect (`None`: none), cross-fading.
    pub fn set_room(&mut self, preset: Option<PresetDef>) {
        self.room.set(preset);
    }

    /// The room effect's preset, if any.
    #[must_use]
    pub fn room(&self) -> Option<&PresetDef> {
        self.room.current()
    }

    /// Mixes the next `out.len() / 2` frames into `out` (interleaved
    /// left, right) at [`SAMPLE_RATE`].
    pub fn render(&mut self, out: &mut [f32]) {
        for chunk in out.chunks_mut(BLOCK * 2) {
            self.render_block(chunk);
        }
    }

    fn render_block(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        self.dry.clear();
        self.dry.resize(frames, [0.0; 2]);
        self.wet.clear();
        self.wet.resize(frames, [0.0; 2]);
        let room = self.room.current().map(RoomMix::of);
        let (listener, speakers) = (self.listener, self.speakers);
        let step = frames as f32 / (OBSCURE_SECS * SAMPLE_RATE as f32);
        for voice in &mut self.voices {
            let target = if voice.params.obscured {
                obscured_level()
            } else {
                1.0
            };
            voice.clearance += (target - voice.clearance).clamp(-step, step);
            let levels = voice.levels(&listener, speakers, room.as_ref());
            if voice.mix(levels, &mut self.dry, &mut self.wet) {
                self.finished.push(voice.id);
            }
        }
        let finished = &self.finished;
        self.voices.retain(|voice| !finished.contains(&voice.id));
        self.room.process(&mut self.wet);
        for ((out, dry), wet) in out
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(&self.dry)
            .zip(&self.wet)
        {
            out[0] = (dry[0] + wet[0]).clamp(-1.0, 1.0);
            out[1] = (dry[1] + wet[1]).clamp(-1.0, 1.0);
        }
    }
}

/// Level of a voice behind a wall.
fn obscured_level() -> f32 {
    falloff::db_to_gain(OBSCURED_DB)
}

/// How the room preset shares sounds out.
#[derive(Debug, Clone, Copy)]
struct RoomMix {
    min: f32,
    max: f32,
    db_min: f32,
    db_mixdrop: f32,
}

impl RoomMix {
    fn of(preset: &PresetDef) -> Self {
        Self {
            min: preset.mix_min,
            max: preset.mix_max,
            db_min: preset.db_min,
            db_mixdrop: preset.db_mixdrop,
        }
    }
}

/// The share of a sound of `soundlevel` dB at `distance` that goes to the
/// room (`SND_GetDspMix`): from `mix_min` at the listener to `mix_max` at
/// [`ROOM_DISTANCE`], less for quiet sounds.
#[must_use]
pub fn room_mix(preset: &PresetDef, distance: f32, soundlevel: f32) -> f32 {
    RoomMix::of(preset).at(distance, soundlevel)
}

impl RoomMix {
    fn at(self, distance: f32, soundlevel: f32) -> f32 {
        let t = distance.clamp(0.0, ROOM_DISTANCE) / ROOM_DISTANCE;
        let (mut min, mut max) = (self.min, self.max);
        if soundlevel <= self.db_min {
            min *= self.db_mixdrop;
            max *= self.db_mixdrop;
        }
        (min + (max - min) * t).clamp(0.0, 1.0)
    }
}

impl Voice {
    /// This block's levels.
    fn levels(&self, listener: &Listener, speakers: Speakers, room: Option<&RoomMix>) -> Levels {
        let params = &self.params;
        let (direction, distance) = match params.place {
            Place::Listener => (listener.frame.forward * OWN_DISTANCE, OWN_DISTANCE),
            Place::World(at) => {
                let direction = at - listener.position;
                (direction, direction.length())
            }
        };
        let multiplier = falloff::distance_multiplier(params.soundlevel);
        let gain = falloff::gain(distance, multiplier) * params.volume.max(0.0) * self.clearance;
        let mono = if params.flags.contains(WaveFlags::OMNI) {
            1.0
        } else {
            0.0
        };
        let pan = pan::stereo(&listener.frame, direction, mono, speakers);
        let dry_only = !params.room
            || params.flags.contains(WaveFlags::DRY)
            || params.flags.contains(WaveFlags::DIRECTIONAL_FAR);
        let mix = match room {
            Some(room) if !dry_only => room.at(distance, falloff::soundlevel(multiplier)),
            _ => 0.0,
        };
        Levels {
            dry: pan.map(|ear| ear * gain * (1.0 - mix)),
            wet: pan.map(|ear| ear * gain * mix),
            far: ((distance.clamp(NEAR, FAR) - NEAR) / (FAR - NEAR)).clamp(0.0, 1.0),
        }
    }

    /// Mixes one block into the buses, sliding from the last block's
    /// levels to `levels`. Returns whether the voice reached its end.
    fn mix(&mut self, levels: Levels, dry: &mut [[f32; 2]], wet: &mut [[f32; 2]]) -> bool {
        let from = self.levels.unwrap_or(levels);
        self.levels = Some(levels);
        let frames = self.pcm.frames();
        if frames == 0 {
            return true;
        }
        let stereo = self.pcm.channels() > 1;
        let flags = self.params.flags;
        let downmix = stereo
            && (flags.contains(WaveFlags::SPATIAL_STEREO)
                || flags.contains(WaveFlags::DISTANCE_VARIANT));
        let step = f64::from(self.params.speed.clamp(0.01, 8.0)) * f64::from(self.pcm.rate())
            / f64::from(SAMPLE_RATE);
        let len = dry.len().max(1) as f32;
        for (n, (dry, wet)) in dry.iter_mut().zip(wet.iter_mut()).enumerate() {
            if self.cursor >= frames as f64 && !self.wrap(frames) {
                return true;
            }
            let t = n as f32 / len;
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            let mut sample = self.sample(frames);
            if downmix {
                let far = lerp(from.far, levels.far);
                let mono = if flags.contains(WaveFlags::DISTANCE_VARIANT) {
                    sample[0] * (1.0 - far) + sample[1] * far
                } else {
                    f32::midpoint(sample[0], sample[1])
                };
                sample = [mono; 2];
            }
            for ear in 0..2 {
                dry[ear] += sample[ear] * lerp(from.dry[ear], levels.dry[ear]);
                wet[ear] += sample[ear] * lerp(from.wet[ear], levels.wet[ear]);
            }
            self.cursor += step;
        }
        self.cursor >= frames as f64 && !self.wrap(frames)
    }

    /// Past the end: loops back if it loops. Returns whether it goes on.
    fn wrap(&mut self, frames: usize) -> bool {
        let start = match (self.params.looping, self.pcm.loop_start()) {
            (Looping::Once, _) | (Looping::AsRecorded, None) => return false,
            (_, start) => start.unwrap_or(0),
        };
        let span = (frames - start).max(1) as f64;
        self.cursor = start as f64 + (self.cursor - frames as f64) % span;
        true
    }

    /// The frame at the cursor, interpolated with the next.
    fn sample(&self, frames: usize) -> [f32; 2] {
        let whole = self.cursor.floor();
        let fraction = (self.cursor - whole) as f32;
        let index = whole as usize;
        let next = if index + 1 < frames {
            index + 1
        } else {
            match (self.params.looping, self.pcm.loop_start()) {
                (Looping::Always, start) | (Looping::AsRecorded, start @ Some(_)) => {
                    start.unwrap_or(0)
                }
                _ => frames,
            }
        };
        let (a, b) = (self.pcm.frame(index), self.pcm.frame(next));
        [
            a[0] + (b[0] - a[0]) * fraction,
            a[1] + (b[1] - a[1]) * fraction,
        ]
    }
}

#[cfg(test)]
mod tests {
    use source_formats::dsp_presets::DspPresets;

    use super::*;

    /// A second of a full-scale square wave at the mix rate.
    fn tone(channels: u16, loop_start: Option<usize>) -> Pcm {
        let frames = SAMPLE_RATE as usize;
        let samples = (0..frames * usize::from(channels))
            .map(|n| {
                if (n / usize::from(channels) / 50) % 2 == 0 {
                    16384
                } else {
                    -16384
                }
            })
            .collect();
        Pcm::new(samples, channels, SAMPLE_RATE, loop_start)
    }

    /// Peak (left, right) over `frames` frames.
    fn peak(mixer: &mut Mixer, frames: usize) -> [f32; 2] {
        let mut out = vec![0.0; frames * 2];
        mixer.render(&mut out);
        out.chunks(2).skip(BLOCK).fold([0.0f32; 2], |peak, f| {
            [peak[0].max(f[0].abs()), peak[1].max(f[1].abs())]
        })
    }

    fn at(position: Vec3) -> VoiceParams {
        VoiceParams {
            place: Place::World(position),
            soundlevel: 75.0,
            ..VoiceParams::default()
        }
    }

    #[test]
    fn close_sounds_are_louder_than_far_ones() {
        let mut mixer = Mixer::new();
        mixer.play(tone(1, None), at(Vec3::new(100.0, 0.0, 0.0)));
        let near = peak(&mut mixer, 4 * BLOCK);
        mixer.stop_all();
        mixer.play(tone(1, None), at(Vec3::new(1000.0, 0.0, 0.0)));
        let far = peak(&mut mixer, 4 * BLOCK);
        assert!(near[0] > far[0] * 3.0, "{near:?} vs {far:?}");
    }

    #[test]
    fn sounds_to_one_side_are_in_that_ear() {
        let mut mixer = Mixer::new();
        mixer.set_speakers(Speakers::Headphones);
        // Source's y is left.
        mixer.play(tone(1, None), at(Vec3::new(0.0, 100.0, 0.0)));
        let [left, right] = peak(&mut mixer, 4 * BLOCK);
        assert!(left > 0.3 && right < 0.01, "{left} {right}");
    }

    #[test]
    fn voices_end_or_loop_as_asked() {
        let mut mixer = Mixer::new();
        let once = mixer.play(tone(1, None), VoiceParams::default());
        let looping = mixer.play(
            tone(1, None),
            VoiceParams {
                looping: Looping::Always,
                ..VoiceParams::default()
            },
        );
        let recorded = mixer.play(
            tone(2, Some(100)),
            VoiceParams {
                looping: Looping::AsRecorded,
                ..VoiceParams::default()
            },
        );
        let mut out = vec![0.0; SAMPLE_RATE as usize * 3];
        mixer.render(&mut out);
        assert!(!mixer.is_playing(once));
        assert!(mixer.is_playing(looping) && mixer.is_playing(recorded));
        assert_eq!(mixer.take_finished(), [once]);
        assert!(mixer.take_finished().is_empty());
        mixer.stop(looping);
        assert_eq!(mixer.voice_count(), 1);
    }

    #[test]
    fn speed_changes_how_fast_a_sound_runs_out() {
        let mut mixer = Mixer::new();
        let fast = mixer.play(
            tone(1, None),
            VoiceParams {
                speed: 2.0,
                ..VoiceParams::default()
            },
        );
        let mut out = vec![0.0; SAMPLE_RATE as usize + 2 * BLOCK];
        mixer.render(&mut out);
        assert!(
            !mixer.is_playing(fast),
            "a one second sound lasts half a second at double speed"
        );
        let half = Pcm::new(vec![1000; 22050], 1, 22050, None);
        let slow = mixer.play(half, VoiceParams::default());
        let mut out = vec![0.0; SAMPLE_RATE as usize];
        mixer.render(&mut out);
        assert!(
            mixer.is_playing(slow),
            "22050 Hz sounds play at their own rate"
        );
    }

    #[test]
    fn the_room_takes_more_of_far_sounds_and_none_of_dry_ones() {
        let presets = DspPresets::parse("{ 1 LINEAR 0.2 0.7 0 0 80 0.5 { 0 0 } }");
        let preset = presets.get(1).expect("preset");
        assert!((room_mix(preset, 0.0, 100.0) - 0.2).abs() < 1e-6);
        assert!((room_mix(preset, 5000.0, 100.0) - 0.7).abs() < 1e-6);
        assert!(
            (room_mix(preset, 0.0, 75.0) - 0.1).abs() < 1e-6,
            "quiet sounds get less"
        );
        let mut mixer = Mixer::new();
        mixer.set_room(Some(preset.clone()));
        let dry = Voice {
            id: VoiceId(0),
            pcm: tone(1, None),
            params: VoiceParams {
                flags: WaveFlags::of("#a.wav"),
                ..at(Vec3::new(500.0, 0.0, 0.0))
            },
            cursor: 0.0,
            levels: None,
            clearance: 1.0,
        };
        let room = RoomMix::of(preset);
        let listener = Listener::default();
        assert_eq!(
            dry.levels(&listener, Speakers::Stereo, Some(&room)).wet,
            [0.0; 2]
        );
        let wet = Voice {
            params: at(Vec3::new(500.0, 0.0, 0.0)),
            ..dry
        };
        assert!(wet.levels(&listener, Speakers::Stereo, Some(&room)).wet[0] > 0.0);
    }

    #[test]
    fn walls_muffle_slowly() {
        let mut mixer = Mixer::new();
        let id = mixer.play(tone(1, None), at(Vec3::new(100.0, 0.0, 0.0)));
        let open = peak(&mut mixer, 4 * BLOCK);
        if let Some(params) = mixer.params_mut(id) {
            params.obscured = true;
        }
        let mut out = vec![0.0; SAMPLE_RATE as usize / 2];
        mixer.render(&mut out);
        let behind = peak(&mut mixer, 4 * BLOCK);
        assert!(
            (behind[0] / open[0] - obscured_level()).abs() < 0.02,
            "{open:?} {behind:?}"
        );
    }
}
