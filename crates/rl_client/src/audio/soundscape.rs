//! The map's soundscapes: the ambience of where the listener is.
//!
//! `env_soundscape` (and `env_soundscape_proxy`, which borrows another's)
//! entities mark places with a radius. Where Source plays the nearest one
//! the listener can see and cuts to the next at its edge, rustlife mixes:
//! every soundscape the listener is within reach of is a *layer* with a
//! gain that follows how deep inside its radius they stand, so walking from
//! one place to the next fades one out and the other in with the distance
//! rather than switching at a line. A layer's gain also eases over time, so
//! a doorway that opens or a wall that comes between does not cut it. A
//! layer's looping sounds play for as long as it does, its random ones
//! every so often, all at the listener (rustlife does not place positional
//! soundscape sounds). Leaving every radius keeps the last place playing.

use std::collections::BTreeMap;

use bevy::audio::{AudioSink, Volume};
use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::collision::contents::MASK_SHOT;
use rl_sim::rng::Rng;
use source_formats::entities::MapEntity;
use source_formats::soundscape::{self, Soundscapes};

use super::{Chosen, Emitter, Repeat, SoundCategory, SoundPlayer};
use crate::camera::FlyCam;
use crate::net::{Game, LevelEntities};
use crate::settings::Settings;
use crate::{AppState, GameVfs};

/// Seconds between looks at where the listener stands.
const CHOOSE_INTERVAL: f32 = 0.1;
/// How fast a layer's gain approaches its target, per second.
const GAIN_RATE: f32 = 1.8;
/// A layer this quiet, with a target of nothing, is dropped.
const SILENT: f32 = 0.01;
/// How deep into a radius (as a fraction of it) the listener must be for a
/// soundscape to be at full strength.
const FULL_AT: f32 = 0.55;
/// The least the place last in charge keeps while the listener is between
/// places, as a share of full.
const LINGER: f32 = 1.0;

/// Plays the map's soundscapes.
pub(super) struct SoundscapePlugin;

impl Plugin for SoundscapePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Scapes>()
            .init_resource::<Ambience>()
            .add_systems(Startup, load_soundscapes)
            .add_systems(
                OnEnter(AppState::InGame),
                |mut ambience: ResMut<Ambience>| {
                    *ambience = Ambience::default();
                },
            )
            .add_systems(
                Update,
                (choose, fade_layers, play_random)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Every soundscape of the game.
#[derive(Resource, Default)]
struct Scapes(Soundscapes);

/// One soundscape being mixed in.
struct Layer {
    /// The soundscape, lower case.
    name: String,
    /// Its looping sounds' emitters with the volume each has at full gain.
    loops: Vec<(Entity, f32)>,
    /// Its random sounds, each with the time it next plays.
    random: Vec<(soundscape::RandomSound, f32)>,
    /// How loud the layer is now, `0..=1`.
    gain: f32,
    /// How loud it is heading for.
    target: f32,
}

/// What plays now.
#[derive(Resource)]
struct Ambience {
    layers: Vec<Layer>,
    /// The place that was most strongly in charge, which lingers between
    /// places.
    anchor: Option<String>,
    /// Seconds until the next look.
    until_look: f32,
    rng: Rng,
}

impl Default for Ambience {
    fn default() -> Self {
        Self {
            layers: Vec::new(),
            anchor: None,
            until_look: 0.0,
            rng: Rng::seeded(&[0x0073_6361_7065]),
        }
    }
}

/// How strongly a soundscape of `radius` is heard from `distance` away:
/// nothing at its edge, rising smoothly to full at [`FULL_AT`] of the way in.
#[must_use]
fn weight(distance: f32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }
    let depth = ((radius - distance) / (radius * FULL_AT)).clamp(0.0, 1.0);
    depth * depth * (3.0 - 2.0 * depth)
}

