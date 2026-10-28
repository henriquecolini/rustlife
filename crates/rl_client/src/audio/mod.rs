//! Sound: the game's own sounds, played where things happen.
//!
//! Sounds are named as the mounted game names them — soundscript entries
//! (`Weapon_Shotgun.Single`) or files under `sound/` — by the gameplay data
//! (weapons, items, `sounds.ron`) and by the map (its `ambient_generic`s,
//! doors and buttons). [`Sounds`] resolves a name through the game's
//! soundscripts (`scripts/game_sounds_manifest.txt`, plus the map's own
//! `maps/<map>_level_sounds.txt`): it picks one of the entry's waves and a
//! volume and pitch within its ranges.
//!
//! Every sound plays from an [`Emitter`] entity. An emitter waits for its
//! sound to load, then starts playing: once (and despawns) or looping
//! (until its owner despawns it). Emitters at a place are spatial: panned
//! between the ears of the listener on the camera and fading with
//! distance as the sound's level says; those of the local player's own
//! actions play at the listener.
//!
//! What plays when is split by source: game events (`events`), the map's
//! ambient sounds, moving doors and wall chargers (`world`), the
//! soundscape of where the listener is (`soundscape`), footsteps
//! (`steps`), the weapon list (`weapon_select`) and the menus' widgets
//! (`ui`), which sound in the main menu too, before any map is loaded.

use std::collections::HashMap;

use bevy::audio::{GlobalVolume, PlaybackMode, SpatialListener, SpatialScale, Volume};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use rl_assets::{SOURCE, SoundAsset, coords};
use rl_sim::rng::Rng;
use source_formats::soundscript::{self, SoundScripts};
use source_fs::Vfs;

use crate::camera::FlyCam;
use crate::settings::Settings;
use crate::world::MapScoped;
use crate::{AppState, CurrentMap, GameVfs};

mod events;
mod gravity_gun;
mod props;
mod soundscape;
mod steps;
mod ui;
mod weapon_select;
mod world;

/// Gap between the listener's ears, metres.
const EAR_GAP: f32 = 0.3;
/// Seconds a one-shot sound may wait to load before it is too late to
/// play.
const STALE_SECS: f32 = 0.5;
/// Units per metre (Source's inch).
const UNITS_PER_METRE: f32 = 1.0 / 0.0254;

/// Plays the game's sounds.
#[derive(Debug, Clone, Copy, Default)]
pub struct SoundPlugin {
    /// Play every sound at volume zero this run (see [`Silent`]).
    pub silent: bool,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        if self.silent {
            // Nothing should reach the speakers, not even a sound played
            // without an emitter, so Bevy's own volume is zeroed too. It is
            // only read when a sound starts, which is why the emitters'
            // gains go through `Volumes` as well.
            app.insert_resource(Silent)
                .insert_resource(GlobalVolume::new(Volume::SILENT));
        }
        app.init_resource::<Sounds>()
            .add_systems(
                OnEnter(AppState::InGame),
                (load_soundscripts, load_level_sounds).chain(),
            )
            .add_systems(Update, (add_listener, start_emitters, apply_volumes))
            .add_plugins((
                events::EventSoundsPlugin,
                gravity_gun::GravityGunSoundsPlugin,
                props::PropSoundsPlugin,
                world::WorldSoundsPlugin,
                steps::StepsPlugin,
                weapon_select::WeaponSelectSoundsPlugin,
                soundscape::SoundscapePlugin,
                ui::UiSoundsPlugin,
            ));
    }
}

/// The game's soundscripts and the sounds loaded so far.
#[derive(Resource)]
pub struct Sounds {
    scripts: SoundScripts,
    /// Loaded (or loading) sounds by path under `sound/`.
    handles: HashMap<String, Handle<SoundAsset>>,
    /// Picks waves, volumes and pitches.
    rng: Rng,
}

impl Default for Sounds {
    fn default() -> Self {
        Self {
            scripts: SoundScripts::default(),
            handles: HashMap::new(),
            rng: Rng::seeded(&[0x736f_756e_6473]),
        }
    }
}

