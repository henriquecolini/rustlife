//! `scripts/dsp_presets.txt`: the room effects of Source's sound mixer.
//!
//! A preset is a chain of up to five processors (delays, reverbs,
//! filters, diffusors, amplifiers...) that the mixer runs the "room" part
//! of every sound through. Presets 0-29 are the classic room types a
//! soundscape's `dsp` names (2 small metal room, 17 small concrete room,
//! 20 small outside...), 30-59 special effects, and 100 up the templates
//! the automatic room effect blends between.
//!
//! ```text
//! //  #  ptype   mixrng   dur  fade dbmin mixdrop
//! {   17 LINEAR  0.2 0.7  0.0  0.0  80    0.5
//!     // DFR size #dly feedbk
//!     {  DFR 1.0  2    0.15 }
//!     // RVA size min #dly feedbk gain cutoff fpar fmod rate w d h fw fd fh ftap
//!     {  RVA 50.0 20.0 6   0.9    1.4  4000   1    4    3.48 0 0 0 0  0  0  0 }
//! }
//! ```
//!
//! The text is read as HL2DM's `DSP_LoadPresetFile` reads it: tokens are
//! numbers or names from one table (`RVA` 2, `LOWPASS` 2, `LINEAR` 1...,
//! whatever slot they are in), `//` starts a comment, braces stand alone
//! even when glued to a number, a processor keeps at most 16 parameters
//! and a preset at most 5 processors, and a preset of one processor is
//! run as `SIMPLE` whatever type it names.

use tracing::warn;

/// Most processors a preset holds.
pub const MAX_PROCESSORS: usize = 5;
/// Most parameters a processor takes.
pub const MAX_PARAMS: usize = 16;

/// Names the file may use for numbers, and their values, as the engine's
/// own table has them (`engine.so`, beside `dsp_presets.txt`'s loader).
const TOKENS: &[(&str, f32)] = &[
    // Processor types.
    ("NULL", 0.0),
    ("DLY", 1.0),
    ("RVA", 2.0),
    ("FLT", 3.0),
    ("CRS", 4.0),
    ("PTC", 5.0),
    ("ENV", 6.0),
    ("LFO", 7.0),
    ("EFO", 8.0),
    ("MDY", 9.0),
    ("DFR", 10.0),
    ("AMP", 11.0),
    // Filter types and qualities.
    ("LP", 0.0),
    ("HP", 1.0),
    ("BP", 2.0),
    ("LO", 0.0),
    ("MED", 1.0),
    ("HI", 2.0),
    ("VHI", 3.0),
    // Delay types.
    ("PLAIN", 0.0),
    ("ALLPASS", 1.0),
    ("LOWPASS", 2.0),
    ("DLINEAR", 3.0),
    ("FLINEAR", 4.0),
    ("LOWPASS_4TAP", 5.0),
    ("PLAIN_4TAP", 6.0),
    // LFO and envelope shapes.
    ("SIN", 0.0),
    ("TRI", 1.0),
    ("SQR", 2.0),
    ("SAW", 3.0),
    ("RND", 4.0),
    ("LOG_IN", 5.0),
    ("LOG_OUT", 6.0),
    ("LIN_IN", 7.0),
    ("LIN_OUT", 8.0),
    ("LIN", 0.0),
    ("EXP", 1.0),
    // How a preset chains its processors.
    ("SIMPLE", 0.0),
    ("LINEAR", 1.0),
    ("PARALLEL2", 5.0),
    ("PARALLEL4", 6.0),
    ("PARALLEL5", 7.0),
    ("FEEDBACK", 8.0),
    ("FEEDBACK3", 9.0),
    ("FEEDBACK4", 10.0),
    ("MOD1", 11.0),
    ("MOD2", 12.0),
    ("MOD3", 13.0),
];

