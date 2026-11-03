//! Decoded sound data.

use std::sync::Arc;

/// A decoded sound: 16-bit samples, interleaved by channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    samples: Arc<[i16]>,
    channels: u16,
    rate: u32,
    loop_start: Option<usize>,
}

impl Pcm {
    /// A sound of `samples` with `channels` channels (1 or 2; more are
    /// read as their first two) at `rate` Hz, looping back to frame
    /// `loop_start` if it has one.
    #[must_use]
    pub fn new(samples: Vec<i16>, channels: u16, rate: u32, loop_start: Option<usize>) -> Self {
        let channels = channels.max(1);
        let frames = samples.len() / usize::from(channels);
        Self {
            samples: samples.into(),
            channels,
            rate: rate.max(1),
            loop_start: loop_start.filter(|&start| start < frames),
        }
    }

    /// Number of frames.
    #[must_use]
    pub fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }

    /// Channels per frame.
    #[must_use]
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames per second.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// The frame it loops back to, if it loops.
    #[must_use]
    pub fn loop_start(&self) -> Option<usize> {
        self.loop_start
    }

    /// Length in seconds.
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.frames() as f32 / self.rate as f32
    }

    /// Frame `frame` as (left, right) in `-1..1`; a mono sound is the same
    /// in both. Past the end it is silent.
    #[must_use]
    pub fn frame(&self, frame: usize) -> [f32; 2] {
        let channels = usize::from(self.channels);
        let at = frame * channels;
        let sample = |i: usize| self.samples.get(i).map_or(0.0, |&s| f32::from(s) / 32768.0);
        if channels == 1 {
            let s = sample(at);
            [s, s]
        } else {
            [sample(at), sample(at + 1)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_by_channel() {
        let stereo = Pcm::new(vec![16384, -16384, 0, 32767], 2, 22050, Some(1));
        assert_eq!(stereo.frames(), 2);
        assert_eq!(stereo.frame(0), [0.5, -0.5]);
        assert_eq!(stereo.frame(5), [0.0, 0.0]);
        assert!((stereo.duration() - 2.0 / 22050.0).abs() < 1e-9);
        assert_eq!(stereo.loop_start(), Some(1));
        let mono = Pcm::new(vec![8192; 4], 1, 0, Some(9));
        assert_eq!(mono.frame(3), [0.25, 0.25]);
        assert_eq!((mono.rate(), mono.loop_start()), (1, None), "sanitised");
    }
}