/// A sound chosen to play: which file, how loud, at what speed, and how
/// far it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    /// The file, relative to `sound/`.
    pub path: String,
    /// Volume, `0..=1`.
    pub volume: f32,
    /// Playback speed (1: as recorded).
    pub speed: f32,
    /// Sound level, decibels (0: heard everywhere).
    pub soundlevel: f32,
}

impl Sounds {
    /// Resolves `name` — a soundscript entry, else a file under `sound/` —
    /// picking a wave, volume and pitch. `None` for an unknown entry that
    /// is not a sound file either.
    pub fn choose(&mut self, name: &str) -> Option<Chosen> {
        if let Some(entry) = self.scripts.get(name) {
            let pick = |rng: &mut Rng, (low, high): (f32, f32)| low + (high - low) * rng.unit();
            let wave = entry.waves.get(self.rng.below(entry.waves.len()))?.clone();
            return Some(Chosen {
                path: wave,
                volume: pick(&mut self.rng, entry.volume).clamp(0.0, 1.0),
                speed: (pick(&mut self.rng, entry.pitch) / 100.0).clamp(0.05, 4.0),
                soundlevel: entry.soundlevel,
            });
        }
        let path = soundscript::clean_wave(name);
        let file = path.strip_prefix("sound/").unwrap_or(&path).to_owned();
        is_sound_file(&file).then_some(Chosen {
            path: file,
            volume: 1.0,
            speed: 1.0,
            soundlevel: soundscript::SNDLVL_NORM,
        })
    }

    /// Whether `name` is a soundscript entry (rather than a file).
    #[must_use]
    pub fn is_scripted(&self, name: &str) -> bool {
        self.scripts.get(name).is_some()
    }

    /// The loaded (or loading) sound at `path` under `sound/`.
    pub fn handle(&mut self, path: &str, assets: &AssetServer) -> Handle<SoundAsset> {
        self.handles
            .entry(path.to_owned())
            .or_insert_with(|| assets.load(format!("{SOURCE}://sound/{path}")))
            .clone()
    }
}

/// Whether `path` names a sound file (`.wav` or `.mp3`).
fn is_sound_file(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("wav") || extension.eq_ignore_ascii_case("mp3")
        })
}

/// How an emitter plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// Once, then the emitter despawns.
    Once,
    /// Over and over, until the emitter is despawned.
    Loop,
    /// Over and over if the sound loops (its WAV has a loop point), else
    /// once — as the map's ambient sounds play. The emitter stays.
    AsRecorded,
}

/// An entity that plays a sound once it has loaded. Give it a `Transform`
/// to place it; without [`Emitter::spatial`] it plays at the listener.
#[derive(Component, Debug, Clone)]
pub struct Emitter {
    /// The sound.
    pub sound: Handle<SoundAsset>,
    /// Volume, `0..=1`.
    pub volume: f32,
    /// Playback speed.
    pub speed: f32,
    /// Distance scale of a spatial sound (see [`spatial_scale`]); `None`
    /// plays it at the listener, unpanned and unfaded.
    pub spatial: Option<f32>,
    /// Once or looping.
    pub repeat: Repeat,
    /// When it was asked for, seconds of app time.
    pub requested: f32,
}

/// Which of the player's volume sliders an emitter follows.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoundCategory {
    /// Weapons, footsteps, props, pickups and the like.
    #[default]
    Effects,
    /// The place's soundscape and the map's ambient sounds.
    Ambient,
    /// The menus' widgets: only the master volume applies.
    Ui,
}

impl SoundCategory {
    /// The gain the player's volume settings give this category.
    #[must_use]
    pub fn gain(self, audio: &rl_config::AudioSettings) -> f32 {
        audio.master
            * match self {
                Self::Effects => audio.effects,
                Self::Ambient => audio.ambient,
                Self::Ui => 1.0,
            }
    }
}

