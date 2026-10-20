//! WAV sounds, decoded to 16-bit PCM.
//!
//! Source ships its sounds as RIFF WAVE files: mostly 8- and 16-bit PCM at
//! 11, 22 or 44 kHz, mono or stereo, and some Microsoft ADPCM (4 bits a
//! sample). Whether a sound loops is not in any script: the engine loops a
//! WAV that carries a `cue ` chunk (or a sampler `smpl` loop), from the cue
//! point to the end. [`decode`] reads all of that into a [`Wav`] of
//! interleaved 16-bit samples, which [`Wav::to_pcm16`] writes back out as a
//! plain PCM file any player decodes.

use thiserror::Error;

/// WAVE format tags.
const FORMAT_PCM: u16 = 1;
const FORMAT_ADPCM: u16 = 2;

/// Microsoft ADPCM's adaptation table, by nibble.
const ADAPTATION: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
/// Microsoft ADPCM's standard predictor coefficients, used when a file
/// lists none.
const STANDARD_COEFFICIENTS: [(i32, i32); 7] = [
    (256, 0),
    (512, -256),
    (0, 0),
    (192, 64),
    (240, 0),
    (460, -208),
    (392, -232),
];

/// Errors from [`decode`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WavError {
    /// Not a RIFF WAVE file.
    #[error("not a RIFF WAVE file")]
    NotWave,
    /// A chunk the decoder needs is missing or cut short.
    #[error("missing or truncated {0} chunk")]
    Missing(&'static str),
    /// A format this decoder does not read.
    #[error("unsupported WAVE format {tag} with {bits} bits a sample")]
    Unsupported {
        /// The format tag.
        tag: u16,
        /// Bits per sample.
        bits: u16,
    },
    /// The format chunk holds impossible values.
    #[error("invalid format: {0}")]
    Invalid(&'static str),
}

/// A decoded sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wav {
    /// Channels (1 mono, 2 stereo).
    pub channels: u16,
    /// Frames per second.
    pub sample_rate: u32,
    /// Interleaved samples, `channels` per frame.
    pub samples: Vec<i16>,
    /// The frame a looping sound loops back to, if it loops.
    pub loop_start: Option<u32>,
}

impl Wav {
    /// The number of frames.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.max(1))
    }

    /// The length in seconds.
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.frames() as f32 / self.sample_rate.max(1) as f32
    }

    /// The sound as a 16-bit PCM WAVE file.
    #[must_use]
    pub fn to_pcm16(&self) -> Vec<u8> {
        let data_len = (self.samples.len() * 2) as u32;
        let block_align = self.channels * 2;
        let mut out = Vec::with_capacity(44 + self.samples.len() * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&FORMAT_PCM.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&(self.sample_rate * u32::from(block_align)).to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for sample in &self.samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }
}

/// The parts of the format chunk the decoders use.
struct Format<'a> {
    tag: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits: u16,
    /// Bytes after the basic 16.
    extra: &'a [u8],
}

/// Decodes a WAVE file. Never panics on malformed input.
///
/// # Errors
///
/// Fails on files that are not WAVE, lack a format or data chunk, or use a
/// format other than PCM (8, 16 or 24 bits) or Microsoft ADPCM.
pub fn decode(bytes: &[u8]) -> Result<Wav, WavError> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }
    let mut format = None;
    let mut data = None;
    let mut cue = None;
    let mut sampler_loop = None;
    let mut at = 12;
    while let Some(header) = bytes.get(at..at + 8) {
        let id = &header[..4];
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let body_start = at + 8;
        // A truncated last chunk keeps what there is (common in content).
        let body = &bytes[body_start..bytes.len().min(body_start.saturating_add(len))];
        match id {
            b"fmt " => format = Some(read_format(body)?),
            b"data" => data = Some(body),
            // The first cue point's sample offset.
            b"cue " => cue = body.get(4 + 20..4 + 24).map(read_u32),
            // The first sampler loop's start.
            b"smpl" if body.get(28..32).is_some_and(|loops| read_u32(loops) > 0) => {
                sampler_loop = body.get(36 + 8..36 + 12).map(read_u32);
            }
            _ => {}
        }
        at = body_start.saturating_add(len).saturating_add(len & 1);
    }
    let format = format.ok_or(WavError::Missing("fmt"))?;
    let data = data.ok_or(WavError::Missing("data"))?;
    if format.channels == 0 || format.sample_rate == 0 {
        return Err(WavError::Invalid("no channels or no sample rate"));
    }
    let samples = match (format.tag, format.bits) {
        (FORMAT_PCM, 8) => data
            .iter()
            .map(|&byte| (i16::from(byte) - 128) << 8)
            .collect(),
        (FORMAT_PCM, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect(),
        (FORMAT_PCM, 24) => data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triple| i16::from_le_bytes([triple[1], triple[2]]))
            .collect(),
        (FORMAT_ADPCM, 4) => decode_adpcm(&format, data)?,
        (tag, bits) => return Err(WavError::Unsupported { tag, bits }),
    };
    let wav = Wav {
        channels: format.channels,
        sample_rate: format.sample_rate,
        samples,
        loop_start: None,
    };
    let frames = wav.frames() as u32;
    Ok(Wav {
        loop_start: cue.or(sampler_loop).filter(|&start| start < frames.max(1)),
        ..wav
    })
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn read_format(body: &[u8]) -> Result<Format<'_>, WavError> {
    let missing = WavError::Missing("fmt");
    let u16_at = |at| read_u16(body, at).ok_or(missing.clone());
    Ok(Format {
        tag: u16_at(0)?,
        channels: u16_at(2)?,
        sample_rate: body.get(4..8).map(read_u32).ok_or(missing.clone())?,
        block_align: u16_at(12)?,
        bits: u16_at(14)?,
        extra: body.get(16..).unwrap_or_default(),
    })
}

