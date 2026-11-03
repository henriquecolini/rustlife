//! Building blocks of the room processors: delay lines and filters.

use std::f32::consts::PI;

use crate::SAMPLE_RATE;

/// Samples in `ms` milliseconds, at least one.
#[must_use]
pub(crate) fn samples(ms: f32) -> usize {
    ((ms.max(0.0) * SAMPLE_RATE as f32 / 1000.0).round() as usize).max(1)
}

/// A circular buffer that gives back what went in a number of samples ago.
#[derive(Debug, Clone)]
pub(crate) struct DelayLine {
    buffer: Vec<f32>,
    write: usize,
}

impl DelayLine {
    /// A line that can delay by up to `max` samples.
    pub(crate) fn new(max: usize) -> Self {
        Self {
            buffer: vec![0.0; max.max(1) + 1],
            write: 0,
        }
    }

    /// The sample that went in `delay` samples ago (clamped to the line's
    /// length).
    pub(crate) fn read(&self, delay: usize) -> f32 {
        let len = self.buffer.len();
        let delay = delay.clamp(1, len - 1);
        self.buffer[(self.write + len - delay) % len]
    }

    /// The same at a fractional delay, interpolated.
    pub(crate) fn read_fraction(&self, delay: f32) -> f32 {
        let whole = delay.floor();
        let fraction = delay - whole;
        let a = self.read(whole as usize);
        let b = self.read(whole as usize + 1);
        a + (b - a) * fraction
    }

    /// Pushes the next sample in.
    pub(crate) fn write(&mut self, sample: f32) {
        self.buffer[self.write] = sample;
        self.write = (self.write + 1) % self.buffer.len();
    }
}

/// A one-pole low-pass filter.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OnePole {
    coefficient: f32,
    state: f32,
}

impl OnePole {
    /// Cutting off above `cutoff` Hz; passing everything at 0 or less.
    pub(crate) fn new(cutoff: f32) -> Self {
        let coefficient = if cutoff <= 0.0 {
            1.0
        } else {
            1.0 - (-2.0 * PI * cutoff.min(SAMPLE_RATE as f32 * 0.45) / SAMPLE_RATE as f32).exp()
        };
        Self {
            coefficient,
            state: 0.0,
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        self.state += self.coefficient * (x - self.state);
        self.state
    }
}

/// Which band a [`Biquad`] keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Band {
    Low,
    High,
    Pass,
}

/// A second-order filter (RBJ's cookbook).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    /// Keeping `band` at `cutoff` Hz; a band pass reaches `width` Hz above
    /// it.
    pub(crate) fn new(band: Band, cutoff: f32, width: f32) -> Self {
        let nyquist = SAMPLE_RATE as f32 * 0.45;
        let (frequency, q) = match band {
            Band::Pass if width > 0.0 => {
                let low = cutoff.clamp(10.0, nyquist);
                let high = (cutoff + width).clamp(low + 1.0, nyquist);
                let centre = (low * high).sqrt();
                (centre, centre / (high - low))
            }
            _ => (cutoff.clamp(10.0, nyquist), std::f32::consts::FRAC_1_SQRT_2),
        };
        let omega = 2.0 * PI * frequency / SAMPLE_RATE as f32;
        let (sin, cos) = omega.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b, a0, a) = match band {
            Band::Low => {
                let b1 = 1.0 - cos;
                (
                    [b1 / 2.0, b1, b1 / 2.0],
                    1.0 + alpha,
                    [-2.0 * cos, 1.0 - alpha],
                )
            }
            Band::High => {
                let b1 = 1.0 + cos;
                (
                    [b1 / 2.0, -b1, b1 / 2.0],
                    1.0 + alpha,
                    [-2.0 * cos, 1.0 - alpha],
                )
            }
            Band::Pass => ([alpha, 0.0, -alpha], 1.0 + alpha, [-2.0 * cos, 1.0 - alpha]),
        };
        Self {
            b: b.map(|b| b / a0),
            a: a.map(|a| a / a0),
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// A feedback comb: a delay whose output is fed back in, optionally
/// through a low-pass. Its output is the delayed signal only.
#[derive(Debug, Clone)]
pub(crate) struct Comb {
    line: DelayLine,
    delay: usize,
    feedback: f32,
    damping: Option<OnePole>,
}

impl Comb {
    pub(crate) fn new(delay: usize, feedback: f32, cutoff: Option<f32>) -> Self {
        Self {
            line: DelayLine::new(delay),
            delay,
            feedback: feedback.clamp(-0.999, 0.999),
            damping: cutoff.map(OnePole::new),
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let out = self.line.read(self.delay);
        let back = match &mut self.damping {
            Some(filter) => filter.process(out),
            None => out,
        };
        self.line.write(x + self.feedback * back);
        out
    }
}

/// A Schroeder all-pass: smears a sound in time without colouring it.
#[derive(Debug, Clone)]
pub(crate) struct AllPass {
    line: DelayLine,
    delay: usize,
    gain: f32,
}

impl AllPass {
    pub(crate) fn new(delay: usize, gain: f32) -> Self {
        Self {
            line: DelayLine::new(delay),
            delay,
            gain: gain.clamp(-0.999, 0.999),
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let delayed = self.line.read(self.delay);
        let w = x + self.gain * delayed;
        self.line.write(w);
        delayed - self.gain * w
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delay_line_gives_back_what_went_in() {
        let mut line = DelayLine::new(4);
        for sample in 1..=6 {
            line.write(sample as f32);
        }
        assert_eq!(line.read(1), 6.0);
        assert_eq!(line.read(4), 3.0);
        assert_eq!(line.read_fraction(1.5), 5.5);
        assert_eq!(line.read(99), line.read(4), "clamped to its length");
    }

    #[test]
    fn a_comb_echoes_and_decays() {
        let mut comb = Comb::new(3, 0.5, None);
        let out: Vec<f32> = (0..10)
            .map(|n| comb.process(if n == 0 { 1.0 } else { 0.0 }))
            .collect();
        assert_eq!(out[3], 1.0);
        assert_eq!(out[6], 0.5);
        assert_eq!(out[9], 0.25);
        assert_eq!(out[0], 0.0, "no direct sound");
    }

    #[test]
    fn an_all_pass_keeps_energy() {
        let mut filter = AllPass::new(7, 0.5);
        let energy: f32 = (0..2000)
            .map(|n| filter.process(if n == 0 { 1.0 } else { 0.0 }).powi(2))
            .sum();
        assert!((energy - 1.0).abs() < 1e-3, "{energy}");
    }

    #[test]
    fn a_low_pass_keeps_bass_and_cuts_treble() {
        let tone = |hz: f32| {
            let mut filter = Biquad::new(Band::Low, 1000.0, 0.0);
            (0..4410)
                .map(|n| {
                    let x = (2.0 * PI * hz * n as f32 / SAMPLE_RATE as f32).sin();
                    filter.process(x)
                })
                .skip(2000)
                .fold(0.0f32, |peak, y| peak.max(y.abs()))
        };
        assert!(tone(100.0) > 0.95);
        assert!(tone(8000.0) < 0.05);
        let mut smooth = OnePole::new(500.0);
        let settled = (0..2000).map(|_| smooth.process(1.0)).last().unwrap_or(0.0);
        assert!((settled - 1.0).abs() < 1e-3);
    }
}
