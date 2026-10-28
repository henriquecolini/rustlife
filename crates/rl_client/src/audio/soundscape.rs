//! The map's soundscapes: the ambience of where the listener is, as
//! HL2DM's `C_SoundscapeSystem` plays it.
//!
//! One soundscape plays at a time. The server's choice
//! (`CEnvSoundscape::UpdateForPlayer`) is made here: among the enabled
//! `env_soundscape`s (and `_triggerable`s, and `_proxy`s, which borrow
//! another's soundscape and positions), the nearest one whose radius
//! (`-1`: everywhere) reaches the listener and that can see them. When
//! none can, the last one keeps playing.
//!
//! When another takes over, every loop fades to silence over
//! [`FADE_SECS`] and the new soundscape's loops fade up at the same rate;
//! a loop the new one shares with the old (same wave and pitch, both at the
//! listener or both at the same place) carries on and glides to its new
//! volume. Random sounds still to come are dropped at once, and the new
//! ones first play after half their interval.
//!
//! Loops play at the listener, or at the soundscape entity's `positionN`
//! targets, fading with distance as their sound level says. Random sounds
//! play at the listener, at a position, or (`position random`) 36 units
//! from the eye in a random direction, drawn anew each time. Volumes are
//! the scripts' alone, times the ambient volume setting.

use bevy::audio::{AudioSink, SpatialAudioSink, Volume};
use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::collision::contents::{GRATE, MASK_WATER, MOVEABLE, SOLID, WINDOW};
use rl_sim::rng::Rng;
use source_formats::entities::MapEntity;
use source_formats::soundscape::{
    self, RandomPlace, Soundscapes, StartedLoop, StartedRandom, draw,
};

use super::{Chosen, Emitter, Repeat, SoundCategory, SoundPlayer, Volumes, spatial_scale};
use crate::camera::FlyCam;
use crate::net::{Game, LevelEntities};
use crate::{AppState, GameVfs};

/// Seconds between looks at which soundscape is in charge.
const CHOOSE_INTERVAL: f32 = 0.1;
/// Seconds a loop takes to fade between silence and full volume
/// (`soundscape_fadetime`).
pub(crate) const FADE_SECS: f32 = 3.0;
/// The volume a positional loop starts at (HL2DM's engine culls a
/// positional sound at zero).
const POSITIONAL_START: f32 = 0.05;
/// Units from the eye a `position random` sound plays at.
const RANDOM_DISTANCE: f32 = 36.0;
/// Positions an `env_soundscape` names (`position0`..`position7`).
const POSITIONS: usize = 8;
/// Loops within this many units of each other are at the same place.
const SAME_PLACE: f32 = 0.1;
/// What blocks a soundscape's sight of the listener
/// (`MASK_SOLID_BRUSHONLY | MASK_WATER`).
const SIGHT_MASK: u32 = SOLID | WINDOW | GRATE | MOVEABLE | MASK_WATER;
/// The radius that reaches everywhere.
const EVERYWHERE: f32 = -1.0;

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
                (choose, fade_loops, play_random)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Every soundscape of the game.
#[derive(Resource, Default)]
struct Scapes(Soundscapes);

/// A looping sound of the mix.
#[derive(Debug, Clone, PartialEq)]
struct Loop {
    wave: String,
    pitch: f32,
    /// Where it plays, Source space; `None` at the listener.
    place: Option<Vec3>,
    soundlevel: f32,
    /// Its volume now...
    current: f32,
    /// ...and the one it is fading to.
    target: f32,
    /// The soundscape start that last claimed it.
    generation: u32,
    /// Its sound, once started.
    emitter: Option<Entity>,
}

/// A random sound waiting for its next play.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    sound: StartedRandom,
    /// Where an `At` sound plays, Source space.
    place: Option<Vec3>,
    /// When it next plays, seconds of game time.
    due: f32,
}

/// The sounds of the soundscapes, the one in charge and those fading out.
#[derive(Debug, Default)]
struct Mix {
    loops: Vec<Loop>,
    random: Vec<Pending>,
    generation: u32,
}

impl Mix {
    /// A new soundscape takes over: every loop heads for silence, and the
    /// random sounds still to come are dropped.
    fn begin(&mut self) {
        for sound in &mut self.loops {
            sound.target = 0.0;
        }
        self.generation += 1;
        self.random.clear();
    }

