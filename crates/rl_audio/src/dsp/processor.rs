//! The processors of a room preset, built from their `dsp_presets.txt`
//! parameters.
//!
//! HL2DM's processors are known from the preset file's own documentation
//! of every parameter; how each one computes its output is rustlife's
//! (standard comb, all-pass and biquad designs), aiming, as the file says
//! its gains do, at an output as loud as the input.

use source_formats::dsp_presets::{MAX_PARAMS, ProcessorDef, ProcessorKind};

use super::units::{AllPass, Band, Biquad, Comb, DelayLine, OnePole, samples};
use crate::SAMPLE_RATE;

/// Diffusor delays, milliseconds, before scaling ("13 ms to 41 ms").
const DIFFUSOR_MS: [f32; 5] = [13.0, 17.0, 23.0, 31.0, 41.0];
/// Level of a reverberator's tail under the sound that sets it off, its
/// energy in all (-6 dB). The sound itself passes through at full level,
/// as Source's delays pass their input (`y(n) = w(n)`).
const TAIL: f32 = 0.5;
/// Fewest delays a reverberator runs.
const MIN_COMBS: usize = 6;
/// All-passes (ms) after a reverberator's delays, and their feedback:
/// they turn its repeats into a smooth tail (rustlife's, for a reverb
/// with little echo).
const WASH_MS: [f32; 4] = [12.6, 10.0, 7.7, 5.1];
const WASH_FEEDBACK: f32 = 0.5;
/// Level of a delay's echoes under the sound that sets them off (-12 dB):
/// rooms should reverberate, not echo.
const ECHO: f32 = 0.25;
/// Spread of the delays a reverb dimension makes when `ftaps` is 0.
const TAP_SPREAD: f32 = 0.07;

/// One running processor.
#[derive(Debug, Clone)]
pub(crate) enum Processor {
    /// Passes its input on.
    Pass,
    Delay(Delay),
    Reverb(Reverb),
    Diffusor(Diffusor),
    Filter(Filter),
    Amplifier(Amplifier),
}

impl Processor {
    /// The processor `def` describes. Kinds no room uses (pitch shifting,
    /// chorus, envelopes) pass their input on.
    pub(crate) fn new(def: &ProcessorDef) -> Self {
        let p = &def.params;
        match def.kind {
            ProcessorKind::Delay => Self::Delay(Delay::new(p)),
            ProcessorKind::ModDelay => Self::Delay(Delay::modulated(p)),
            ProcessorKind::Reverb => Self::Reverb(Reverb::new(p)),
            ProcessorKind::Diffusor => Self::Diffusor(Diffusor::new(p)),
            ProcessorKind::Filter => Self::Filter(Filter::new(p)),
            ProcessorKind::Amplifier => Self::Amplifier(Amplifier::new(p)),
            _ => Self::Pass,
        }
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        match self {
            Self::Pass => x,
            Self::Delay(delay) => delay.process(x),
            Self::Reverb(reverb) => reverb.process(x),
            Self::Diffusor(diffusor) => diffusor.process(x),
            Self::Filter(filter) => filter.process(x),
            Self::Amplifier(amplifier) => amplifier.process(x),
        }
    }
}

