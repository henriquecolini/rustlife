//! Soundscapes: the ambience of a place.
//!
//! A map's `env_soundscape` entities name a soundscape from
//! `scripts/soundscapes_manifest.txt`'s files; one at a time plays, the
//! nearest the listener can see within its radius. A soundscape is
//! KeyValues: sounds that loop for as long as it plays (`playlooping`),
//! sounds picked at random every so often (`playrandom`), and other
//! soundscapes mixed in at a volume (`playsoundscape`):
//!
//! ```text
//! "prison.control_room"
//! {
//!     "dsp" "1"
//!     "playlooping" { "volume" "0.4" "wave" "ambient/atmosphere/tone_alley.wav" }
//!     "playrandom"
//!     {
//!         "time" "5,15"  "volume" "0.2,0.4"  "pitch" "90,110"  "position" "random"
//!         "rndwave" { "wave" "ambient/machines/hum1.wav" "wave" "ambient/machines/hum2.wav" }
//!     }
//!     "playsoundscape" { "name" "prison.util_distantcombat_light" "volume" "0.5" }
//! }
//! ```
//!
//! Numbers are intervals (`low, high`, or one value), drawn afresh as
//! HL2DM's `C_SoundscapeSystem` draws them: a loop's once when the
//! soundscape starts, a random sound's at every play. [`Soundscapes::start`]
//! does what starting a soundscape does: it draws the loops' volumes,
//! pitches and levels, mixes the children in, and settles where each sound
//! plays — at the listener, at one of the `env_soundscape`'s `positionN`
//! targets, or (random sounds) around the listener — leaving out sounds
//! that are silent or name a position the entity does not set.

use std::collections::BTreeMap;

use source_fs::keyvalues::{self, Block, Pair};

use crate::soundscript::{SNDLVL_NORM, clean_wave, soundlevel};

/// A value drawn uniformly from `low..=high` (Source's `interval_t`).
pub type Interval = (f32, f32);

/// Deepest `playsoundscape` nesting HL2DM follows.
const MAX_RECURSION: u32 = 8;
/// Positions an `env_soundscape` can set (`localSound`'s bits).
const MAX_POSITIONS: i32 = 32;

/// How far a looping sound carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    /// A sound level, decibels.
    Decibels(Interval),
    /// An `attenuation`, a sound level of `20 / a + 50` decibels (0 for an
    /// attenuation of 0: heard everywhere).
    Attenuation(Interval),
}

/// A sound that loops while its soundscape plays.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopingSound {
    /// The wave, relative to `sound/`.
    pub wave: String,
    /// Volume, `0..=1`; without one the sound is not played.
    pub volume: Interval,
    /// Pitch, percent.
    pub pitch: Interval,
    /// How far it carries, when positional.
    pub level: Level,
    /// `position N`: at the soundscape entity's `positionN` (plus the
    /// enclosing soundscapes' offsets); `None` at the listener.
    pub position: Option<i32>,
}

/// Where a random sound plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomPosition {
    /// `position N`: at the soundscape entity's `positionN`.
    At(i32),
    /// `position random`: somewhere around the listener, anew every play.
    Random,
}

/// A sound played every so often while its soundscape plays.
#[derive(Debug, Clone, PartialEq)]
pub struct RandomSound {
    /// Waves to pick from (`rndwave`), relative to `sound/`.
    pub waves: Vec<String>,
    /// Seconds between plays.
    pub time: Interval,
    /// Volume, `0..=1`.
    pub volume: Interval,
    /// Pitch, percent; HL2DM plays nothing at pitch 0, its default.
    pub pitch: Interval,
    /// Sound level, decibels, when positional.
    pub soundlevel: Interval,
    /// Where it plays; `None` at the listener.
    pub position: Option<RandomPosition>,
}

/// Another soundscape mixed into one.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildScape {
    /// Its name, lower case.
    pub name: String,
    /// Its master volume, times the parent's.
    pub volume: Interval,
    /// Added to the positions its sounds name.
    pub position: Option<i32>,
    /// Puts every positional sound of it at this position.
    pub position_override: Option<i32>,
    /// Puts every ambient sound of it at this position.
    pub ambient_position_override: Option<i32>,
}

