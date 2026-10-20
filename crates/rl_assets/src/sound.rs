//! Sound loading: `source://sound/**/*.wav` (and `.mp3`) becomes a
//! [`SoundAsset`].
//!
//! Source's WAVs come in formats Bevy's player does not all read (8-bit,
//! Microsoft ADPCM), and whether one loops is told by a cue chunk the
//! player ignores. The loader decodes them itself (`source_formats::wav`)
//! and hands the player plain 16-bit PCM, noting whether the sound loops.
//! MP3s (music) are passed through as they are.

use std::sync::Arc;

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext};
use bevy_audio::AudioSource;
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
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
}

/// A sound ready to play.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct SoundAsset {
    /// The audio itself, for an `AudioPlayer`.
    pub audio: Handle<AudioSource>,
    /// Whether the sound loops (a WAV with a cue point or sampler loop).
    pub looped: bool,
    /// Its length in seconds (zero when unknown, as for MP3s).
    pub duration: f32,
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
        let (bytes, looped, duration) = if is_mp3 {
            (bytes, false, 0.0)
        } else {
            let decoded = wav::decode(&bytes)?;
            (
                decoded.to_pcm16(),
                decoded.loop_start.is_some(),
                decoded.duration(),
            )
        };
        let audio = load_context.add_labeled_asset(
            "audio".to_owned(),
            AudioSource {
                bytes: Arc::from(bytes),
            },
        );
        Ok(SoundAsset {
            audio,
            looped,
            duration,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["wav", "mp3"]
    }
}