/// `DLY` (and `MDY`): a delay, with feedback or not, filtered or not.
///
/// Parameters: type (`PLAIN` 0, `ALLPASS` 1, `LOWPASS` 2, `DLINEAR` 3,
/// `FLINEAR` 4, `LOWPASS_4TAP` 5, `PLAIN_4TAP` 6), delay (ms), feedback,
/// gain, filter type, cutoff (Hz), width, quality, then three tap delays
/// (ms). `MDY` adds a modulation rate (Hz), depth (share of the delay),
/// glide and a wet mix in place of the taps.
#[derive(Debug, Clone)]
pub(crate) struct Delay {
    line: DelayLine,
    length: usize,
    taps: Vec<usize>,
    feedback: f32,
    gain: f32,
    kind: DelayKind,
    loop_filter: Option<OnePole>,
    out_filter: Option<Biquad>,
    modulation: Option<Modulation>,
    mix: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelayKind {
    Feedback,
    AllPass,
    Linear,
}

/// A slow sine wander of a delay's length.
#[derive(Debug, Clone, Copy)]
struct Modulation {
    depth: f32,
    phase: f32,
    step: f32,
}

impl Delay {
    fn new(p: &[f32; MAX_PARAMS]) -> Self {
        let delay = samples(p[1].max(1.0));
        let kind = match p[0] as u32 {
            1 => DelayKind::AllPass,
            3 | 4 => DelayKind::Linear,
            _ => DelayKind::Feedback,
        };
        let filtered = matches!(p[0] as u32, 2 | 5);
        let taps = if matches!(p[0] as u32, 5 | 6) {
            p[8..11]
                .iter()
                .filter(|&&ms| ms > 0.0)
                .map(|&ms| samples(ms).min(delay))
                .collect()
        } else {
            Vec::new()
        };
        Self {
            line: DelayLine::new(delay + 2),
            length: delay,
            taps,
            feedback: p[2].clamp(-0.99, 0.99),
            gain: p[3],
            kind,
            loop_filter: filtered.then(|| OnePole::new(p[5])),
            out_filter: (p[0] as u32 == 4).then(|| Biquad::new(band(p[4]), p[5], p[6])),
            modulation: None,
            mix: 1.0,
        }
    }

    fn modulated(p: &[f32; MAX_PARAMS]) -> Self {
        let mut delay = Self::new(p);
        delay.taps.clear();
        let (rate, depth) = (p[8], p[9].clamp(0.0, 1.0));
        if rate > 0.0 && depth > 0.0 {
            delay.modulation = Some(Modulation {
                depth: depth * delay.length as f32 * 0.5,
                phase: 0.0,
                step: rate / SAMPLE_RATE as f32,
            });
        }
        delay.mix = if p[12] > 0.0 { p[12].min(1.0) } else { 1.0 };
        delay
    }