/// One soundscape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Soundscape {
    /// Its looping sounds.
    pub looping: Vec<LoopingSound>,
    /// Its random sounds.
    pub random: Vec<RandomSound>,
    /// Other soundscapes mixed in.
    pub children: Vec<ChildScape>,
}

/// A looping sound of a started soundscape, its values drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct StartedLoop {
    /// The wave, relative to `sound/`.
    pub wave: String,
    /// Volume, its parents' masters applied.
    pub volume: f32,
    /// Pitch, percent.
    pub pitch: f32,
    /// Sound level, decibels.
    pub soundlevel: f32,
    /// The position (an index into the entity's positions) it plays at;
    /// `None` at the listener.
    pub position: Option<usize>,
}

/// Where a started random sound plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomPlace {
    /// At the listener.
    Ambient,
    /// At one of the entity's positions.
    At(usize),
    /// Around the listener, anew every play.
    AroundListener,
}

/// A random sound of a started soundscape: drawn anew at every play.
#[derive(Debug, Clone, PartialEq)]
pub struct StartedRandom {
    /// The sound.
    pub sound: RandomSound,
    /// Its parents' master volume.
    pub master: f32,
    /// Where it plays.
    pub place: RandomPlace,
}

/// What a started soundscape plays.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Started {
    /// Its loops, children's included.
    pub looping: Vec<StartedLoop>,
    /// Its random sounds, children's included.
    pub random: Vec<StartedRandom>,
}

/// What a (sub-)soundscape starts with (`subsoundscapeparams_t`).
#[derive(Debug, Clone, Copy)]
struct Params {
    recursion: u32,
    master: f32,
    start: i32,
    position_override: i32,
    ambient_position_override: i32,
}

/// Every soundscape of a set of files, by lower-cased name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Soundscapes {
    scapes: BTreeMap<String, Soundscape>,
}

impl Soundscapes {
    /// Adds the soundscapes of one file's text; one already known keeps
    /// its first definition. Returns how many were added, or `None` if the
    /// text is not KeyValues.
    pub fn add_file(&mut self, text: &str) -> Option<usize> {
        let document = keyvalues::parse(text).ok()?;
        let mut added = 0;
        for pair in document.root.iter() {
            let Some(block) = pair.value.as_block() else {
                continue;
            };
            let name = pair.key.to_ascii_lowercase();
            if self.scapes.contains_key(&name) {
                continue;
            }
            self.scapes.insert(name, soundscape(block));
            added += 1;
        }
        Some(added)
    }

    /// The soundscape named `name` (any case).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Soundscape> {
        self.scapes.get(&name.to_ascii_lowercase())
    }

    /// How many soundscapes are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.scapes.len()
    }

    /// Whether none are known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scapes.is_empty()
    }

    /// Starts soundscape `name` as HL2DM does: every loop's volume, pitch
    /// and level drawn (`unit` gives numbers in `0..1`), children mixed in
    /// at their drawn volumes down to eight deep, and each sound placed.
    /// `positions` has bit N set when the entity sets `positionN`; a sound
    /// at a position it does not set is left out, as is a silent loop.
    #[must_use]
    pub fn start(&self, name: &str, positions: u32, unit: &mut dyn FnMut() -> f32) -> Started {
        let mut started = Started::default();
        let top = Params {
            recursion: 0,
            master: 1.0,
            start: 0,
            position_override: -1,
            ambient_position_override: -1,
        };
        if let Some(scape) = self.get(name) {
            self.start_into(scape, top, positions, unit, &mut started);
        }
        started
    }

    fn start_into(
        &self,
        scape: &Soundscape,
        params: Params,
        positions: u32,
        unit: &mut dyn FnMut() -> f32,
        started: &mut Started,
    ) {
        let is_set = |position: i32| {
            (0..MAX_POSITIONS).contains(&position) && positions & (1 << position) != 0
        };
        for sound in &scape.looping {
            let volume = draw(sound.volume, unit) * params.master;
            let pitch = draw(sound.pitch, unit).trunc();
            let soundlevel = sound.level.draw(unit);
            let mut position = sound.position.map_or(-1, |at| params.start + at);
            if position >= 0 && params.position_override >= 0 {
                position = params.position_override;
            }
            if position < 0 {
                position = params.ambient_position_override;
            }
            if volume == 0.0 || (position >= 0 && !is_set(position)) {
                continue;
            }
            started.looping.push(StartedLoop {
                wave: sound.wave.clone(),
                volume,
                pitch,
                soundlevel,
                position: usize::try_from(position).ok(),
            });
        }
        for sound in &scape.random {
            if let Some(place) = random_place(sound.position, params, is_set) {
                started.random.push(StartedRandom {
                    sound: sound.clone(),
                    master: params.master,
                    place,
                });
            }
        }
        for child in &scape.children {
            let recursion = params.recursion + 1;
            if recursion > MAX_RECURSION {
                break;
            }
            let Some(found) = self.get(&child.name) else {
                continue;
            };
            let sub = child.params(params, recursion, unit);
            self.start_into(found, sub, positions, unit, started);
        }
    }
}