/// What a processor does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorKind {
    /// Passes its input through.
    Null,
    /// A delay, plain or with feedback, filtered or not.
    Delay,
    /// Parallel reverberating delays: a room.
    Reverb,
    /// A low-, high- or band-pass filter.
    Filter,
    /// Chorus.
    Chorus,
    /// Pitch shifter.
    Pitch,
    /// Envelope.
    Envelope,
    /// Low-frequency oscillator.
    Lfo,
    /// Envelope follower or gate.
    EnvelopeFollower,
    /// Delay whose length wanders.
    ModDelay,
    /// Series all-pass delays that smear a sound.
    Diffusor,
    /// Amplifier, with distortion.
    Amplifier,
}

impl ProcessorKind {
    /// The kind numbered `number` in the file (`DLY` is 1...); `None` for
    /// numbers the engine does not know.
    #[must_use]
    pub fn from_number(number: u32) -> Option<Self> {
        Some(match number {
            0 => Self::Null,
            1 => Self::Delay,
            2 => Self::Reverb,
            3 => Self::Filter,
            4 => Self::Chorus,
            5 => Self::Pitch,
            6 => Self::Envelope,
            7 => Self::Lfo,
            8 => Self::EnvelopeFollower,
            9 => Self::ModDelay,
            10 => Self::Diffusor,
            11 => Self::Amplifier,
            _ => return None,
        })
    }

    /// Its number in the file.
    #[must_use]
    pub fn number(self) -> u32 {
        self as u32
    }
}

/// How a preset wires its processors together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chain {
    /// The first processor alone.
    Simple,
    /// Each processor feeds the next.
    Linear,
    /// `P0 + P1`.
    Parallel2,
    /// `P0→P1 + P2→P3`.
    Parallel4,
    /// `(P0→P1 + P2→P3) → P4`.
    Parallel5,
    /// A wiring with feedback or modulation (`FEEDBACK*`, `MOD*`), which no
    /// room preset uses.
    Other(u32),
}

impl Chain {
    fn from_number(number: u32) -> Self {
        match number {
            0 => Self::Simple,
            1 => Self::Linear,
            5 => Self::Parallel2,
            6 => Self::Parallel4,
            7 => Self::Parallel5,
            other => Self::Other(other),
        }
    }
}

/// One processor of a preset: its kind and its parameters, in the order
/// the file gives them (missing ones are zero).
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessorDef {
    /// What it does.
    pub kind: ProcessorKind,
    /// Its parameters.
    pub params: [f32; MAX_PARAMS],
}

/// One preset.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetDef {
    /// How its processors are wired.
    pub chain: Chain,
    /// Room-effect share of a sound next to the listener.
    pub mix_min: f32,
    /// Room-effect share of a sound far away (`dsp_dist_max`) .
    pub mix_max: f32,
    /// Seconds it lasts before the previous preset returns; 0 for good.
    pub duration: f32,
    /// Seconds it fades out over when it has a duration.
    pub fade: f32,
    /// Sounds quieter than this (decibels) get less of the effect...
    pub db_min: f32,
    /// ...their share times this.
    pub db_mixdrop: f32,
    /// Its processors, in file order.
    pub processors: Vec<ProcessorDef>,
}

impl PresetDef {
    /// The `index`th processor of `kind`, if the preset has that many.
    #[must_use]
    pub fn nth(&self, kind: ProcessorKind, index: usize) -> Option<&ProcessorDef> {
        self.processors
            .iter()
            .filter(|processor| processor.kind == kind)
            .nth(index)
    }

    /// The same, to change.
    pub fn nth_mut(&mut self, kind: ProcessorKind, index: usize) -> Option<&mut ProcessorDef> {
        self.processors
            .iter_mut()
            .filter(|processor| processor.kind == kind)
            .nth(index)
    }
}

/// Every preset of a `dsp_presets.txt`, by number.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DspPresets {
    presets: Vec<Option<PresetDef>>,
}

