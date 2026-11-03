//! rustlife's sound mixer, after Source's.
//!
//! Bevy's own audio plays each sound on its own and can only pan it a
//! little and fade it with the square of distance. Half-Life 2:
//! Deathmatch's engine does more, and this crate does it the same way:
//!
//! - [`falloff`]: how loud a sound is at a distance, from its sound level
//!   (`SND_GetGainFromMult`, with the engine's console defaults).
//! - [`pan`]: which ear hears it, from its direction (the engine's
//!   headphone and stereo-speaker pan laws).
//! - [`dsp`]: the room effect — the echo and reverberation of the place,
//!   from the game's own `dsp_presets.txt`, chosen by the soundscape or
//!   worked out from the shape of the place ([`dsp::room`]).
//! - [`mixer`]: every playing sound, placed and mixed to stereo with the
//!   room effect.
//!
//! The numbers that make HL2DM sound as it does (reference distance and
//! level, compressor, pan exponents, the room effect's mix range and its
//! room classifier) were read from the game's `engine.so`; where the
//! engine's code was not followed (the inside of the room processors) the
//! module says so.
//!
//! Nothing here knows about Bevy: the client feeds the [`Mixer`] and plays
//! what it renders.

#![forbid(unsafe_code)]

pub mod dsp;
pub mod falloff;
pub mod mixer;
pub mod pan;
pub mod pcm;

pub use mixer::{Listener, Looping, Mixer, Place, VoiceId, VoiceParams};
pub use pan::{Frame, Speakers};
pub use pcm::Pcm;

/// Frames per second the mixer runs at (Source's).
pub const SAMPLE_RATE: u32 = 44_100;
