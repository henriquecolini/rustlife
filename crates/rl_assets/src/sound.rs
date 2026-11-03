//! Sound loading: `source://sound/**/*.wav` (and `.mp3`) becomes a
//! [`SoundAsset`].
//!
//! Sounds are decoded once, when they load, into 16-bit PCM for the
//! client's mixer (`rl_audio`): Source's WAVs by `source_formats::wav`
//! (8-bit, 16-bit, 24-bit and Microsoft ADPCM, with the cue point that
//! makes a sound loop), MP3s (music) by Bevy's decoder.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, LoadContext};
use bevy_audio::{AudioSource, Decodable, Source};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use rl_audio::Pcm;
use source_formats::wav::{self, WavError};
use thiserror::Error;

/// Errors from [`SoundLoader`].
#[derive(Debug, Error)]
pub enum SoundLoadError {
    /// The asset bytes could not be read from the reader.
    #[error("reading sound bytes: {0}")]
    Read(#[from] std::io::Error),
    /// The bytes are not a WAV this game reads.
    #[error(transparent)]
    Decode(#[from] WavError),
    /// The bytes are not an MP3 that decodes.
    #[error("not a readable MP3")]
    Mp3,
}

/// A sound ready to play.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct SoundAsset {
    /// The decoded samples.
    pub pcm: Pcm,
}

impl SoundAsset {
    /// Whether the sound loops (a WAV with a cue point or sampler loop).
    #[must_use]
    pub fn looped(&self) -> bool {
        self.pcm.loop_start().is_some()
    }

    /// Its length in seconds.
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.pcm.duration()
    }
}

/// Loads `.wav` and `.mp3` files into [`SoundAsset`]s.
#[derive(Default, TypePath)]
pub struct SoundLoader;

impl AssetLoader for SoundLoader {
    type Asset = SoundAsset;
    type Settings = ();
    type Error = SoundLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<SoundAsset, SoundLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        let is_mp3 = load_context
            .path()
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mp3"));
        let pcm = if is_mp3 {
            decode_mp3(bytes).ok_or(SoundLoadError::Mp3)?
        } else {
            let decoded = wav::decode(&bytes)?;
            Pcm::new(
                decoded.samples,
                decoded.channels,
                decoded.sample_rate,
                decoded.loop_start.map(|frame| frame as usize),
            )
        };
        Ok(SoundAsset { pcm })
    }

    fn extensions(&self) -> &[&str] {
        &["wav", "mp3"]
    }
}

/// Decodes an MP3 with Bevy's decoder, which panics on some bad input:
/// that is caught and reported as a failed load.
fn decode_mp3(bytes: Vec<u8>) -> Option<Pcm> {
    let source = AudioSource {
        bytes: Arc::from(bytes),
    };
    catch_unwind(AssertUnwindSafe(|| {
        let decoder = source.decoder();
        let channels = decoder.channels().get();
        let rate = decoder.sample_rate().get();
        let samples: Vec<i16> = decoder
            .map(|sample| (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect();
        Pcm::new(samples, channels, rate, None)
    }))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbage_is_not_an_mp3() {
        assert!(decode_mp3(b"definitely not an mp3".to_vec()).is_none());
    }
}