impl DspPresets {
    /// Reads the file. What cannot be read is left out, with a warning, as
    /// the engine skips it.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut tokens = Tokens::new(text);
        let mut presets: Vec<Option<PresetDef>> = Vec::new();
        while let Some(token) = tokens.next() {
            if token != "{" {
                continue;
            }
            let Some((number, preset)) = preset(&mut tokens) else {
                warn!("dsp_presets.txt: a preset ends early");
                break;
            };
            let Ok(number) = usize::try_from(number) else {
                continue;
            };
            if presets.len() <= number {
                presets.resize(number + 1, None);
            }
            presets[number] = Some(preset);
        }
        Self { presets }
    }

    /// Preset `number`, if the file defines it.
    #[must_use]
    pub fn get(&self, number: usize) -> Option<&PresetDef> {
        self.presets.get(number)?.as_ref()
    }

    /// How many preset numbers there are (the highest plus one).
    #[must_use]
    pub fn len(&self) -> usize {
        self.presets.len()
    }

    /// Whether the file defined no preset.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presets.iter().all(Option::is_none)
    }
}

/// The rest of a preset after its `{`: its number and definition.
fn preset(tokens: &mut Tokens<'_>) -> Option<(i64, PresetDef)> {
    let number = value(tokens.next()?) as i64;
    let chain = value(tokens.next()?) as u32;
    let mut header = [0.0; 6];
    for slot in &mut header {
        *slot = value(tokens.next()?);
    }
    let mut processors = Vec::new();
    loop {
        match tokens.next()? {
            "}" => break,
            "{" => {
                let (kind, params) = processor(tokens)?;
                if processors.len() < MAX_PROCESSORS {
                    processors.extend(kind.map(|kind| ProcessorDef { kind, params }));
                } else {
                    warn!("dsp_presets.txt: preset {number} has too many processors");
                }
            }
            _ => {}
        }
    }
    let chain = if processors.len() == 1 {
        Chain::Simple
    } else {
        Chain::from_number(chain)
    };
    let [mix_min, mix_max, duration, fade, db_min, db_mixdrop] = header;
    Some((
        number,
        PresetDef {
            chain,
            mix_min,
            mix_max,
            duration,
            fade,
            db_min,
            db_mixdrop,
            processors,
        },
    ))
}

/// The rest of a processor after its `{`: its kind (`None` when unknown:
/// it is skipped) and parameters.
fn processor(tokens: &mut Tokens<'_>) -> Option<(Option<ProcessorKind>, [f32; MAX_PARAMS])> {
    let kind = value(tokens.next()?);
    let mut params = [0.0; MAX_PARAMS];
    let mut count = 0;
    loop {
        let token = tokens.next()?;
        if token == "}" {
            break;
        }
        if count < MAX_PARAMS {
            params[count] = value(token);
            count += 1;
        }
    }
    let kind = ProcessorKind::from_number(kind as u32);
    if kind.is_none() {
        warn!("dsp_presets.txt: unknown processor type");
    }
    Some((kind, params))
}

/// A number, or a name's value; 0 (with a warning) for anything else.
fn value(token: &str) -> f32 {
    if let Ok(number) = token.parse::<f32>() {
        return if number.is_finite() { number } else { 0.0 };
    }
    TOKENS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(token))
        .map_or_else(
            || {
                warn!("dsp_presets.txt: unknown token {token:?}");
                0.0
            },
            |&(_, value)| value,
        )
}

