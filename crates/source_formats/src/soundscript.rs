//! Soundscripts: named sounds with their waves, volume, pitch and reach.
//!
//! Games refer to sounds by name (`Weapon_Shotgun.Single`) rather than by
//! file. `scripts/game_sounds_manifest.txt` lists the soundscript files,
//! and each file is KeyValues: one block per sound, giving a wave (or a
//! `rndwave` block of waves to pick from), a volume and a pitch (each a
//! value or a `low, high` range) and a sound level, the loudness that
//! decides how far the sound carries:
//!
//! ```text
//! "Weapon_Shotgun.Single"
//! {
//!     "channel"    "CHAN_WEAPON"
//!     "volume"     "0.9, 1.0"
//!     "soundlevel" "SNDLVL_GUNFIRE"
//!     "pitch"      "PITCH_NORM"
//!     "wave"       ")weapons/shotgun/shotgun_fire6.wav"
//! }
//! ```
//!
//! Wave paths are relative to `sound/` and may start with characters that
//! tell the engine's mixer how to play them (`)` spatial stereo, `^`
//! distance-variant, `*` streamed, `#` no effects, `@` everywhere...);
//! [`SoundEntry::waves`] has them stripped.

use std::collections::BTreeMap;

use source_fs::keyvalues::{self, Block};

/// Characters that may lead a wave path as mixer hints.
const WAVE_PREFIXES: &[char] = &[
    '*', '#', '@', '>', '<', '^', ')', '(', '}', '$', '!', '?', '&', '~', '`', '+', '%',
];

/// Volume of `VOL_NORM`.
const VOL_NORM: f32 = 1.0;
/// Pitch of `PITCH_NORM`, percent.
const PITCH_NORM: f32 = 100.0;
/// Sound level of `SNDLVL_NORM`, decibels.
pub const SNDLVL_NORM: f32 = 75.0;

/// One named sound.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundEntry {
    /// Wave files to pick from, relative to `sound/`, prefixes stripped.
    pub waves: Vec<String>,
    /// Volume range, `0..=1`.
    pub volume: (f32, f32),
    /// Pitch range, percent (100 plays at the recorded pitch).
    pub pitch: (f32, f32),
    /// Sound level, decibels (0: heard everywhere).
    pub soundlevel: f32,
}

/// Every named sound of a set of soundscripts, by lower-cased name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SoundScripts {
    entries: BTreeMap<String, SoundEntry>,
}

impl SoundScripts {
    /// Adds the sounds of one soundscript file's text; a sound already
    /// known keeps its first definition, as the engine keeps the first it
    /// loads. Returns how many sounds were added, or `None` if the text is
    /// not KeyValues.
    pub fn add_script(&mut self, text: &str) -> Option<usize> {
        let document = keyvalues::parse(text).ok()?;
        let mut added = 0;
        for pair in document.root.iter() {
            let Some(block) = pair.value.as_block() else {
                continue;
            };
            let name = pair.key.to_ascii_lowercase();
            if self.entries.contains_key(&name) {
                continue;
            }
            if let Some(entry) = entry(block) {
                self.entries.insert(name, entry);
                added += 1;
            }
        }
        Some(added)
    }

    /// The sound named `name` (any case).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SoundEntry> {
        self.entries.get(&name.to_ascii_lowercase())
    }

    /// How many sounds are known.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no sound is known.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The soundscript files a manifest lists (`precache_file` and
/// `preload_file`), in order.
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
        .filter(|pair| {
            pair.key.eq_ignore_ascii_case("precache_file")
                || pair.key.eq_ignore_ascii_case("preload_file")
        })
        .filter_map(|pair| pair.value.as_str())
        .map(|path| path.replace('\\', "/").to_ascii_lowercase())
        .collect()
}

/// A wave path without its mixer prefixes, `/`-separated and lower case.
#[must_use]
pub fn clean_wave(path: &str) -> String {
    path.trim_start_matches(WAVE_PREFIXES)
        .replace('\\', "/")
        .to_ascii_lowercase()
}

/// How a sound fades with distance: the factor Source multiplies distance
/// by (`SNDLVL_TO_ATTN`, over the engine's nominal clip distance of 1000
/// units) so that the gain is `1 / (distance × factor)`, at most 1. Zero
/// for sounds heard everywhere at full volume.
#[must_use]
pub fn distance_factor(soundlevel: f32) -> f32 {
    if soundlevel <= 0.0 {
        return 0.0;
    }
    let attenuation = if soundlevel > 50.0 {
        20.0 / (soundlevel - 50.0)
    } else {
        4.0
    };
    attenuation / 1000.0
}

fn entry(block: &Block) -> Option<SoundEntry> {
    let mut waves: Vec<String> = block
        .get_all("wave")
        .filter_map(|value| value.as_str())
        .map(clean_wave)
        .collect();
    if let Some(random) = block.get_block("rndwave") {
        waves.extend(
            random
                .get_all("wave")
                .filter_map(|value| value.as_str())
                .map(clean_wave),
        );
    }
    waves.retain(|wave| !wave.is_empty());
    if waves.is_empty() {
        return None;
    }
    let range = |key: &str, named: fn(&str) -> Option<f32>, default: f32| {
        block
            .get_str(key)
            .and_then(|text| parse_range(text, named))
            .unwrap_or((default, default))
    };
    Some(SoundEntry {
        waves,
        volume: range("volume", volume_name, VOL_NORM),
        pitch: range("pitch", pitch_name, PITCH_NORM),
        soundlevel: block
            .get_str("soundlevel")
            .or_else(|| block.get_str("attenuation").map(|_| "SNDLVL_NORM"))
            .and_then(soundlevel)
            .unwrap_or(SNDLVL_NORM),
    })
}