/// The gains the soundscapes are heading for, given how strongly each is
/// heard (`weights`, by name) and which one is `anchor`: they share the
/// whole between them (two soundscapes both at full strength play at half),
/// and the anchor keeps its place while nothing else has taken over.
/// Nothing at all gives no targets: the layers stay as they are.
#[must_use]
fn targets(weights: &BTreeMap<String, f32>, anchor: Option<&str>) -> Vec<(String, f32)> {
    let sum: f32 = weights.values().sum();
    if sum <= 0.0 {
        return Vec::new();
    }
    let others = |name: &str| {
        weights
            .iter()
            .filter(|(other, _)| other.as_str() != name)
            .map(|(_, weight)| weight)
            .sum::<f32>()
    };
    let mut out: Vec<(String, f32)> = weights
        .iter()
        .map(|(name, &weight)| (name.clone(), weight / sum.max(1.0)))
        .collect();
    if let Some(anchor) = anchor
        && let Some(entry) = out.iter_mut().find(|(name, _)| name == anchor)
    {
        // Between places, the one that was here carries on at full.
        let linger = LINGER * (1.0 - others(anchor).min(1.0));
        entry.1 = entry.1.max(linger.min(1.0));
    }
    out
}

/// Reads every soundscape file the manifest lists.
fn load_soundscapes(vfs: Res<GameVfs>, mut scapes: ResMut<Scapes>) {
    let read = |path: &str| {
        vfs.0
            .read(path)
            .ok()
            .flatten()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    };
    let Some(manifest) = read("scripts/soundscapes_manifest.txt") else {
        return;
    };
    for file in soundscape::manifest_files(&manifest) {
        if let Some(text) = read(&file) {
            scapes.0.add_file(&text);
        }
    }
    info!("{} soundscapes", scapes.0.len());
}

/// An `env_soundscape`'s place, reach and soundscape.
struct Spot<'a> {
    origin: Vec3,
    radius: f32,
    soundscape: &'a str,
}

/// The map's soundscape entities, proxies resolved to the soundscape of
/// the entity they name.
fn spots(entities: &[MapEntity]) -> Vec<Spot<'_>> {
    let named = |name: &str| {
        entities
            .iter()
            .find(|entity| {
                entity.classname == "env_soundscape"
                    && entity
                        .property("targetname")
                        .is_some_and(|target| target.eq_ignore_ascii_case(name))
            })
            .and_then(|entity| entity.property("soundscape"))
    };
    entities
        .iter()
        .filter_map(|entity| {
            let soundscape = match entity.classname.as_str() {
                "env_soundscape" => entity.property("soundscape")?,
                "env_soundscape_proxy" => named(entity.property("MainSoundscapeName")?)?,
                _ => return None,
            };
            Some(Spot {
                origin: Vec3::from(entity.origin),
                radius: entity.number("radius").unwrap_or(128.0),
                soundscape,
            })
        })
        .collect()
}

/// Every so often, works out how strongly each soundscape is heard and
/// sets the layers' targets, starting any that are new.
fn choose(
    time: Res<Time>,
    game: Game,
    entities: Res<LevelEntities>,
    scapes: Res<Scapes>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut ambience: ResMut<Ambience>,
    mut player: SoundPlayer,
) {
    ambience.until_look -= time.delta_secs();
    if ambience.until_look > 0.0 {
        return;
    }
    ambience.until_look = CHOOSE_INTERVAL;
    let (Some(sim), Ok(camera)) = (game.sim(), camera.single()) else {
        return;
    };
    let listener = Vec3::from(coords::to_source_position(camera.translation()));
    let world = sim.world();
    let mut heard: BTreeMap<String, f32> = BTreeMap::new();
    for spot in spots(&entities.0) {
        let strength = weight(spot.origin.distance(listener), spot.radius);
        if strength <= 0.0 || world.trace_ray(spot.origin, listener, MASK_SHOT).hit() {
            continue;
        }
        let entry = heard
            .entry(spot.soundscape.to_ascii_lowercase())
            .or_default();
        *entry = entry.max(strength);
    }
    let wanted = targets(&heard, ambience.anchor.as_deref());
    if wanted.is_empty() {
        return;
    }
    if let Some((name, _)) = heard
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .filter(|(_, strength)| **strength > 0.3)
    {
        ambience.anchor = Some(name.clone());
    }
    let now = time.elapsed_secs();
    for layer in &mut ambience.layers {
        layer.target = 0.0;
    }
    for (name, target) in wanted {
        if let Some(layer) = ambience.layers.iter_mut().find(|layer| layer.name == name) {
            layer.target = target;
        } else {
            let layer = start_layer(&name, target, now, &scapes, &mut ambience.rng, &mut player);
            ambience.layers.push(layer);
        }
    }
}