impl ChildScape {
    /// The parameters this child starts with under `parent`'s.
    fn params(&self, parent: Params, recursion: u32, unit: &mut dyn FnMut() -> f32) -> Params {
        let mut sub = Params {
            recursion,
            master: draw(self.volume, unit) * parent.master,
            ..parent
        };
        if let Some(offset) = self.position {
            sub.start = parent.start + offset;
        }
        if let Some(at) = self.position_override
            && parent.position_override < 0
        {
            sub.position_override = parent.start + at;
            sub.ambient_position_override = parent.start + at;
        }
        if let Some(at) = self.ambient_position_override
            && parent.ambient_position_override < 0
        {
            sub.ambient_position_override = parent.start + at;
        }
        sub
    }
}

impl Level {
    /// A sound level, decibels, drawn from this.
    fn draw(self, unit: &mut dyn FnMut() -> f32) -> f32 {
        match self {
            Self::Decibels(level) => draw(level, unit).trunc(),
            // HL2DM draws once to see whether the attenuation is zero, then
            // again for the level.
            Self::Attenuation(attenuation) => {
                if draw(attenuation, unit) == 0.0 {
                    0.0
                } else {
                    attenuation_level(draw(attenuation, unit))
                }
            }
        }
    }
}

/// Where a random sound at `position` plays, under `params`; `None` for a
/// position the entity does not set.
fn random_place(
    position: Option<RandomPosition>,
    params: Params,
    is_set: impl Fn(i32) -> bool,
) -> Option<RandomPlace> {
    let mut random = position == Some(RandomPosition::Random);
    let mut at = match position {
        Some(RandomPosition::At(offset)) => params.start + offset,
        _ => -1,
    };
    if at >= 0 && params.position_override >= 0 {
        at = params.position_override;
        random = false;
    }
    if at < 0 {
        at = params.ambient_position_override;
    }
    if at < 0 && !random {
        Some(RandomPlace::Ambient)
    } else if random {
        Some(RandomPlace::AroundListener)
    } else {
        is_set(at)
            .then(|| usize::try_from(at).ok().map(RandomPlace::At))
            .flatten()
    }
}

/// A value from `interval`, `unit` giving a number in `0..1`
/// (`RandomInterval`).
#[must_use]
pub fn draw((low, high): Interval, unit: &mut dyn FnMut() -> f32) -> f32 {
    low + (high - low) * unit()
}

/// The sound level of an attenuation (`ATTN_TO_SNDLVL`).
#[must_use]
pub fn attenuation_level(attenuation: f32) -> f32 {
    if attenuation == 0.0 {
        0.0
    } else {
        (20.0 / attenuation + 50.0).trunc()
    }
}

/// The soundscape files a manifest lists (`file` keys), in order.
#[must_use]
pub fn manifest_files(text: &str) -> Vec<String> {
    let Ok(document) = keyvalues::parse(text) else {
        return Vec::new();
    };
    document
        .root
        .iter()
        .filter_map(|pair| pair.value.as_block())
        .flat_map(Block::iter)
        .filter(|pair| pair.key.eq_ignore_ascii_case("file"))
        .filter_map(|pair| pair.value.as_str())
        .map(|path| path.replace('\\', "/").to_ascii_lowercase())
        .collect()
}