/// `value` or `low, high`, each a number or a name `named` knows.
fn parse_range(text: &str, named: fn(&str) -> Option<f32>) -> Option<(f32, f32)> {
    let value = |part: &str| {
        let part = part.trim();
        part.parse::<f32>()
            .ok()
            .filter(|value| value.is_finite())
            .or_else(|| named(part))
    };
    match text.split_once(',') {
        Some((low, high)) => {
            let (low, high) = (value(low)?, value(high)?);
            Some((low.min(high), low.max(high)))
        }
        None => value(text).map(|value| (value, value)),
    }
}

fn volume_name(name: &str) -> Option<f32> {
    name.eq_ignore_ascii_case("VOL_NORM").then_some(VOL_NORM)
}

fn pitch_name(name: &str) -> Option<f32> {
    match name.to_ascii_uppercase().as_str() {
        "PITCH_NORM" => Some(PITCH_NORM),
        "PITCH_LOW" => Some(95.0),
        "PITCH_HIGH" => Some(120.0),
        _ => None,
    }
}

/// A sound level: decibels, or one of the engine's names for them.
pub(crate) fn soundlevel(text: &str) -> Option<f32> {
    let text = text.trim();
    if let Ok(value) = text.parse::<f32>() {
        return value.is_finite().then_some(value);
    }
    let name = text.to_ascii_uppercase();
    let named = match name.as_str() {
        "SNDLVL_NONE" => 0.0,
        "SNDLVL_NORM" => SNDLVL_NORM,
        "SNDLVL_IDLE" | "SNDLVL_TALKING" => 60.0,
        "SNDLVL_STATIC" => 66.0,
        "SNDLVL_STATEMENT" => 70.0,
        "SNDLVL_SCREAMING" => 90.0,
        "SNDLVL_GUNFIRE" => 140.0,
        _ => {
            return name
                .strip_prefix("SNDLVL_")?
                .strip_suffix("DB")?
                .parse::<f32>()
                .ok();
        }
    };
    Some(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"
        "Weapon_Shotgun.Single"
        {
            "channel"    "CHAN_WEAPON"
            "volume"     "0.9, 1.0"
            "soundlevel" "SNDLVL_GUNFIRE"
            "pitch"      "PITCH_NORM"
            "wave"       ")weapons\shotgun\Shotgun_Fire6.wav"
        }
        "FX_RicochetSound.Ricochet"
        {
            "volume"     "VOL_NORM"
            "soundlevel" "SNDLVL_80dB"
            "pitch"      "110, 90"
            "rndwave"
            {
                "wave" "weapons/fx/rics/ric1.wav"
                "wave" "^weapons/fx/rics/ric2.wav"
            }
        }
        "Silent.Nothing" { "volume" "1" }
    "#;

    #[test]
    fn entries_read_their_waves_ranges_and_levels() {
        let mut scripts = SoundScripts::default();
        assert_eq!(
            scripts.add_script(SCRIPT),
            Some(2),
            "a sound without waves is no sound"
        );
        let shotgun = scripts.get("weapon_shotgun.single").expect("shotgun");
        assert_eq!(shotgun.waves, ["weapons/shotgun/shotgun_fire6.wav"]);
        assert_eq!(
            (shotgun.volume, shotgun.pitch, shotgun.soundlevel),
            ((0.9, 1.0), (100.0, 100.0), 140.0)
        );
        let ricochet = scripts.get("FX_RicochetSound.Ricochet").expect("ricochet");
        assert_eq!(
            ricochet.waves,
            ["weapons/fx/rics/ric1.wav", "weapons/fx/rics/ric2.wav"]
        );
        assert_eq!((ricochet.pitch, ricochet.soundlevel), ((90.0, 110.0), 80.0));
        assert!(scripts.get("Silent.Nothing").is_none());
    }

    #[test]
    fn the_first_definition_of_a_sound_wins() {
        let mut scripts = SoundScripts::default();
        scripts.add_script(SCRIPT);
        let again = r#""Weapon_Shotgun.Single" { "wave" "other.wav" }"#;
        assert_eq!(scripts.add_script(again), Some(0));
        assert_eq!(
            scripts.get("weapon_shotgun.single").expect("kept").waves[0],
            "weapons/shotgun/shotgun_fire6.wav"
        );
        assert_eq!(scripts.len(), 2);
    }

    #[test]
    fn manifests_list_their_files() {
        let manifest = r#"game_sounds_manifest
        {
            "precache_file" "scripts/game_sounds.txt"
            "preload_file"  "scripts\Game_Sounds_UI.txt"
        }"#;
        assert_eq!(
            manifest_files(manifest),
            ["scripts/game_sounds.txt", "scripts/game_sounds_ui.txt"]
        );
        assert!(manifest_files("{").is_empty());
    }

    #[test]
    fn louder_sounds_carry_further() {
        assert_eq!(distance_factor(0.0), 0.0, "heard everywhere");
        assert!(distance_factor(140.0) < distance_factor(75.0));
        // SNDLVL_NORM: full volume out to 1250 units, as in Source.
        assert!((1.0 / distance_factor(75.0) - 1250.0).abs() < 1e-3);
        assert_eq!(soundlevel("SNDLVL_95dB"), Some(95.0));
        assert_eq!(soundlevel("garbage"), None);
    }
}