/// Present for a silent run (`--silent`, and every unattended capture):
/// every sound plays at volume zero, whatever the volume settings say.
///
/// The [`Settings`] are left as the player set them, so a silent run that
/// changes some other setting saves the player's own volumes, not zero.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Silent;

/// The gains sounds play at this run: those of the player's volume
/// settings, or none at all in a [`Silent`] run. Every volume given to a
/// playing sound goes through here.
#[derive(SystemParam)]
pub struct Volumes<'w> {
    settings: Res<'w, Settings>,
    silent: Option<Res<'w, Silent>>,
}

impl Volumes<'_> {
    /// The gain a sound of `category` plays at.
    #[must_use]
    pub fn gain(&self, category: SoundCategory) -> f32 {
        if self.silent.is_some() {
            0.0
        } else {
            category.gain(&self.settings.audio)
        }
    }

    /// Whether the gains may have changed since the system last ran.
    #[must_use]
    pub fn is_changed(&self) -> bool {
        self.settings.is_changed() || self.silent.as_ref().is_some_and(Res::is_changed)
    }
}

/// Marks an emitter whose sound has started.
#[derive(Component)]
struct Started;

/// The rodio distance scale (per metre) that makes a sound of `soundlevel`
/// dB fade as Source's: full volume out to the distance where Source's
/// `1 / (distance × factor)` gain drops below 1, then falling. rodio falls
/// with the square of distance where Source falls with distance, so the
/// curves are matched at twice that distance: louder than Source close
/// in, quieter far off. `None` for sounds heard everywhere.
#[must_use]
pub fn spatial_scale(soundlevel: f32) -> Option<f32> {
    let factor = soundscript::distance_factor(soundlevel);
    (factor > 0.0).then(|| {
        let full_volume_metres = 1.0 / factor / UNITS_PER_METRE;
        1.0 / (std::f32::consts::SQRT_2 * full_volume_metres)
    })
}

/// Playing sounds, for any system.
#[derive(SystemParam)]
pub struct SoundPlayer<'w, 's> {
    commands: Commands<'w, 's>,
    sounds: ResMut<'w, Sounds>,
    assets: Res<'w, AssetServer>,
    time: Res<'w, Time>,
}

impl SoundPlayer<'_, '_> {
    /// Plays `name` once at `at` (Source space), or at the listener, at
    /// `volume` times its own. Returns the emitter.
    pub fn play(&mut self, name: &str, at: Option<Vec3>, volume: f32) -> Option<Entity> {
        self.spawn(name, at, volume, Repeat::Once)
    }

    /// Starts `name` playing at `at` (or the listener) as `repeat` says;
    /// the caller despawns the emitter to stop it, and may move it.
    pub fn spawn(
        &mut self,
        name: &str,
        at: Option<Vec3>,
        volume: f32,
        repeat: Repeat,
    ) -> Option<Entity> {
        let Some(chosen) = self.sounds.choose(name) else {
            debug!("no sound named {name}");
            return None;
        };
        let spatial = at.and_then(|_| spatial_scale(chosen.soundlevel));
        self.spawn_chosen(&chosen, at, volume, spatial, repeat)
    }

    /// Starts a resolved sound, with an explicit distance scale.
    pub fn spawn_chosen(
        &mut self,
        chosen: &Chosen,
        at: Option<Vec3>,
        volume: f32,
        spatial: Option<f32>,
        repeat: Repeat,
    ) -> Option<Entity> {
        debug!(path = chosen.path, ?at, ?repeat, "sound");
        let transform = at.map_or_else(Transform::default, |at| {
            Transform::from_translation(coords::position(at.to_array()))
        });
        let emitter = self.emitter(chosen, volume, at.and(spatial), repeat);
        Some(self.commands.spawn((emitter, transform, MapScoped)).id())
    }

    /// Plays `name` once at the listener for the menus: under the
    /// [`SoundCategory::Ui`] volume, and not cut off by leaving a map.
    pub fn play_ui(&mut self, name: &str, volume: f32) -> Option<Entity> {
        let Some(chosen) = self.sounds.choose(name) else {
            debug!("no sound named {name}");
            return None;
        };
        let emitter = self.emitter(&chosen, volume, None, Repeat::Once);
        let sound = (emitter, Transform::default(), SoundCategory::Ui);
        Some(self.commands.spawn(sound).id())
    }