    /// Adds a loop of the soundscape just begun at `place`
    /// (`AddLoopingSound`): one the old soundscapes share carries on and
    /// glides to the new volume. Returns the loop whose sound must start
    /// now (stopping any it had), if any.
    fn add_loop(&mut self, sound: &StartedLoop, place: Option<Vec3>) -> Option<usize> {
        let generation = self.generation;
        let reused = self.loops.iter().rposition(|old| {
            old.generation != generation
                && (old.pitch - sound.pitch).abs() < 0.5 // whole percents
                && old.wave.eq_ignore_ascii_case(&sound.wave)
                && old.place.is_some() == place.is_some()
        });
        let claim = |old: &mut Loop| {
            old.target = sound.volume;
            old.generation = generation;
            old.soundlevel = sound.soundlevel;
        };
        if let Some(index) = reused {
            let old = &mut self.loops[index];
            let moved = match (old.place, place) {
                (Some(was), Some(now)) => (was - now).abs().max_element() > SAME_PLACE,
                _ => false,
            };
            claim(old);
            old.place = place;
            // A positional loop that moves restarts there at once, at the
            // volume it had.
            return moved.then_some(index);
        }
        self.loops.push(Loop {
            wave: sound.wave.clone(),
            pitch: sound.pitch,
            place,
            soundlevel: sound.soundlevel,
            current: if place.is_some() {
                POSITIONAL_START
            } else {
                0.0
            },
            target: sound.volume,
            generation,
            emitter: None,
        });
        Some(self.loops.len() - 1)
    }

    /// Moves every loop's volume linearly towards its target, a whole
    /// volume in [`FADE_SECS`], and drops the loops that have gone silent,
    /// returning their sounds to stop.
    fn fade(&mut self, dt: f32) -> Vec<Entity> {
        let rate = dt / FADE_SECS;
        for sound in &mut self.loops {
            let step = (sound.target - sound.current).clamp(-rate, rate);
            sound.current += step;
        }
        let mut stopped = Vec::new();
        self.loops.retain(|sound| {
            let silent = sound.target == 0.0 && sound.current == 0.0;
            if silent {
                stopped.extend(sound.emitter);
            }
            !silent
        });
        stopped
    }
}

/// What plays now.
#[derive(Resource)]
struct Ambience {
    /// The soundscape entity in charge (its index among the map's
    /// entities).
    current: Option<usize>,
    mix: Mix,
    /// Seconds until the next look.
    until_look: f32,
    rng: Rng,
}

impl Default for Ambience {
    fn default() -> Self {
        Self {
            current: None,
            mix: Mix::default(),
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

/// A soundscape entity: its place, reach and soundscape.
#[derive(Debug, Clone, PartialEq)]
struct Spot<'a> {
    /// Its index among the map's entities.
    entity: usize,
    origin: Vec3,
    /// Units it reaches; [`EVERYWHERE`] for no limit.
    radius: f32,
    enabled: bool,
    soundscape: &'a str,
    /// Where its `positionN` targets are, Source space.
    positions: [Option<Vec3>; POSITIONS],
}

impl Spot<'_> {
    /// Which positions it sets, a bit each (`localBits`).
    fn position_bits(&self) -> u32 {
        self.positions
            .iter()
            .enumerate()
            .filter(|(_, position)| position.is_some())
            .fold(0, |bits, (index, _)| bits | (1 << index))
    }

    /// Whether it reaches `range` units.
    fn reaches(&self, range: f32) -> bool {
        self.radius == EVERYWHERE || self.radius > range
    }
}

/// The map's soundscape entities, proxies given the soundscape and
/// positions of the entity they name.
fn spots(entities: &[MapEntity]) -> Vec<Spot<'_>> {
    let named = |name: &str| {
        entities.iter().find(|entity| {
            matches!(
                entity.classname.as_str(),
                "env_soundscape" | "env_soundscape_triggerable"
            ) && entity
                .property("targetname")
                .is_some_and(|target| target.eq_ignore_ascii_case(name))
        })
    };
    let origin_of = |name: &str| {
        entities
            .iter()
            .find(|entity| {
                entity
                    .property("targetname")
                    .is_some_and(|target| target.eq_ignore_ascii_case(name))
            })
            .map(|entity| Vec3::from(entity.origin))
    };
    entities
        .iter()
        .enumerate()
        .filter_map(|(index, entity)| {
            let main = match entity.classname.as_str() {
                "env_soundscape" | "env_soundscape_triggerable" => entity,
                "env_soundscape_proxy" => named(entity.property("MainSoundscapeName")?)?,
                _ => return None,
            };
            let positions = std::array::from_fn(|n| {
                main.property(&format!("position{n}"))
                    .filter(|name| !name.trim().is_empty())
                    .and_then(origin_of)
            });
            Some(Spot {
                entity: index,
                origin: Vec3::from(entity.origin),
                radius: entity.number("radius").unwrap_or(128.0),
                enabled: entity.number("StartDisabled").unwrap_or(0.0) == 0.0,
                soundscape: main.property("soundscape")?,
                positions,
            })
        })
        .collect()
}