    fn process(&mut self, x: f32) -> f32 {
        let delayed = match &mut self.modulation {
            Some(wander) => {
                wander.phase = (wander.phase + wander.step).fract();
                let swing = (wander.phase * std::f32::consts::TAU).sin() * wander.depth;
                self.line
                    .read_fraction((self.length as f32 - wander.depth + swing).max(1.0))
            }
            None => self.line.read(self.length),
        };
        let mut out = match self.kind {
            DelayKind::AllPass => {
                let w = x + self.feedback * delayed;
                self.line.write(w);
                delayed - self.feedback * w
            }
            DelayKind::Linear => {
                self.line.write(x);
                delayed
            }
            DelayKind::Feedback => {
                let back = match &mut self.loop_filter {
                    Some(filter) => filter.process(delayed),
                    None => delayed,
                };
                // Source's `w(n) = x(n) + a·w(n - D)`, `y(n) = w(n)`: the
                // sound, then its echoes.
                let w = x + self.feedback * back;
                self.line.write(w);
                x + (w - x) * ECHO
            }
        };
        if !self.taps.is_empty() {
            let taps: f32 = self.taps.iter().map(|&tap| self.line.read(tap)).sum();
            out = (out + taps) / (self.taps.len() + 1) as f32;
        }
        if let Some(filter) = &mut self.out_filter {
            out = filter.process(out);
        }
        x * (1.0 - self.mix) + out * self.gain * self.mix
    }
}

/// `RVA`: parallel feedback delays, a room's reverberation.
///
/// Parameters: longest and shortest delay (ms), number of delays,
/// feedback, gain, cutoff (Hz) of the low-pass in the loops (or after them
/// when "parallel filters" is 0), modulation depth (ms) and rate (left
/// out: the delays hold still), then a room's width, depth and height (ms, about
/// feet) with each one's feedback; a negative feedback makes that
/// dimension a predelay as well. When the dimensions are given they are
/// used instead of the delay range, and `ftaps` spreads each one's delays.
#[derive(Debug, Clone)]
pub(crate) struct Reverb {
    paths: Vec<ReverbPath>,
    gain: f32,
    out_filter: Option<OnePole>,
    /// All-passes that smear the combs' repeats into a wash.
    diffusion: Vec<AllPass>,
}

#[derive(Debug, Clone)]
struct ReverbPath {
    predelay: Option<(DelayLine, usize)>,
    comb: Comb,
    /// The comb's energy gain on noise.
    energy: f32,
}

impl ReverbPath {
    fn new(
        predelay: Option<usize>,
        delay: usize,
        feedback: f32,
        cutoff: Option<f32>,
        out_cutoff: Option<f32>,
    ) -> Self {
        let feedback = feedback.clamp(0.0, 0.999);
        Self {
            predelay: predelay.map(|d| (DelayLine::new(d), d)),
            comb: Comb::new(delay, feedback, cutoff),
            energy: comb_energy(feedback, cutoff, out_cutoff),
        }
    }
}

/// Bandwidth over which [`comb_energy`] averages, Hz: where game sounds
/// (recorded at 11-44 kHz) carry their energy.
const ENERGY_BAND: f32 = 8000.0;

/// Energy gain on noise of a feedback comb whose loop is low-passed at
/// `cutoff` and output at `out_cutoff`: at each frequency the comb's
/// ripple averages to `1 / (1 - r²)`, `r` the loop gain there.
fn comb_energy(feedback: f32, cutoff: Option<f32>, out_cutoff: Option<f32>) -> f32 {
    const POINTS: usize = 64;
    let response =
        |cutoff: Option<f32>, hz: f32| cutoff.map_or(1.0, |cutoff| one_pole_power(cutoff, hz));
    let sum: f32 = (0..POINTS)
        .map(|n| {
            let hz = (n as f32 + 0.5) / POINTS as f32 * ENERGY_BAND;
            let loop_gain = feedback * feedback * response(cutoff, hz);
            response(out_cutoff, hz) / (1.0 - loop_gain).max(1e-3)
        })
        .sum();
    sum / POINTS as f32
}

/// Power response of [`OnePole`] at `hz`.
fn one_pole_power(cutoff: f32, hz: f32) -> f32 {
    let a = 1.0 - (-std::f32::consts::TAU * cutoff / SAMPLE_RATE as f32).exp();
    let w = std::f32::consts::TAU * hz / SAMPLE_RATE as f32;
    let b = 1.0 - a;
    a * a / (1.0 - 2.0 * b * w.cos() + b * b)
}

impl Reverb {
    fn new(p: &[f32; MAX_PARAMS]) -> Self {
        let cutoff = (p[5] > 0.0).then_some(p[5]);
        let in_loop = p[6] != 0.0;
        let (loop_cutoff, out_cutoff) = if in_loop {
            (cutoff, None)
        } else {
            (None, cutoff)
        };
        // Never fewer than `MIN_COMBS`: a few long combs repeat audibly.
        let count = (p[2].max(1.0) as usize).clamp(MIN_COMBS, 12);
        let dimensions = [(p[9], p[12]), (p[10], p[13]), (p[11], p[14])];
        let mut paths = Vec::new();
        if dimensions.iter().any(|&(size, _)| size > 0.0) {
            let per = count.div_ceil(3).max(2);
            let spread = if p[15] > 0.0 { p[15] } else { TAP_SPREAD };
            for (size, feedback) in dimensions.into_iter().filter(|&(size, _)| size > 0.0) {
                let predelay = (feedback < 0.0).then(|| samples(size));
                for n in 0..per {
                    let delay = samples(size * (1.0 - n as f32 * spread).max(0.1));
                    paths.push(ReverbPath::new(
                        predelay,
                        delay,
                        feedback.abs(),
                        loop_cutoff,
                        out_cutoff,
                    ));
                }
            }
        } else {
            let (longest, shortest) = (p[0].max(p[1]).max(1.0), p[1].max(1.0).min(p[0].max(1.0)));
            for n in 0..count {
                let t = if count > 1 {
                    n as f32 / (count - 1) as f32
                } else {
                    1.0
                };
                // Mutually detuned lengths, so the echoes do not line up.
                let ms = shortest + (longest - shortest) * t + n as f32 * 0.37;
                let delay = samples(ms);
                // The same fade time for every delay.
                let feedback = p[3].clamp(0.0, 0.999).powf(ms / longest);
                paths.push(ReverbPath::new(
                    None,
                    delay,
                    feedback,
                    loop_cutoff,
                    out_cutoff,
                ));
            }
        }
        // The paths add in power: scaled together to unit energy, then
        // to `TAIL` under the sound itself.
        let energy: f32 = paths.iter().map(|path| path.energy).sum();
        let gain = if energy > 0.0 {
            TAIL * energy.sqrt().recip()
        } else {
            0.0
        };
        Self {
            paths,
            gain,
            out_filter: if in_loop {
                None
            } else {
                cutoff.map(OnePole::new)
            },
            diffusion: WASH_MS
                .iter()
                .map(|&ms| AllPass::new(samples(ms), WASH_FEEDBACK))
                .collect(),
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let mut sum = 0.0;
        for path in &mut self.paths {
            let input = match &mut path.predelay {
                Some((line, delay)) => {
                    let out = line.read(*delay);
                    line.write(x);
                    out
                }
                None => x,
            };
            sum += path.comb.process(input);
        }
        let mut tail = sum * self.gain;
        if let Some(filter) = &mut self.out_filter {
            tail = filter.process(tail);
        }
        x + self
            .diffusion
            .iter_mut()
            .fold(tail, |signal, stage| stage.process(signal))
    }
}

/// `DFR`: all-pass delays in series, smearing a sound into a wash.
///
/// Parameters: size (scales the 13-41 ms delays), number of delays,
/// feedback, gain.
#[derive(Debug, Clone)]
pub(crate) struct Diffusor {
    stages: Vec<AllPass>,
    gain: f32,
}

impl Diffusor {
    fn new(p: &[f32; MAX_PARAMS]) -> Self {
        let size = if p[0] > 0.0 { p[0] } else { 1.0 };
        let count = (p[1].max(1.0) as usize).min(DIFFUSOR_MS.len());
        Self {
            stages: DIFFUSOR_MS[..count]
                .iter()
                .map(|&ms| AllPass::new(samples(ms * size), p[2]))
                .collect(),
            gain: if p[3] > 0.0 { p[3] } else { 1.0 },
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.stages
            .iter_mut()
            .fold(x, |signal, stage| stage.process(signal))
            * self.gain
    }
}

/// `FLT`: a low-, high- or band-pass filter.
///
/// Parameters: type (`LP` 0, `HP` 1, `BP` 2), cutoff (Hz), band width
/// (Hz), quality (1-4 filters in series), gain.
#[derive(Debug, Clone)]
pub(crate) struct Filter {
    stages: Vec<Biquad>,
    gain: f32,
}

impl Filter {
    fn new(p: &[f32; MAX_PARAMS]) -> Self {
        let count = (p[3].clamp(0.0, 3.0) as usize) + 1;
        Self {
            stages: vec![Biquad::new(band(p[0]), p[1], p[2]); count],
            gain: if p[4] > 0.0 { p[4] } else { 1.0 },
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        self.stages
            .iter_mut()
            .fold(x, |signal, stage| stage.process(signal))
            * self.gain
    }
}

/// `AMP`: gain, with optional distortion.
///
/// Parameters: gain, distortion threshold (1: none), distortion mix, then
/// feedback and modulation, which are left out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Amplifier {
    gain: f32,
    threshold: f32,
    mix: f32,
}

impl Amplifier {
    fn new(p: &[f32; MAX_PARAMS]) -> Self {
        Self {
            gain: p[0],
            threshold: p[1],
            mix: p[2].clamp(0.0, 1.0),
        }
    }

