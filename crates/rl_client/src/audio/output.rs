//! The mixer's way out: one endless stream that Bevy's audio output plays,
//! whose samples the [`Mixer`] renders as they are asked for, and the
//! listener it hears from — the camera.
//!
//! The game and the audio device's thread share the mixer behind a lock:
//! the game changes voices and moves the listener between frames, the
//! device renders a block of [`BLOCK`] frames at a time (5.8 ms), so
//! neither holds it long.

use std::num::NonZero;
use std::sync::{Arc, Mutex, PoisonError};

use bevy::audio::{AddAudioSource, Decodable, Source};
use bevy::prelude::*;
use rl_assets::coords;
use rl_audio::mixer::BLOCK;
use rl_audio::{Frame, Listener, Mixer, SAMPLE_RATE, Speakers};

use crate::camera::FlyCam;
use crate::settings::Settings;

/// Sets up the mixer, its output and its listener.
pub(super) struct OutputPlugin;

impl Plugin for OutputPlugin {
    fn build(&self, app: &mut App) {
        let mixer = MixerHandle::default();
        let bus = MixBus(mixer.clone());
        app.insert_resource(mixer)
            .add_audio_source::<MixBus>()
            .add_systems(
                Startup,
                move |mut commands: Commands, mut buses: ResMut<Assets<MixBus>>| {
                    commands.spawn((AudioPlayer(buses.add(bus.clone())), PlaybackSettings::ONCE));
                },
            )
            .add_systems(Update, (set_listener, set_speakers).in_set(SetListener));
    }
}

/// Moves the mixer's listener to the camera (and applies the speaker
/// setting): sound systems that place voices run after it.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SetListener;

/// The mixer every sound plays through, shared with the audio device.
#[derive(Resource, Clone, Default)]
pub struct MixerHandle(Arc<Mutex<Mixer>>);

impl MixerHandle {
    /// Runs `f` on the mixer.
    pub fn with<T>(&self, f: impl FnOnce(&mut Mixer) -> T) -> T {
        // A panic while holding the lock leaves the mixer usable: its state
        // is only voices and buffers.
        let mut mixer = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut mixer)
    }
}

/// The mixer as something Bevy's audio output can play.
#[derive(Asset, TypePath, Clone)]
pub struct MixBus(MixerHandle);

impl Decodable for MixBus {
    type Decoder = MixerSource;

    fn decoder(&self) -> MixerSource {
        MixerSource {
            mixer: self.0.clone(),
            block: vec![0.0; BLOCK * 2],
            next: BLOCK * 2,
        }
    }
}

/// The endless stream of the mixer's output, interleaved stereo at
/// [`SAMPLE_RATE`].
pub struct MixerSource {
    mixer: MixerHandle,
    block: Vec<f32>,
    next: usize,
}

impl Iterator for MixerSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.next >= self.block.len() {
            self.mixer.with(|mixer| mixer.render(&mut self.block));
            self.next = 0;
        }
        let sample = self.block[self.next];
        self.next += 1;
        Some(sample)
    }
}

impl Source for MixerSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZero<u16> {
        NonZero::<u16>::MIN.saturating_add(1)
    }

    fn sample_rate(&self) -> NonZero<u32> {
        NonZero::new(SAMPLE_RATE).unwrap_or(NonZero::<u32>::MIN)
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

/// Puts the listener at the camera, facing where it faces.
fn set_listener(mixer: Res<MixerHandle>, cameras: Query<&GlobalTransform, With<FlyCam>>) {
    let Some(camera) = cameras.iter().next() else {
        return;
    };
    let source = |direction: Vec3| Vec3::from_array(coords::to_source_direction(direction));
    let listener = Listener {
        position: Vec3::from_array(coords::to_source_position(camera.translation())),
        frame: Frame {
            forward: source(camera.forward().into()),
            right: source(camera.right().into()),
            up: source(camera.up().into()),
        },
    };
    mixer.with(|mixer| mixer.set_listener(listener));
}

/// Applies the speaker setting.
fn set_speakers(mixer: Res<MixerHandle>, settings: Res<Settings>) {
    if !settings.is_changed() {
        return;
    }
    let speakers = match settings.audio.speakers {
        rl_config::Speakers::Stereo => Speakers::Stereo,
        rl_config::Speakers::Headphones => Speakers::Headphones,
    };
    mixer.with(|mixer| mixer.set_speakers(speakers));
}

#[cfg(test)]
mod tests {
    use rl_audio::{Pcm, VoiceParams};

    use super::*;

    #[test]
    fn the_stream_is_the_mixers_output() {
        let mixer = MixerHandle::default();
        mixer.with(|mixer| {
            mixer.play(
                Pcm::new(vec![16384; 1000], 1, SAMPLE_RATE, None),
                VoiceParams::default(),
            )
        });
        let mut stream = MixBus(mixer).decoder();
        assert_eq!(
            (stream.channels().get(), stream.sample_rate().get()),
            (2, SAMPLE_RATE)
        );
        let first: Vec<f32> = stream.by_ref().take(4).collect();
        assert!(first.iter().all(|&s| s > 0.1), "{first:?}");
        assert!(stream.nth(10_000).is_some(), "and it never ends");
    }
}