/// The spot that should be in charge for a listener at `listener`, given
/// the one that is (`current`, an entity index), as HL2DM's server chooses:
/// the nearest enabled one that reaches the listener and that `sees` (from
/// its origin), the one in charge winning a tie. `None` when none does.
fn audible(
    spots: &[Spot],
    listener: Vec3,
    current: Option<usize>,
    sees: impl Fn(Vec3) -> bool,
) -> Option<usize> {
    // The one in charge goes first.
    let mut order: Vec<usize> = (0..spots.len()).collect();
    order.sort_by_key(|&index| Some(spots[index].entity) != current);
    let mut chosen = None;
    let mut distance = 0.0;
    let mut in_range = false;
    for index in order {
        let spot = &spots[index];
        if !spot.enabled {
            continue;
        }
        let range = spot.origin.distance(listener);
        if Some(spot.entity) == current {
            distance = range;
            in_range = spot.reaches(range) && sees(spot.origin);
            if in_range {
                chosen = Some(index);
            }
        } else if (!in_range || range < distance) && spot.reaches(range) && sees(spot.origin) {
            chosen = Some(index);
            in_range = true;
            distance = range;
        }
    }
    chosen
}

/// Every so often, finds the soundscape in charge and starts it if it is
/// new.
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
    let spots = spots(&entities.0);
    let sees = |from: Vec3| {
        let trace = world.trace_ray(from, listener, SIGHT_MASK);
        !trace.hit() && !trace.start_solid
    };
    let Some(spot) = audible(&spots, listener, ambience.current, sees).map(|i| &spots[i]) else {
        return;
    };
    if ambience.current == Some(spot.entity) {
        return;
    }
    ambience.current = Some(spot.entity);
    if scapes.0.get(spot.soundscape).is_none() {
        debug!(soundscape = spot.soundscape, "unknown soundscape");
        return;
    }
    debug!(soundscape = spot.soundscape, "soundscape");
    start(
        spot,
        &scapes.0,
        time.elapsed_secs(),
        &mut ambience,
        &mut player,
    );
}

/// Starts `spot`'s soundscape over whatever plays (`StartNewSoundscape`).
fn start(
    spot: &Spot,
    scapes: &Soundscapes,
    now: f32,
    ambience: &mut Ambience,
    player: &mut SoundPlayer,
) {
    let Ambience { mix, rng, .. } = ambience;
    let mut unit = || rng.unit();
    let started = scapes.start(spot.soundscape, spot.position_bits(), &mut unit);
    mix.begin();
    let place = |index: Option<usize>| index.and_then(|n| spot.positions.get(n).copied()?);
    for sound in &started.looping {
        let Some(index) = mix.add_loop(sound, place(sound.position)) else {
            continue;
        };
        let fresh = &mut mix.loops[index];
        if let Some(old) = fresh.emitter.take() {
            player.stop(old);
        }
        fresh.emitter = start_loop(fresh, player);
    }
    mix.random = started
        .random
        .into_iter()
        .map(|sound| {
            let wait = 0.5 * draw(sound.sound.time, &mut unit);
            let place = match sound.place {
                RandomPlace::At(n) => spot.positions.get(n).copied().flatten(),
                RandomPlace::Ambient | RandomPlace::AroundListener => None,
            };
            Pending {
                sound,
                place,
                due: now + wait,
            }
        })
        .collect();
}