/// The file's tokens: braces, and runs of anything else between
/// whitespace, with `//` comments left out.
struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    fn new(text: &'a str) -> Self {
        Self { rest: text }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        loop {
            self.rest = self.rest.trim_start();
            if let Some(comment) = self.rest.strip_prefix("//") {
                self.rest = comment.split_once('\n').map_or("", |(_, rest)| rest);
                continue;
            }
            break;
        }
        let first = self.rest.chars().next()?;
        let len = if matches!(first, '{' | '}') {
            1
        } else {
            self.rest
                .find(|c: char| c.is_whitespace() || matches!(c, '{' | '}'))
                .unwrap_or(self.rest.len())
        };
        let (token, rest) = self.rest.split_at(len);
        self.rest = rest;
        Some(token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "
        // PRESET DEFINITIONS: { <preset#> <type> ... }
        {   0   LINEAR  0.2 0.7 0.0 0.0 80 0.5
                {0  0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 }
        }
        {   17  LINEAR  0.2 0.7 0.0 0.0 80 0.5
                // DFR size #dly feedbk
                {  DFR  1.0 2 0.15 }
                {  RVA  50.0 20.0 6 0.9 1.4 4000 1 4 3.48 0 0 0 0 0 0 0 }
        }
        {   20  LINEAR  0.2 0.7 0.0  0.0  80 0.5
                {  DLY LOWPASS 300.0 0.5 0.84 LP 2000 0 LO 0 0 0}
        }
        {   44  LINEAR  0.2 0.7 0 0 0 0{  PTC 1.1 100.0 20.0 } }
        {   126 LINEAR 0.21 0.8 0.0 0.0 95 0.1
                { DLY LOWPASS -1.0 0.5 1.2 LP 4000 0 LO -1 -1 -1}
        }
    ";

    #[test]
    fn presets_read_by_number_with_their_processors() {
        let presets = DspPresets::parse(FILE);
        assert_eq!(presets.len(), 127);
        let concrete = presets.get(17).expect("preset 17");
        assert_eq!(concrete.chain, Chain::Linear);
        assert_eq!((concrete.mix_min, concrete.mix_max), (0.2, 0.7));
        assert_eq!((concrete.db_min, concrete.db_mixdrop), (80.0, 0.5));
        assert_eq!(concrete.processors.len(), 2);
        assert_eq!(concrete.processors[0].kind, ProcessorKind::Diffusor);
        assert_eq!(concrete.processors[0].params[..3], [1.0, 2.0, 0.15]);
        let reverb = concrete.nth(ProcessorKind::Reverb, 0).expect("RVA");
        assert_eq!(reverb.params[..6], [50.0, 20.0, 6.0, 0.9, 1.4, 4000.0]);
        assert!(presets.get(18).is_none());
    }

    #[test]
    fn names_are_numbers_wherever_they_appear() {
        let presets = DspPresets::parse(FILE);
        let outside = presets.get(20).expect("preset 20");
        assert_eq!(outside.chain, Chain::Simple, "one processor runs alone");
        let delay = &outside.processors[0];
        assert_eq!(delay.kind, ProcessorKind::Delay);
        // LOWPASS 2, LP 0, LO 0.
        assert_eq!(
            delay.params[..8],
            [2.0, 300.0, 0.5, 0.84, 0.0, 2000.0, 0.0, 0.0]
        );
        let courtyard = presets.get(126).expect("preset 126");
        assert_eq!(courtyard.processors[0].params[1], -1.0);
    }

    #[test]
    fn braces_split_from_glued_numbers() {
        let presets = DspPresets::parse(FILE);
        let pitch = presets.get(44).expect("preset 44");
        assert_eq!(pitch.processors[0].kind, ProcessorKind::Pitch);
        assert_eq!(pitch.processors[0].params[..3], [1.1, 100.0, 20.0]);
        let null = presets.get(0).expect("preset 0");
        assert_eq!(null.processors[0].kind, ProcessorKind::Null);
    }

    #[test]
    fn broken_text_is_survived() {
        assert!(DspPresets::parse("").is_empty());
        assert!(DspPresets::parse("{ 3 LINEAR 0.2").is_empty());
        let unknown = DspPresets::parse("{ 1 LINEAR 0 0 0 0 0 0 { 99 1 2 } { DFR 1 2 3 } }");
        let preset = unknown.get(1).expect("preset 1");
        assert_eq!(
            preset.processors.len(),
            1,
            "the unknown processor is skipped"
        );
        assert_eq!(value("BOGUS"), 0.0);
        assert_eq!(value("nan"), 0.0);
    }
}