fn soundscape(block: &Block) -> Soundscape {
    let mut scape = Soundscape::default();
    for pair in block.iter() {
        let Some(body) = pair.value.as_block() else {
            continue;
        };
        let key = pair.key.to_ascii_lowercase();
        match key.as_str() {
            "playlooping" => scape.looping.extend(looping(body)),
            "playrandom" => scape.random.extend(random(body)),
            "playsoundscape" => scape.children.extend(child(body)),
            _ => {}
        }
    }
    scape
}

/// The pairs of `body` whose value is text, keys lower-cased, in order:
/// later keys override earlier ones, as HL2DM reads them.
fn texts(body: &Block) -> impl Iterator<Item = (String, &str)> {
    body.iter()
        .filter_map(|pair: &Pair| Some((pair.key.to_ascii_lowercase(), pair.value.as_str()?)))
}

fn looping(body: &Block) -> Option<LoopingSound> {
    let mut sound = LoopingSound {
        wave: String::new(),
        volume: (0.0, 0.0),
        pitch: (100.0, 100.0),
        level: Level::Decibels((SNDLVL_NORM, SNDLVL_NORM)),
        position: None,
    };
    for (key, value) in texts(body) {
        match key.as_str() {
            "wave" => sound.wave = clean_wave(value),
            "volume" => sound.volume = interval(value),
            "pitch" => sound.pitch = interval(value),
            "position" => sound.position = Some(atoi(value)),
            "attenuation" => sound.level = Level::Attenuation(interval(value)),
            "soundlevel" => sound.level = Level::Decibels(level_interval(value)),
            _ => {}
        }
    }
    (!sound.wave.is_empty()).then_some(sound)
}

fn random(body: &Block) -> Option<RandomSound> {
    let mut sound = RandomSound {
        waves: Vec::new(),
        time: (0.0, 0.0),
        volume: (0.0, 0.0),
        pitch: (0.0, 0.0),
        soundlevel: (0.0, 0.0),
        position: None,
    };
    for (key, value) in texts(body) {
        match key.as_str() {
            "time" => sound.time = interval(value),
            "volume" => sound.volume = interval(value),
            "pitch" => sound.pitch = interval(value),
            "soundlevel" => sound.soundlevel = level_interval(value),
            "attenuation" => {
                let (low, high) = interval(value);
                sound.soundlevel = (attenuation_level(low), attenuation_level(high));
            }
            "position" if value.trim().eq_ignore_ascii_case("random") => {
                sound.position = Some(RandomPosition::Random);
            }
            "position" => sound.position = Some(RandomPosition::At(atoi(value))),
            _ => {}
        }
    }
    if let Some(waves) = body.get_block("rndwave") {
        sound.waves = waves
            .get_all("wave")
            .filter_map(|value| value.as_str())
            .map(clean_wave)
            .filter(|wave| !wave.is_empty())
            .collect();
    }
    (!sound.waves.is_empty()).then_some(sound)
}

fn child(body: &Block) -> Option<ChildScape> {
    let mut child = ChildScape {
        name: String::new(),
        volume: (1.0, 1.0),
        position: None,
        position_override: None,
        ambient_position_override: None,
    };
    for (key, value) in texts(body) {
        match key.as_str() {
            "name" => child.name = value.to_ascii_lowercase(),
            "volume" => child.volume = interval(value),
            "position" => child.position = Some(atoi(value)),
            "positionoverride" => child.position_override = Some(atoi(value)),
            "ambientpositionoverride" => child.ambient_position_override = Some(atoi(value)),
            _ => {}
        }
    }
    (!child.name.is_empty()).then_some(child)
}

/// `value` or `low, high` (`ParseInterval`); what does not read as a
/// number is 0, as `atof` has it.
fn interval(text: &str) -> Interval {
    let value = |part: &str| {
        part.trim()
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(0.0)
    };
    let (low, high) = text.split_once(',').unwrap_or((text, text));
    (value(low), value(high))
}

/// A sound level: a `SNDLVL_` name, else a decibel interval.
fn level_interval(text: &str) -> Interval {
    if text.trim().to_ascii_uppercase().starts_with("SNDLVL_") {
        let level = soundlevel(text).unwrap_or(0.0);
        (level, level)
    } else {
        interval(text)
    }
}

/// The integer `text` starts with, else 0 (`atoi`).
fn atoi(text: &str) -> i32 {
    let text = text.trim_start();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end].parse::<i32>().map_or(0, |value| sign * value)
}

#[cfg(test)]
mod tests;