/// Starts a loop's sound at the volume it has now.
fn start_loop(sound: &Loop, player: &mut SoundPlayer) -> Option<Entity> {
    let chosen = Chosen {
        path: sound.wave.clone(),
        volume: 1.0,
        speed: (sound.pitch / 100.0).clamp(0.05, 4.0),
        soundlevel: sound.soundlevel,
    };
    let spatial = sound.place.and_then(|_| spatial_scale(sound.soundlevel));
    let emitter =
        player.spawn_chosen(&chosen, sound.place, sound.current, spatial, Repeat::Loop)?;
    player.categorize(emitter, SoundCategory::Ambient);
    Some(emitter)
}

/// Fades the loops and sets their sounds' volumes; stops the silent ones.
fn fade_loops(
    mut commands: Commands,
    time: Res<Time>,
    volumes: Volumes,
    mut ambience: ResMut<Ambience>,
    mut emitters: Query<(
        &mut Emitter,
        Option<&mut AudioSink>,
        Option<&mut SpatialAudioSink>,
    )>,
) {
    for stopped in ambience.mix.fade(time.delta_secs()) {
        commands.entity(stopped).try_despawn();
    }
    let level = volumes.gain(SoundCategory::Ambient);
    for sound in &ambience.mix.loops {
        let Some(Ok((mut emitter, plain, spatial))) = sound.emitter.map(|e| emitters.get_mut(e))
        else {
            continue;
        };
        emitter.volume = sound.current;
        let volume = Volume::Linear(sound.current * level);
        if let Some(mut sink) = plain {
            sink.set_volume(volume);
        }
        if let Some(mut sink) = spatial {
            sink.set_volume(volume);
        }
    }
}

/// Plays the random sounds that are due, each drawn afresh, and sets when
/// they next play.
fn play_random(
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut ambience: ResMut<Ambience>,
    mut player: SoundPlayer,
) {
    let now = time.elapsed_secs();
    let Ambience { mix, rng, .. } = &mut *ambience;
    for pending in &mut mix.random {
        if now < pending.due {
            continue;
        }
        let mut unit = || rng.unit();
        let sound = &pending.sound.sound;
        pending.due = now + draw(sound.time, &mut unit);
        let wave = sound.waves[rng.below(sound.waves.len())].clone();
        let mut unit = || rng.unit();
        let volume = draw(sound.volume, &mut unit) * pending.sound.master;
        let pitch = draw(sound.pitch, &mut unit).trunc();
        let soundlevel = draw(sound.soundlevel, &mut unit).trunc();
        let place = match pending.sound.place {
            RandomPlace::Ambient => None,
            RandomPlace::At(_) => pending.place,
            RandomPlace::AroundListener => {
                let angle = draw((-180.0, 180.0), &mut unit);
                camera.single().ok().map(|eye| around(eye, angle))
            }
        };
        // The engine ignores a sound at pitch 0, the default.
        if pitch <= 0.0 {
            continue;
        }
        let chosen = Chosen {
            path: wave,
            volume: volume.clamp(0.0, 1.0),
            speed: (pitch / 100.0).clamp(0.05, 4.0),
            soundlevel,
        };
        let spatial = place.and_then(|_| spatial_scale(soundlevel));
        if let Some(emitter) = player.spawn_chosen(&chosen, place, 1.0, spatial, Repeat::Once) {
            player.categorize(emitter, SoundCategory::Ambient);
        }
    }
}

/// Where a `position random` sound plays: [`RANDOM_DISTANCE`] from the
/// eye at `angle` round it, between its right and its forward
/// (`GenerateRandomSoundPosition`, which takes the degrees it draws as
/// radians). Source space.
fn around(eye: &GlobalTransform, angle: f32) -> Vec3 {
    let at = eye.translation();
    let source = |bevy: Vec3| Vec3::from(coords::to_source_position(bevy));
    let direction = |towards: Vec3| (source(at + towards) - source(at)).normalize_or_zero();
    let (forward, right) = (direction(*eye.forward()), direction(*eye.right()));
    let (sin, cos) = angle.sin_cos();
    source(at) + RANDOM_DISTANCE * (cos * right + sin * forward)
}

#[cfg(test)]
mod tests;