    fn emitter(
        &mut self,
        chosen: &Chosen,
        volume: f32,
        spatial: Option<f32>,
        repeat: Repeat,
    ) -> Emitter {
        Emitter {
            sound: self.sounds.handle(&chosen.path, &self.assets),
            volume: (chosen.volume * volume).clamp(0.0, 1.0),
            speed: chosen.speed,
            spatial,
            repeat,
            requested: self.time.elapsed_secs(),
        }
    }

    /// Stops an emitter's sound, if it is still playing.
    pub fn stop(&mut self, emitter: Entity) {
        self.commands.entity(emitter).try_despawn();
    }

    /// Files an emitter under a volume slider other than the default one.
    pub fn categorize(&mut self, emitter: Entity, category: SoundCategory) {
        self.commands.entity(emitter).insert(category);
    }

    /// Resolves `name` without playing it.
    pub fn choose(&mut self, name: &str) -> Option<Chosen> {
        self.sounds.choose(name)
    }

    /// Whether `name` is a soundscript entry (rather than a file).
    #[must_use]
    pub fn is_scripted(&self, name: &str) -> bool {
        self.sounds.is_scripted(name)
    }

    /// Starts loading `name`'s waves, so its first play is not late.
    pub fn preload(&mut self, name: &str) {
        let waves: Vec<String> = match self.sounds.scripts.get(name) {
            Some(entry) => entry.waves.clone(),
            None => self
                .sounds
                .choose(name)
                .map(|c| vec![c.path])
                .unwrap_or_default(),
        };
        for wave in waves {
            self.sounds.handle(&wave, &self.assets);
        }
    }
}

impl Sounds {
    /// The sounds of the game mounted in `vfs`: every soundscript its
    /// manifest lists.
    #[must_use]
    pub fn load(vfs: &Vfs) -> Self {
        let mut sounds = Self::default();
        let read = |path: &str| {
            vfs.read(path)
                .ok()
                .flatten()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        };
        let Some(manifest) = read("scripts/game_sounds_manifest.txt") else {
            warn!("no scripts/game_sounds_manifest.txt: named sounds are silent");
            return sounds;
        };
        for file in soundscript::manifest_files(&manifest) {
            if let Some(text) = read(&file)
                && sounds.scripts.add_script(&text).is_none()
            {
                warn!("unreadable soundscript {file}");
            }
        }
        info!("{} named sounds", sounds.scripts.len());
        sounds
    }
}

/// Reads the game's soundscripts.
fn load_soundscripts(vfs: Res<GameVfs>, mut sounds: ResMut<Sounds>) {
    *sounds = Sounds::load(&vfs.0);
}

/// Adds the map's own soundscript, if it has one.
fn load_level_sounds(vfs: Res<GameVfs>, map: Res<CurrentMap>, mut sounds: ResMut<Sounds>) {
    let path = format!("maps/{}_level_sounds.txt", map.0);
    if let Some(bytes) = vfs.0.read(&path).ok().flatten()
        && let Some(added) = sounds.scripts.add_script(&String::from_utf8_lossy(&bytes))
    {
        info!("{added} named sounds from {path}");
    }
}

/// Puts the listener's ears on the camera.
fn add_listener(
    mut commands: Commands,
    cameras: Query<Entity, (With<FlyCam>, Without<SpatialListener>)>,
) {
    for camera in &cameras {
        commands
            .entity(camera)
            .insert(SpatialListener::new(EAR_GAP));
    }
}

