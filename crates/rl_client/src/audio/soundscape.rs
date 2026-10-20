//! The map's soundscapes: the ambience of where the listener is.
//!
//! As in Source, the soundscape that plays is that of the nearest
//! `env_soundscape` (or `env_soundscape_proxy`, which borrows another's)
//! whose radius the listener is within and which it can see; leaving every
//! radius keeps the last one playing. A soundscape's looping sounds play
//! for as long as it does, its random ones every so often, all at the
//! listener (rustlife does not place positional soundscape sounds).

use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::collision::contents::MASK_SHOT;
use rl_sim::rng::Rng;
use source_formats::entities::MapEntity;
use source_formats::soundscape::{self, Soundscape, Soundscapes};

use super::{Chosen, Repeat, SoundPlayer};
use crate::camera::FlyCam;
use crate::net::{Game, LevelEntities};
use crate::{AppState, GameVfs};

/// Seconds between looks for the soundscape to play.
const CHOOSE_INTERVAL: f32 = 0.25;

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
                (choose, play_random)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Every soundscape of the game.
#[derive(Resource, Default)]
struct Scapes(Soundscapes);

/// What plays now.
#[derive(Resource)]
struct Ambience {
    /// The soundscape playing, lower case.
    current: Option<String>,
    /// Its looping sounds' emitters.
    loops: Vec<Entity>,
    /// Its random sounds, each with the time it next plays.
    random: Vec<(soundscape::RandomSound, f32)>,
    /// Seconds until the next look.
    until_look: f32,
    rng: Rng,
}

impl Default for Ambience {
    fn default() -> Self {
        Self {
            current: None,
            loops: Vec::new(),
            random: Vec::new(),
            until_look: 0.0,
            rng: Rng::seeded(&[0x0073_6361_7065]),
        }
    }
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

/// Every so often, picks the soundscape to play and switches to it.
fn choose(
    mut commands: Commands,
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
    let nearest = spots(&entities.0)
        .into_iter()
        .filter(|spot| spot.origin.distance(listener) <= spot.radius)
        .filter(|spot| !world.trace_ray(spot.origin, listener, MASK_SHOT).hit())
        .min_by(|a, b| {
            a.origin
                .distance_squared(listener)
                .total_cmp(&b.origin.distance_squared(listener))
        })
        .map(|spot| spot.soundscape.to_ascii_lowercase());
    let Some(name) = nearest.filter(|name| ambience.current.as_ref() != Some(name)) else {
        return;
    };
    for emitter in std::mem::take(&mut ambience.loops) {
        commands.entity(emitter).try_despawn();
    }
    let scape: Soundscape = scapes.0.flatten(&name);
    debug!(
        soundscape = name,
        loops = scape.looping.len(),
        random = scape.random.len(),
        "soundscape"
    );
    for sound in &scape.looping {
        let chosen = Chosen {
            path: sound.wave.clone(),
            volume: sound.volume.clamp(0.0, 1.0),
            speed: (sound.pitch / 100.0).clamp(0.05, 4.0),
            soundlevel: 0.0,
        };
        if let Some(emitter) = player.spawn_chosen(&chosen, None, 1.0, None, Repeat::Loop) {
            ambience.loops.push(emitter);
        }
    }
    let now = time.elapsed_secs();
    let mut random = Vec::new();
    for sound in scape.random {
        let wait = pick(&mut ambience.rng, sound.time);
        random.push((sound, now + wait));
    }
    ambience.random = random;
    ambience.current = Some(name);
}

/// Plays the soundscape's random sounds when they are due.
fn play_random(time: Res<Time>, mut ambience: ResMut<Ambience>, mut player: SoundPlayer) {
    let now = time.elapsed_secs();
    let Ambience { random, rng, .. } = &mut *ambience;
    for (sound, due) in random.iter_mut() {
        if now < *due {
            continue;
        }
        *due = now + pick(rng, sound.time).max(0.1);
        let Some(wave) = sound.waves.get(rng.below(sound.waves.len())) else {
            continue;
        };
        let chosen = Chosen {
            path: wave.clone(),
            volume: pick(rng, sound.volume).clamp(0.0, 1.0),
            speed: (pick(rng, sound.pitch) / 100.0).clamp(0.05, 4.0),
            soundlevel: 0.0,
        };
        player.spawn_chosen(&chosen, None, 1.0, None, Repeat::Once);
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
}