    fn process(&self, x: f32) -> f32 {
        let y = x * self.gain;
        if self.threshold <= 0.0 || self.threshold >= 1.0 || self.mix == 0.0 {
            return y;
        }
        let clipped = y.clamp(-self.threshold, self.threshold);
        y + (clipped - y) * self.mix
    }
}

fn band(kind: f32) -> Band {
    match kind as u32 {
        1 => Band::High,
        2 => Band::Pass,
        _ => Band::Low,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(kind: ProcessorKind, values: &[f32]) -> ProcessorDef {
        let mut params = [0.0; MAX_PARAMS];
        params[..values.len()].copy_from_slice(values);
        ProcessorDef { kind, params }
    }

    /// Energy out over energy in for a burst of noise.
    fn energy_gain(processor: &mut Processor) -> f32 {
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 * 2.0 - 1.0
        };
        let burst = SAMPLE_RATE as usize / 2;
        let (mut input, mut output) = (0.0, 0.0);
        for n in 0..SAMPLE_RATE as usize * 4 {
            let x = if n < burst { noise() } else { 0.0 };
            input += x * x;
            output += processor.process(x).powi(2);
        }
        output / input
    }

    #[test]
    fn room_reverbs_are_about_as_loud_as_their_input() {
        // Presets 17 (concrete small), 2 (metal small) and 110 (hall).
        for values in [
            &[50.0, 20.0, 6.0, 0.9, 1.4, 4000.0, 1.0, 4.0, 3.48][..],
            &[80.0, 30.0, 4.0, 0.85, 1.1, 4000.0, 1.0],
            &[
                0.0, 0.0, 3.0, 0.0, 2.0, 6000.0, 1.0, 0.0, 0.0, 7.0, 20.0, 10.0, 0.85, -0.9, 0.3,
            ],
        ] {
            let mut reverb = Processor::new(&def(ProcessorKind::Reverb, values));
            let gain = energy_gain(&mut reverb);
            assert!((0.1..10.0).contains(&gain), "{values:?}: {gain}");
        }
    }

    #[test]
    fn an_outdoor_delay_passes_the_sound_then_echoes_it() {
        // Preset 20: DLY LOWPASS 300 ms, feedback 0.5, gain 0.84.
        let mut delay = Processor::new(&def(
            ProcessorKind::Delay,
            &[2.0, 300.0, 0.5, 0.84, 0.0, 2000.0],
        ));
        let echo = samples(300.0);
        let out: Vec<f32> = (0..echo * 3)
            .map(|n| delay.process(if n == 0 { 1.0 } else { 0.0 }))
            .collect();
        assert!((out[0] - 0.84).abs() < 1e-6, "the sound itself");
        assert!(out[1..echo].iter().all(|&y| y == 0.0), "then nothing");
        let first: f32 = out[echo..echo * 2].iter().sum();
        let second: f32 = out[echo * 2..].iter().sum();
        assert!((first - 0.42 * ECHO).abs() < 0.01, "a faint echo: {first}");
        assert!(second > 0.0 && second < first, "and fainter again");
    }

    #[test]
    fn diffusors_and_filters_keep_the_level() {
        let mut diffusor = Processor::new(&def(ProcessorKind::Diffusor, &[1.0, 3.0, 0.15]));
        let gain = energy_gain(&mut diffusor);
        assert!((gain - 1.0).abs() < 0.05, "{gain}");
        let mut low = Processor::new(&def(ProcessorKind::Filter, &[0.0, 3000.0, 0.0, 1.0, 1.0]));
        assert!(energy_gain(&mut low) < 0.5, "white noise loses its treble");
        let mut pass = Processor::new(&def(ProcessorKind::Pitch, &[1.1, 100.0, 20.0]));
        assert_eq!(pass.process(0.25), 0.25);
        let amp = Amplifier::new(&def(ProcessorKind::Amplifier, &[2.0, 0.5, 1.0]).params);
        assert_eq!(amp.process(0.5), 0.5, "clipped at the threshold");
    }
}