/// Starts a soundscape's looping sounds, silent, to be faded in.
fn start_layer(
    name: &str,
    target: f32,
    now: f32,
    scapes: &Scapes,
    rng: &mut Rng,
    player: &mut SoundPlayer,
) -> Layer {
    let scape = scapes.0.flatten(name);
    debug!(
        soundscape = name,
        loops = scape.looping.len(),
        random = scape.random.len(),
        "soundscape"
    );
    let mut loops = Vec::new();
    for sound in &scape.looping {
        let base = sound.volume.clamp(0.0, 1.0);
        let chosen = Chosen {
            path: sound.wave.clone(),
            volume: 0.0,
            speed: (sound.pitch / 100.0).clamp(0.05, 4.0),
            soundlevel: 0.0,
        };
        if let Some(emitter) = player.spawn_chosen(&chosen, None, 1.0, None, Repeat::Loop) {
            player.categorize(emitter, SoundCategory::Ambient);
            loops.push((emitter, base));
        }
    }
    let random = scape
        .random
        .into_iter()
        .map(|sound| {
            let wait = pick(rng, sound.time);
            (sound, now + wait)
        })
        .collect();
    Layer {
        name: name.to_owned(),
        loops,
        random,
        gain: 0.0,
        target,
    }
}

/// Eases every layer's gain towards its target, sets its loops' volumes and
/// drops the layers that have faded out.
fn fade_layers(
    mut commands: Commands,
    time: Res<Time>,
    settings: Res<Settings>,
    mut ambience: ResMut<Ambience>,
    mut emitters: Query<(&mut Emitter, Option<&mut AudioSink>)>,
) {
    let dt = time.delta_secs();
    let level = SoundCategory::Ambient.gain(&settings.audio);
    for layer in &mut ambience.layers {
        layer.gain = crate::ui::anim::smooth_toward(layer.gain, layer.target, GAIN_RATE, dt);
        for &(entity, base) in &layer.loops {
            let Ok((mut emitter, sink)) = emitters.get_mut(entity) else {
                continue;
            };
            emitter.volume = base * layer.gain;
            if let Some(mut sink) = sink {
                sink.set_volume(Volume::Linear(emitter.volume * level));
            }
        }
    }
    ambience.layers.retain(|layer| {
        let keep = layer.target > 0.0 || layer.gain > SILENT;
        if !keep {
            for &(entity, _) in &layer.loops {
                commands.entity(entity).try_despawn();
            }
        }
        keep
    });
}

/// Plays the layers' random sounds when they are due, as loud as the layer.
fn play_random(time: Res<Time>, mut ambience: ResMut<Ambience>, mut player: SoundPlayer) {
    let now = time.elapsed_secs();
    let Ambience { layers, rng, .. } = &mut *ambience;
    for layer in layers {
        let gain = layer.gain;
        for (sound, due) in &mut layer.random {
            if now < *due {
                continue;
            }
            *due = now + pick(rng, sound.time).max(0.1);
            let Some(wave) = sound.waves.get(rng.below(sound.waves.len())) else {
                continue;
            };
            if gain < SILENT {
                continue;
            }
            let chosen = Chosen {
                path: wave.clone(),
                volume: (pick(rng, sound.volume) * gain).clamp(0.0, 1.0),
                speed: (pick(rng, sound.pitch) / 100.0).clamp(0.05, 4.0),
                soundlevel: 0.0,
            };
            if let Some(emitter) = player.spawn_chosen(&chosen, None, 1.0, None, Repeat::Once) {
                player.categorize(emitter, SoundCategory::Ambient);
            }
        }
    }
}