/// Applies a change of the volume settings to the sounds already playing.
fn apply_volumes(
    volumes: Volumes,
    mut plain: Query<(&Emitter, Option<&SoundCategory>, &mut AudioSink)>,
    mut spatial: Query<(&Emitter, Option<&SoundCategory>, &mut SpatialAudioSink)>,
) {
    if !volumes.is_changed() {
        return;
    }
    let level = |emitter: &Emitter, category: Option<&SoundCategory>| {
        Volume::Linear(emitter.volume * volumes.gain(category.copied().unwrap_or_default()))
    };
    for (emitter, category, mut sink) in &mut plain {
        sink.set_volume(level(emitter, category));
    }
    for (emitter, category, mut sink) in &mut spatial {
        sink.set_volume(level(emitter, category));
    }
}

/// Starts each emitter whose sound has loaded; drops one-shots that waited
/// too long and sounds that failed to load.
fn start_emitters(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<AssetServer>,
    loaded: Res<Assets<SoundAsset>>,
    volumes: Volumes,
    emitters: Query<(Entity, &Emitter, Option<&SoundCategory>), Without<Started>>,
) {
    let now = time.elapsed_secs();
    for (entity, emitter, category) in &emitters {
        let Some(sound) = loaded.get(&emitter.sound) else {
            let failed = assets.load_state(&emitter.sound).is_failed();
            let stale = emitter.repeat == Repeat::Once && now - emitter.requested > STALE_SECS;
            if failed || stale {
                commands.entity(entity).despawn();
            }
            continue;
        };
        let mode = match emitter.repeat {
            Repeat::Once => PlaybackMode::Despawn,
            Repeat::Loop => PlaybackMode::Loop,
            Repeat::AsRecorded if sound.looped => PlaybackMode::Loop,
            Repeat::AsRecorded => PlaybackMode::Remove,
        };
        let mut settings = PlaybackSettings {
            mode,
            volume: Volume::Linear(
                emitter.volume * volumes.gain(category.copied().unwrap_or_default()),
            ),
            speed: emitter.speed,
            ..PlaybackSettings::ONCE
        };
        if let Some(scale) = emitter.spatial {
            settings.spatial = true;
            settings.spatial_scale = Some(SpatialScale::new(scale));
        }
        commands.entity(entity).insert((
            AudioPlayer::<AudioSource>(sound.audio.clone()),
            settings,
            Started,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menus_follow_only_the_master_volume() {
        let audio = rl_config::AudioSettings {
            master: 0.5,
            effects: 0.2,
            ambient: 0.0,
        };
        assert_eq!(SoundCategory::Ui.gain(&audio), 0.5);
        assert_eq!(SoundCategory::Ambient.gain(&audio), 0.0);
    }

    #[test]
    fn louder_sounds_fade_later() {
        let gunfire = spatial_scale(140.0).expect("spatial");
        let talk = spatial_scale(60.0).expect("spatial");
        assert!(gunfire < talk, "a smaller scale reaches further");
        assert_eq!(spatial_scale(0.0), None, "heard everywhere");
        // SNDLVL_NORM: full volume (scaled distance 1) at √2 × 1250 units.
        let norm = spatial_scale(75.0).expect("spatial");
        let metres = std::f32::consts::SQRT_2 * 1250.0 / UNITS_PER_METRE;
        assert!((norm * metres - 1.0).abs() < 1e-4);
    }

    #[test]
    fn names_resolve_through_scripts_or_as_files() {
        let mut sounds = Sounds::default();
        sounds.scripts.add_script(
            r#""Test.Sound" { "volume" "0.5" "pitch" "50" "soundlevel" "SNDLVL_90dB" "wave" "^a/b.wav" }"#,
        );
        let chosen = sounds.choose("test.sound").expect("scripted");
        assert_eq!(
            chosen,
            Chosen {
                path: "a/b.wav".to_owned(),
                volume: 0.5,
                speed: 0.5,
                soundlevel: 90.0
            }
        );
        let file = sounds.choose("Ambient\\Hum.WAV").expect("a file");
        assert_eq!(file.path, "ambient/hum.wav");
        assert_eq!(sounds.choose("sound/x.mp3").expect("mp3").path, "x.mp3");
        assert!(sounds.choose("Unknown.Entry").is_none());
    }
}
