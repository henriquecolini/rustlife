//! Soundscapes: the ambience of a place.
//!
//! A map's `env_soundscape` entities name a soundscape from
//! `scripts/soundscapes_manifest.txt`'s files; the one nearest the
//! listener (within its radius, in sight) plays. A soundscape is
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
//!         "time" "5,15"  "volume" "0.2,0.4"  "pitch" "90,110"
//!         "rndwave" { "wave" "ambient/machines/hum1.wav" "wave" "ambient/machines/hum2.wav" }
//!     }
//!     "playsoundscape" { "name" "prison.util_distantcombat_light" "volume" "0.5" }
//! }
//! ```
//!
//! Positional sounds (`position N`, placed at the entity's `positionN`
//! target) are read but played by rustlife at the listener.

use std::collections::BTreeMap;

use source_fs::keyvalues::{self, Block};

use crate::soundscript::clean_wave;

/// A sound that loops while its soundscape plays.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopingSound {
    /// The wave, relative to `sound/`.
    pub wave: String,
    /// Volume, `0..=1`.
    pub volume: f32,
    /// Pitch, percent.
    pub pitch: f32,
}

/// A sound played every so often while its soundscape plays.
#[derive(Debug, Clone, PartialEq)]
pub struct RandomSound {
    /// Waves to pick from, relative to `sound/`.
    pub waves: Vec<String>,
    /// Seconds between plays, a range.
    pub time: (f32, f32),
    /// Volume range, `0..=1`.
    pub volume: (f32, f32),
    /// Pitch range, percent.
    pub pitch: (f32, f32),
}

/// One soundscape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Soundscape {
    /// Its looping sounds.
    pub looping: Vec<LoopingSound>,
    /// Its random sounds.
    pub random: Vec<RandomSound>,
    /// Other soundscapes mixed in: lower-cased name and volume.
    pub children: Vec<(String, f32)>,
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

    /// Soundscape `name` with its children mixed in, at their volumes:
    /// every looping and random sound it plays, volumes scaled. Children
    /// nest at most a few deep (a cycle stops there).
    #[must_use]
    pub fn flatten(&self, name: &str) -> Soundscape {
        let mut flat = Soundscape::default();
        self.flatten_into(name, 1.0, 0, &mut flat);
        flat
    }

    fn flatten_into(&self, name: &str, scale: f32, depth: usize, flat: &mut Soundscape) {
        const MAX_DEPTH: usize = 4;
        let Some(found) = self.get(name) else {
            return;
        };
        flat.looping
            .extend(found.looping.iter().map(|sound| LoopingSound {
                volume: sound.volume * scale,
                ..sound.clone()
            }));
        flat.random
            .extend(found.random.iter().map(|sound| RandomSound {
                volume: (sound.volume.0 * scale, sound.volume.1 * scale),
                ..sound.clone()
            }));
        if depth < MAX_DEPTH {
            for (child, volume) in &found.children {
                self.flatten_into(child, scale * volume, depth + 1, flat);
            }
        }
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
        if pair.key.eq_ignore_ascii_case("playlooping") {
            if let Some(wave) = body.get_str("wave").map(clean_wave) {
                scape.looping.push(LoopingSound {
                    wave,
                    volume: number(body, "volume").map_or(1.0, |range| range.0),
                    pitch: number(body, "pitch").map_or(100.0, |range| range.0),
                });
            }
        } else if pair.key.eq_ignore_ascii_case("playrandom") {
            let mut waves: Vec<String> = body
                .get_all("wave")
                .filter_map(|value| value.as_str())
                .map(clean_wave)
                .collect();
            if let Some(random) = body.get_block("rndwave") {
                waves.extend(
                    random
                        .get_all("wave")
                        .filter_map(|value| value.as_str())
                        .map(clean_wave),
                );
            }
            if !waves.is_empty() {
                scape.random.push(RandomSound {
                    waves,
                    time: number(body, "time").unwrap_or((10.0, 10.0)),
                    volume: number(body, "volume").unwrap_or((1.0, 1.0)),
                    pitch: number(body, "pitch").unwrap_or((100.0, 100.0)),
                });
            }
        } else if pair.key.eq_ignore_ascii_case("playsoundscape")
            && let Some(name) = body.get_str("name")
        {
            let volume = number(body, "volume").map_or(1.0, |range| range.0);
            scape.children.push((name.to_ascii_lowercase(), volume));
        }
    }
    scape
}

/// A number or `low, high` range under `key`.
fn number(block: &Block, key: &str) -> Option<(f32, f32)> {
    let text = block.get_str(key)?;
    let value = |part: &str| part.trim().parse::<f32>().ok().filter(|v| v.is_finite());
    match text.split_once(',') {
        Some((low, high)) => {
            let (low, high) = (value(low)?, value(high)?);
            Some((low.min(high), low.max(high)))
        }
        None => value(text).map(|value| (value, value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
        "Prison.Control_Room"
        {
            "dsp" "1"
            "playlooping" { "volume" "0.4" "pitch" "95" "wave" "^ambient\tone.wav" }
            "playrandom"
            {
                "time" "15, 5"
                "volume" "0.2,0.4"
                "rndwave" { "wave" "a.wav" "wave" "b.wav" }
            }
            "playsoundscape" { "name" "Distant" "volume" "0.5" }
        }
        "distant"
        {
            "playlooping" { "volume" "0.8" "wave" "far.wav" }
            "playsoundscape" { "name" "prison.control_room" "volume" "1" }
        }
    "#;

    #[test]
    fn soundscapes_read_their_sounds() {
        let mut scapes = Soundscapes::default();
        assert_eq!(scapes.add_file(FILE), Some(2));
        let room = scapes.get("prison.control_room").expect("room");
        assert_eq!(
            room.looping,
            [LoopingSound {
                wave: "ambient/tone.wav".to_owned(),
                volume: 0.4,
                pitch: 95.0
            }]
        );
        assert_eq!(room.random[0].waves, ["a.wav", "b.wav"]);
        assert_eq!(
            (room.random[0].time, room.random[0].volume),
            ((5.0, 15.0), (0.2, 0.4))
        );
        assert_eq!(room.children, [("distant".to_owned(), 0.5)]);
    }

    #[test]
    fn flattening_mixes_children_in_and_stops_cycles() {
        let mut scapes = Soundscapes::default();
        scapes.add_file(FILE);
        let flat = scapes.flatten("Prison.Control_Room");
        // The room, the distant scape at half volume, the room again inside
        // it... until the depth limit.
        assert!(
            flat.looping.len() > 2 && flat.looping.len() < 12,
            "{}",
            flat.looping.len()
        );
        assert_eq!(flat.looping[1].wave, "far.wav");
        assert!((flat.looping[1].volume - 0.4).abs() < 1e-6);
        assert!(scapes.flatten("missing").looping.is_empty());
    }

    #[test]
    fn manifests_list_their_files() {
        let manifest = r#"soundscaples_manifest { "file" "scripts/soundscapes.txt" "file" "Scripts\Soundscapes_Prison.txt" }"#;
        assert_eq!(
            manifest_files(manifest),
            ["scripts/soundscapes.txt", "scripts/soundscapes_prison.txt"]
        );
    }
}