/// Decodes Microsoft ADPCM: blocks of `block_align` bytes, each starting
/// with a header per channel (predictor, delta, two samples) followed by
/// 4-bit codes, interleaved by channel.
fn decode_adpcm(format: &Format, data: &[u8]) -> Result<Vec<i16>, WavError> {
    let channels = usize::from(format.channels);
    if channels > 2 {
        return Err(WavError::Invalid("ADPCM with more than two channels"));
    }
    let block = usize::from(format.block_align);
    if block < 7 * channels {
        return Err(WavError::Invalid("ADPCM block too small"));
    }
    // The extra format bytes: size, samples per block, coefficient count,
    // then the coefficient pairs.
    let coefficients: Vec<(i32, i32)> = match read_u16(format.extra, 4) {
        Some(count) if count > 0 => (0..usize::from(count))
            .filter_map(|index| {
                let at = 6 + index * 4;
                let first = read_u16(format.extra, at)? as i16;
                let second = read_u16(format.extra, at + 2)? as i16;
                Some((i32::from(first), i32::from(second)))
            })
            .collect(),
        _ => STANDARD_COEFFICIENTS.to_vec(),
    };
    let mut out = Vec::new();
    for chunk in data.chunks(block) {
        if chunk.len() < 7 * channels {
            break;
        }
        let mut states = [Channel::default(); 2];
        // Header: predictors, deltas, second samples, first samples.
        for (channel, state) in states.iter_mut().enumerate().take(channels) {
            let predictor = usize::from(chunk[channel]);
            let (c1, c2) = coefficients
                .get(predictor)
                .copied()
                .ok_or(WavError::Invalid("ADPCM predictor out of range"))?;
            let word = |field: usize| {
                let at = channels + (field * channels + channel) * 2;
                i32::from(i16::from_le_bytes([chunk[at], chunk[at + 1]]))
            };
            *state = Channel {
                c1,
                c2,
                delta: word(0),
                sample1: word(1),
                sample2: word(2),
            };
        }
        for state in states.iter().take(channels) {
            out.push(state.sample2 as i16);
        }
        for state in states.iter().take(channels) {
            out.push(state.sample1 as i16);
        }
        let mut channel = 0;
        for &byte in &chunk[7 * channels..] {
            for nibble in [byte >> 4, byte & 0x0f] {
                out.push(states[channel].step(nibble));
                channel = (channel + 1) % channels;
            }
        }
    }
    Ok(out)
}

/// One channel's ADPCM decoder state.
#[derive(Debug, Clone, Copy, Default)]
struct Channel {
    c1: i32,
    c2: i32,
    delta: i32,
    sample1: i32,
    sample2: i32,
}

