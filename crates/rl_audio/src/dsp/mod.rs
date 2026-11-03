//! Room effects: the share of each sound that is sent to "the room" runs
//! through the current `dsp_presets.txt` preset, giving the echo and
//! reverberation of the place the listener is in.
//!
//! - [`Preset`] runs one preset's processors on a stereo signal.
//! - [`RoomEffect`] holds the current one and cross-fades to the next.
//! - [`room`] builds the automatic preset (`dsp` 1) from the shape of the
//!   place around the listener.

pub mod room;

mod processor;
mod units;

use source_formats::dsp_presets::{Chain, PresetDef};

use self::processor::Processor;
use crate::SAMPLE_RATE;

/// Seconds a change of room cross-fades over.
pub const CROSSFADE_SECS: f32 = 1.0;

/// One preset, running: the same processors on each channel.
#[derive(Debug, Clone)]
pub struct Preset {
    def: PresetDef,
    channels: [Vec<Processor>; 2],
}

impl Preset {
    /// Starts running `def`.
    #[must_use]
    pub fn new(def: PresetDef) -> Self {
        let processors: Vec<Processor> = def.processors.iter().map(Processor::new).collect();
        Self {
            channels: [processors.clone(), processors],
            def,
        }
    }

    /// What it is running.
    #[must_use]
    pub fn def(&self) -> &PresetDef {
        &self.def
    }

    /// Runs one stereo frame through it.
    pub fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        let chain = self.def.chain;
        let [left, right] = &mut self.channels;
        [run(chain, left, frame[0]), run(chain, right, frame[1])]
    }
}

/// One sample through processors wired as `chain`.
fn run(chain: Chain, processors: &mut [Processor], x: f32) -> f32 {
    let all = processors.len();
    let mut series = |range: std::ops::Range<usize>, x: f32| {
        processors
            .get_mut(range)
            .map_or(x, |stages| stages.iter_mut().fold(x, |y, p| p.process(y)))
    };
    match chain {
        Chain::Simple => series(0..1, x),
        Chain::Parallel2 => series(0..1, x) + series(1..2, x),
        Chain::Parallel4 => series(0..2, x) + series(2..4, x),
        Chain::Parallel5 => {
            let both = series(0..2, x) + series(2..4, x);
            series(4..5, both)
        }
        // Feedback and modulation wirings are only used by special effects;
        // run them in series.
        Chain::Linear | Chain::Other(_) => series(0..all, x),
    }
}

/// The room effect playing: the current preset, and the previous one
/// fading out after a change.
#[derive(Debug, Default)]
pub struct RoomEffect {
    current: Option<Preset>,
    /// The previous preset (`None`: no effect) and its level, falling from
    /// 1 to 0.
    fading: Option<(Option<Preset>, f32)>,
}

impl RoomEffect {
    /// Switches to `preset` (`None`: no effect), cross-fading from the
    /// current one.
    pub fn set(&mut self, preset: Option<PresetDef>) {
        let old = self.current.take();
        self.current = preset.map(Preset::new);
        self.fading = Some((old, 1.0));
    }

    /// The preset playing, if any.
    #[must_use]
    pub fn current(&self) -> Option<&PresetDef> {
        self.current.as_ref().map(Preset::def)
    }

    /// Runs the room's share of a block of frames through the effect, in
    /// place.
    pub fn process(&mut self, frames: &mut [[f32; 2]]) {
        let step = 1.0 / (CROSSFADE_SECS * SAMPLE_RATE as f32);
        for frame in frames.iter_mut() {
            let input = *frame;
            let mut out = match &mut self.current {
                Some(preset) => preset.process(input),
                None => [0.0; 2],
            };
            if let Some((old, level)) = &mut self.fading {
                let from = old.as_mut().map_or([0.0; 2], |old| old.process(input));
                *level -= step;
                let new_level = 1.0 - level.max(0.0);
                for (out, from) in out.iter_mut().zip(from) {
                    *out = *out * new_level + from * level.max(0.0);
                }
            }
            *frame = out;
            if self.fading.as_ref().is_some_and(|(_, level)| *level <= 0.0) {
                self.fading = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use source_formats::dsp_presets::DspPresets;

    use super::*;

    const FILE: &str = "
        { 0 LINEAR 0.2 0.7 0 0 80 0.5 { 0 0 0 } }
        { 20 LINEAR 0.2 0.7 0 0 80 0.5
            { DFR 1.0 2 0.15 }
            { DLY LOWPASS 300.0 0.5 0.84 LP 2000 0 LO 0 0 0 } }
        { 5 PARALLEL2 0.2 0.7 0 0 80 0.5 { AMP 1 } { AMP 2 } }
    ";

    #[test]
    fn the_null_preset_passes_sound_through_and_rooms_echo() {
        let presets = DspPresets::parse(FILE);
        let mut null = Preset::new(presets.get(0).expect("0").clone());
        assert_eq!(null.process([0.5, -0.25]), [0.5, -0.25]);
        let mut outside = Preset::new(presets.get(20).expect("20").clone());
        let out: Vec<[f32; 2]> = (0..SAMPLE_RATE as usize / 2)
            .map(|n| outside.process(if n == 0 { [1.0, 1.0] } else { [0.0; 2] }))
            .collect();
        let delay = units::samples(290.0);
        let early: f32 = out[..delay / 2].iter().map(|f| f[0].abs()).sum();
        let echo: f32 = out[delay..].iter().map(|f| f[0].abs()).sum();
        assert!(early > 0.5, "the sound passes: {early}");
        assert!(echo > 0.1, "and echoes after the delay: {echo}");
        assert!(
            out.iter().all(|f| (f[0] - f[1]).abs() < 1e-9),
            "both channels alike"
        );
    }

    #[test]
    fn parallel_presets_add_their_branches() {
        let presets = DspPresets::parse(FILE);
        let mut parallel = Preset::new(presets.get(5).expect("5").clone());
        assert_eq!(parallel.process([1.0, 0.5]), [3.0, 1.5]);
    }

    #[test]
    fn a_change_of_room_crossfades() {
        let presets = DspPresets::parse(FILE);
        let mut room = RoomEffect::default();
        let mut block = [[1.0; 2]; 4];
        room.process(&mut block);
        assert_eq!(block, [[0.0; 2]; 4], "no room, no effect");
        room.set(presets.get(0).cloned());
        let mut block = vec![[1.0; 2]; SAMPLE_RATE as usize * 2];
        room.process(&mut block);
        assert!(block[0][0] < 0.01, "fades in");
        assert!((block[block.len() - 1][0] - 1.0).abs() < 1e-6, "then full");
        assert!(room.current().is_some());
        room.set(None);
        let mut block = vec![[1.0; 2]; 8];
        room.process(&mut block);
        assert!(block[0][0] > 0.99, "fading out");
    }
}