/// A number within `(low, high)`.
fn pick(rng: &mut Rng, (low, high): (f32, f32)) -> f32 {
    low + (high - low) * rng.unit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_borrow_the_soundscape_they_name() {
        let entities = [
            MapEntity::new("env_soundscape", [1.0, 0.0, 0.0])
                .with("targetname", "Out")
                .with("soundscape", "strike_outside")
                .with("radius", "300"),
            MapEntity::new("env_soundscape_proxy", [9.0, 0.0, 0.0])
                .with("MainSoundscapeName", "out"),
            MapEntity::new("env_soundscape_proxy", [9.0, 0.0, 0.0])
                .with("MainSoundscapeName", "nobody"),
        ];
        let spots = spots(&entities);
        assert_eq!(spots.len(), 2);
        assert_eq!(
            (spots[0].soundscape, spots[0].radius),
            ("strike_outside", 300.0)
        );
        assert_eq!(
            (spots[1].soundscape, spots[1].radius, spots[1].origin.x),
            ("strike_outside", 128.0, 9.0)
        );
    }

    #[test]
    fn a_soundscape_is_silent_at_its_edge_and_full_well_inside() {
        assert_eq!(weight(300.0, 300.0), 0.0);
        assert_eq!(weight(500.0, 300.0), 0.0);
        assert_eq!(weight(0.0, 300.0), 1.0);
        assert_eq!(weight(300.0 * (1.0 - FULL_AT), 300.0), 1.0);
        let steps: Vec<f32> = (0..=10)
            .map(|i| weight(300.0 - 16.0 * i as f32, 300.0))
            .collect();
        assert!(steps.windows(2).all(|pair| pair[0] <= pair[1]), "{steps:?}");
        assert_eq!(weight(10.0, 0.0), 0.0);
    }

    #[test]
    fn overlapping_soundscapes_share_the_mix_and_the_anchor_lingers() {
        let weights = |entries: &[(&str, f32)]| -> BTreeMap<String, f32> {
            entries
                .iter()
                .map(|&(name, w)| (name.to_owned(), w))
                .collect()
        };
        let gain = |targets: &[(String, f32)], name: &str| {
            targets.iter().find(|(n, _)| n == name).map(|(_, g)| *g)
        };
        // Both at full strength: half each.
        let both = targets(&weights(&[("a", 1.0), ("b", 1.0)]), None);
        assert_eq!(gain(&both, "a"), Some(0.5));
        assert_eq!(gain(&both, "b"), Some(0.5));
        // Only one, partly in: it fades with distance...
        let edge = targets(&weights(&[("a", 0.3)]), None);
        assert_eq!(gain(&edge, "a"), Some(0.3));
        // ...unless it was the place we were in: then it lingers at full.
        let lingering = targets(&weights(&[("a", 0.3)]), Some("a"));
        assert_eq!(gain(&lingering, "a"), Some(1.0));
        // Entering another place takes the mix from it as it comes.
        let crossing = targets(&weights(&[("a", 0.3), ("b", 0.7)]), Some("a"));
        assert!((gain(&crossing, "a").expect("a") - 0.3).abs() < 1e-6);
        assert!((gain(&crossing, "b").expect("b") - 0.7).abs() < 1e-6);
        // Nowhere: no change.
        assert!(targets(&weights(&[]), Some("a")).is_empty());
    }
}