impl Channel {
    fn step(&mut self, nibble: u8) -> i16 {
        let signed = if nibble & 0x08 != 0 {
            i32::from(nibble) - 16
        } else {
            i32::from(nibble)
        };
        let predicted = (self.sample1 * self.c1 + self.sample2 * self.c2) >> 8;
        let sample = (predicted + signed * self.delta).clamp(-32768, 32767);
        self.sample2 = self.sample1;
        self.sample1 = sample;
        self.delta = ((ADAPTATION[usize::from(nibble & 0x0f)] * self.delta) >> 8).max(16);
        sample as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAVE file of `tag`/`bits` with `data`, plus extra chunks.
    fn wave(
        tag: u16,
        channels: u16,
        bits: u16,
        block_align: u16,
        extra_fmt: &[u8],
        data: &[u8],
        chunks: &[(&[u8; 4], Vec<u8>)],
    ) -> Vec<u8> {
        let mut body = Vec::new();
        let mut chunk = |id: &[u8], content: &[u8]| {
            body.extend_from_slice(id);
            body.extend_from_slice(&(content.len() as u32).to_le_bytes());
            body.extend_from_slice(content);
            if content.len() % 2 == 1 {
                body.push(0);
            }
        };
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&tag.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&22050u32.to_le_bytes());
        fmt.extend_from_slice(&44100u32.to_le_bytes());
        fmt.extend_from_slice(&block_align.to_le_bytes());
        fmt.extend_from_slice(&bits.to_le_bytes());
        fmt.extend_from_slice(extra_fmt);
        chunk(b"fmt ", &fmt);
        for (id, content) in chunks {
            chunk(*id, content);
        }
        chunk(b"data", data);
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend(body);
        out
    }

    #[test]
    fn eight_and_sixteen_bit_pcm_decode_to_sixteen_bits() {
        let eight = decode(&wave(1, 1, 8, 1, &[], &[0, 128, 255], &[])).expect("8-bit");
        assert_eq!(eight.samples, [-32768, 0, 32512]);
        assert_eq!(
            (eight.channels, eight.sample_rate, eight.loop_start),
            (1, 22050, None)
        );

        let samples: Vec<u8> = [-2i16, 300, 7, -9]
            .iter()
            .flat_map(|s| s.to_le_bytes())
            .collect();
        let stereo = decode(&wave(1, 2, 16, 4, &[], &samples, &[])).expect("16-bit");
        assert_eq!(stereo.samples, [-2, 300, 7, -9]);
        assert_eq!(stereo.frames(), 2);
    }

    #[test]
    fn a_cue_point_makes_a_sound_loop() {
        let mut cue = vec![0u8; 28];
        cue[..4].copy_from_slice(&1u32.to_le_bytes());
        cue[24..28].copy_from_slice(&2u32.to_le_bytes());
        let looped = decode(&wave(1, 1, 8, 1, &[], &[1, 2, 3, 4], &[(b"cue ", cue)])).expect("wav");
        assert_eq!(looped.loop_start, Some(2));
    }

    #[test]
    fn written_pcm_reads_back_the_same() {
        let wav = Wav {
            channels: 2,
            sample_rate: 11025,
            samples: vec![1, -1, 1000, -1000],
            loop_start: None,
        };
        assert_eq!(decode(&wav.to_pcm16()).expect("round trip"), wav);
        assert!((wav.duration() - 2.0 / 11025.0).abs() < 1e-9);
    }

    #[test]
    fn adpcm_blocks_decode_their_header_samples_and_codes() {
        // One mono block: predictor 0 (256, 0), delta 16, samples 100 then
        // 200 (stored second-first), then codes +1 and -1.
        let mut block = vec![0u8];
        block.extend_from_slice(&16i16.to_le_bytes());
        block.extend_from_slice(&200i16.to_le_bytes());
        block.extend_from_slice(&100i16.to_le_bytes());
        block.push(0x1f);
        let extra = [32u8, 0, 4, 0, 0, 0]; // no coefficient table: standard
        let wav = decode(&wave(2, 1, 4, block.len() as u16, &extra, &block, &[])).expect("adpcm");
        // 200 + 1·16 = 216; then 216 − 16 = 200 (delta stays at its floor).
        assert_eq!(wav.samples, [100, 200, 216, 200]);
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert_eq!(decode(b"RIFF"), Err(WavError::NotWave));
        assert_eq!(decode(b"RIFF\0\0\0\0WAVE"), Err(WavError::Missing("fmt")));
        let ima = wave(0x11, 1, 4, 256, &[], &[0; 16], &[]);
        assert_eq!(
            decode(&ima),
            Err(WavError::Unsupported { tag: 0x11, bits: 4 })
        );
        for length in 0..64 {
            let truncated = &wave(1, 1, 16, 2, &[], &[1, 2, 3, 4], &[])[..length.min(48)];
            let _ = decode(truncated);
        }
    }
}
