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
//! sound to load, then becomes a voice of the mixer (`rl_audio`, played
//! through Bevy's audio output by [`output`]): once (and despawns) or
//! looping (until its owner despawns it, which stops the voice). Emitters
//! at a place are heard as HL2DM hears them: fading with distance as their
//! sound level says, panned by their direction, and partly sent through
//! the room effect of the place ([`room`]); those of the local player's
//! own actions play just ahead of the listener.
//!
//! What plays when is split by source: game events (`events`), the map's
//! ambient sounds, moving doors and wall chargers (`world`), the
//! soundscape of where the listener is (`soundscape`), footsteps
//! (`steps`), the weapon list (`weapon_select`) and the menus' widgets
//! (`ui`), which sound in the main menu too, before any map is loaded.

use std::collections::HashMap;

use bevy::audio::{GlobalVolume, Volume};
use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::system::SystemParam;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;
use rl_assets::{SOURCE, SoundAsset, coords};
use rl_audio::{Looping, Place, VoiceId, VoiceParams};
use rl_sim::rng::Rng;
use source_formats::soundscript::{self, SoundScripts, WaveFlags};
use source_fs::Vfs;

use crate::settings::Settings;
use crate::world::MapScoped;
use crate::{AppState, CurrentMap, GameVfs};

pub mod output;
pub mod room;

mod events;
mod gravity_gun;
mod props;
mod soundscape;
mod steps;
mod ui;
mod view_model;
mod weapon_select;
mod world;

pub use output::MixerHandle;

/// Seconds a one-shot sound may wait to load before it is too late to
/// play.
const STALE_SECS: f32 = 0.5;
/// Seconds past its end a one-shot voice may go unreported (no audio
/// device pulling the mixer) before its emitter is despawned anyway.
const OVERDUE_SECS: f32 = 1.0;
/// Seconds between looks at which sounds a wall hides.
const OCCLUSION_INTERVAL: f32 = 0.1;
/// A trace that stops this close (units) to a sound has reached it: sounds
/// often sit on (or in) the surface that made them.
const OCCLUSION_SLACK: f32 = 16.0;

/// Plays the game's sounds.
#[derive(Debug, Clone, Copy, Default)]
pub struct SoundPlugin {
    /// Play every sound at volume zero this run (see [`Silent`]).
    pub silent: bool,
}

impl Plugin for SoundPlugin {
    fn build(&self, app: &mut App) {
        if self.silent {
            // Nothing should reach the speakers: the mixer's own stream is
            // zeroed through Bevy's volume, and the voices' gains go
            // through `Volumes` as well.
            app.insert_resource(Silent)
                .insert_resource(GlobalVolume::new(Volume::SILENT));
        }
        app.init_resource::<Sounds>()
            .add_systems(
                OnEnter(AppState::InGame),
                (load_soundscripts, load_level_sounds).chain(),
            )
            .add_systems(
                Update,
                (start_emitters, sync_voices, occlude_voices, finish_voices)
                    .chain()
                    .after(output::SetListener),
            )
            .add_plugins((
                output::OutputPlugin,
                room::RoomPlugin,
                events::EventSoundsPlugin,
                gravity_gun::GravityGunSoundsPlugin,
                props::PropSoundsPlugin,
                world::WorldSoundsPlugin,
                steps::StepsPlugin,
                weapon_select::WeaponSelectSoundsPlugin,
                view_model::ViewModelSoundsPlugin,
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
    /// What the wave's prefixes asked of the mixer.
    pub flags: WaveFlags,
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
                path: wave.path,
                volume: pick(&mut self.rng, entry.volume).clamp(0.0, 1.0),
                speed: (pick(&mut self.rng, entry.pitch) / 100.0).clamp(0.05, 4.0),
                soundlevel: entry.soundlevel,
                flags: wave.flags,
            });
        }
        let path = soundscript::clean_wave(name);
        let file = path.strip_prefix("sound/").unwrap_or(&path).to_owned();
        is_sound_file(&file).then_some(Chosen {
            path: file,
            volume: 1.0,
            speed: 1.0,
            soundlevel: soundscript::SNDLVL_NORM,
            flags: WaveFlags::of(name),
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

impl Repeat {
    fn looping(self) -> Looping {
        match self {
            Self::Once => Looping::Once,
            Self::Loop => Looping::Always,
            Self::AsRecorded => Looping::AsRecorded,
        }
    }
}

/// An entity that plays a sound once it has loaded: at its `Transform` if
/// [`Emitter::placed`], else with the listener. Change its volume, speed
/// or `Transform` while it plays and the sound follows.
#[derive(Component, Debug, Clone)]
pub struct Emitter {
    /// The sound.
    pub sound: Handle<SoundAsset>,
    /// Volume, `0..=1`.
    pub volume: f32,
    /// Playback speed.
    pub speed: f32,
    /// Sound level, decibels: how far it carries when placed (0: heard
    /// everywhere at full volume).
    pub soundlevel: f32,
    /// Whether it is at its `Transform` rather than with the listener.
    pub placed: bool,
    /// Its wave's mixer hints.
    pub flags: WaveFlags,
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
    /// The menus' widgets: only the master volume applies, and no room
    /// effect.
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
}

/// The mixer voice an emitter plays as. Removing it (or despawning the
/// emitter) stops the voice.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
#[component(on_remove = stop_voice)]
pub struct Voice(pub VoiceId);

fn stop_voice(world: DeferredWorld, context: HookContext) {
    let Some(voice) = world.get::<Voice>(context.entity).copied() else {
        return;
    };
    if let Some(mixer) = world.get_resource::<MixerHandle>() {
        mixer.with(|mixer| mixer.stop(voice.0));
    }
}

/// Marks an emitter whose sound has played out but stays (an ambient
/// sound that does not loop), so it is not started again.
#[derive(Component)]
struct PlayedOut;

/// Playing sounds, for any system.
#[derive(SystemParam)]
pub struct SoundPlayer<'w, 's> {
    commands: Commands<'w, 's>,
    sounds: ResMut<'w, Sounds>,
    assets: Res<'w, AssetServer>,
    time: Res<'w, Time>,
}

impl SoundPlayer<'_, '_> {
    /// Plays `name` once at `at` (Source space), or with the listener, at
    /// `volume` times its own. Returns the emitter.
    pub fn play(&mut self, name: &str, at: Option<Vec3>, volume: f32) -> Option<Entity> {
        self.spawn(name, at, volume, Repeat::Once)
    }

    /// Starts `name` playing at `at` (or with the listener) as `repeat`
    /// says; the caller despawns the emitter to stop it, and may move it.
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
        self.spawn_chosen(&chosen, at, volume, repeat)
    }

    /// Starts a resolved sound.
    pub fn spawn_chosen(
        &mut self,
        chosen: &Chosen,
        at: Option<Vec3>,
        volume: f32,
        repeat: Repeat,
    ) -> Option<Entity> {
        debug!(path = chosen.path, ?at, ?repeat, "sound");
        let transform = at.map_or_else(Transform::default, |at| {
            Transform::from_translation(coords::position(at.to_array()))
        });
        let emitter = self.emitter(chosen, volume, at.is_some(), repeat);
        Some(self.commands.spawn((emitter, transform, MapScoped)).id())
    }

    /// Plays `name` once with the listener for the menus: under the
    /// [`SoundCategory::Ui`] volume, and not cut off by leaving a map.
    pub fn play_ui(&mut self, name: &str, volume: f32) -> Option<Entity> {
        let Some(chosen) = self.sounds.choose(name) else {
            debug!("no sound named {name}");
            return None;
        };
        let emitter = self.emitter(&chosen, volume, false, Repeat::Once);
        let sound = (emitter, Transform::default(), SoundCategory::Ui);
        Some(self.commands.spawn(sound).id())
    }

    fn emitter(&mut self, chosen: &Chosen, volume: f32, placed: bool, repeat: Repeat) -> Emitter {
        Emitter {
            sound: self.sounds.handle(&chosen.path, &self.assets),
            volume: (chosen.volume * volume).clamp(0.0, 1.0),
            speed: chosen.speed,
            soundlevel: chosen.soundlevel,
            placed,
            flags: chosen.flags,
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
        self.commands.entity(emitter).try_insert(category);
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
            Some(entry) => entry.waves.iter().map(|wave| wave.path.clone()).collect(),
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

/// How an emitter's voice plays now.
fn voice_params(
    emitter: &Emitter,
    transform: &Transform,
    category: Option<&SoundCategory>,
    volumes: &Volumes,
) -> VoiceParams {
    let category = category.copied().unwrap_or_default();
    VoiceParams {
        volume: emitter.volume * volumes.gain(category),
        speed: emitter.speed,
        place: if emitter.placed {
            Place::World(Vec3::from_array(coords::to_source_position(
                transform.translation,
            )))
        } else {
            Place::Listener
        },
        soundlevel: emitter.soundlevel,
        flags: emitter.flags,
        room: category != SoundCategory::Ui,
        obscured: false,
        looping: emitter.repeat.looping(),
    }
}

/// An emitter waiting for its sound, and what it needs to start.
type Unstarted = (
    Entity,
    &'static Emitter,
    &'static Transform,
    Option<&'static SoundCategory>,
);

/// Starts each emitter whose sound has loaded as a voice; drops one-shots
/// that waited too long and sounds that failed to load.
fn start_emitters(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<AssetServer>,
    loaded: Res<Assets<SoundAsset>>,
    volumes: Volumes,
    mixer: Res<MixerHandle>,
    emitters: Query<Unstarted, (Without<Voice>, Without<PlayedOut>)>,
) {
    let now = time.elapsed_secs();
    for (entity, emitter, transform, category) in &emitters {
        let Some(sound) = loaded.get(&emitter.sound) else {
            let failed = assets.load_state(&emitter.sound).is_failed();
            let stale = emitter.repeat == Repeat::Once && now - emitter.requested > STALE_SECS;
            if failed || stale {
                commands.entity(entity).try_despawn();
            }
            continue;
        };
        let params = voice_params(emitter, transform, category, &volumes);
        let id = mixer.with(|mixer| mixer.play(sound.pcm.clone(), params));
        // A sound stopped this frame (a weapon-list sound cut off by the
        // next one, say) is gone by the time this lands: stop its voice.
        let mixer = mixer.clone();
        commands.queue(move |world: &mut World| {
            let started = world
                .get_entity_mut(entity)
                .map(|mut entity| {
                    entity.insert(Voice(id));
                })
                .is_ok();
            if !started {
                mixer.with(|mixer| mixer.stop(id));
            }
        });
    }
}

/// Hands every voice its emitter's current volume, speed and place.
fn sync_voices(
    volumes: Volumes,
    mixer: Res<MixerHandle>,
    emitters: Query<(&Emitter, &Transform, Option<&SoundCategory>, &Voice)>,
) {
    mixer.with(|mixer| {
        for (emitter, transform, category, voice) in &emitters {
            if let Some(params) = mixer.params_mut(voice.0) {
                let obscured = params.obscured;
                *params = voice_params(emitter, transform, category, &volumes);
                params.obscured = obscured;
            }
        }
    });
}

/// Marks the placed voices a wall stands between the listener and (HL2DM
/// lowers them by `snd_obscured_gain_dB`).
fn occlude_voices(
    time: Res<Time>,
    mut next: Local<f32>,
    game: crate::net::Game,
    mixer: Res<MixerHandle>,
    cameras: Query<&GlobalTransform, With<crate::camera::FlyCam>>,
    emitters: Query<(&Emitter, &Transform, &Voice)>,
) {
    let now = time.elapsed_secs();
    if now < *next {
        return;
    }
    *next = now + OCCLUSION_INTERVAL;
    let (Some(sim), Some(camera)) = (game.sim(), cameras.iter().next()) else {
        return;
    };
    let eye = Vec3::from_array(coords::to_source_position(camera.translation()));
    let world = sim.world();
    mixer.with(|mixer| {
        for (emitter, transform, voice) in &emitters {
            let Some(params) = mixer.params_mut(voice.0) else {
                continue;
            };
            params.obscured = emitter.placed && emitter.soundlevel > 0.0 && {
                let at = Vec3::from_array(coords::to_source_position(transform.translation));
                let trace = world.trace_ray(eye, at, rl_sim::collision::contents::MASK_SHOT);
                trace.hit() && trace.end.distance(at) > OCCLUSION_SLACK
            };
        }
    });
}

/// Despawns the emitters of voices that have played out (keeping ambient
/// ones, marked played out), and of one-shots long overdue when no audio
/// device is running the mixer.
fn finish_voices(
    mut commands: Commands,
    time: Res<Time>,
    loaded: Res<Assets<SoundAsset>>,
    mixer: Res<MixerHandle>,
    emitters: Query<(Entity, &Emitter, &Voice)>,
) {
    let finished = mixer.with(rl_audio::Mixer::take_finished);
    let now = time.elapsed_secs();
    for (entity, emitter, voice) in &emitters {
        let overdue = emitter.repeat == Repeat::Once
            && loaded.get(&emitter.sound).is_some_and(|sound| {
                now - emitter.requested
                    > sound.duration() / emitter.speed.max(0.05) + STALE_SECS + OVERDUE_SECS
            });
        if !finished.contains(&voice.0) && !overdue {
            continue;
        }
        match emitter.repeat {
            Repeat::AsRecorded => {
                commands.entity(entity).remove::<Voice>().insert(PlayedOut);
            }
            Repeat::Once | Repeat::Loop => commands.entity(entity).despawn(),
        }
    }
}

#[cfg(test)]
mod tests {
    use rl_audio::Pcm;

    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()));
        app.init_asset::<SoundAsset>();
        app.insert_resource(Settings(rl_config::Settings::default()));
        app.insert_resource(MixerHandle::default());
        app
    }

    fn sound(app: &mut App, frames: usize) -> Handle<SoundAsset> {
        app.world_mut()
            .resource_mut::<Assets<SoundAsset>>()
            .add(SoundAsset {
                pcm: Pcm::new(vec![0; frames], 1, 22050, None),
            })
    }

    fn emitter(sound: Handle<SoundAsset>, repeat: Repeat) -> Emitter {
        Emitter {
            sound,
            volume: 1.0,
            speed: 1.0,
            soundlevel: 75.0,
            placed: true,
            flags: WaveFlags::default(),
            repeat,
            requested: 0.0,
        }
    }

    /// Picking from the weapon list plays "selected" and, a frame later,
    /// "close", which cuts the first off. When that stop lands in the same
    /// frame as the first sound starts, the start must not panic on the
    /// emitter it can no longer find (it crashed the game), nor leave its
    /// voice playing.
    #[test]
    fn a_sound_stopped_the_frame_it_starts_is_skipped() {
        let mut app = app();
        let sound = sound(&mut app, 100);
        let emitter = app
            .world_mut()
            .spawn((emitter(sound, Repeat::Once), Transform::default()))
            .id();
        let stop = move |mut commands: Commands| commands.entity(emitter).try_despawn();
        // Both see the emitter, then the stop is applied first.
        app.add_systems(Update, (stop, start_emitters).chain_ignore_deferred());

        app.update();

        assert!(
            app.world().get_entity(emitter).is_err(),
            "the sound stays stopped"
        );
        let mixer = app.world().resource::<MixerHandle>().clone();
        assert_eq!(mixer.with(|mixer| mixer.voice_count()), 0);
    }

    #[test]
    fn despawning_an_emitter_stops_its_voice_and_played_out_ones_go() {
        let mut app = app();
        let sound = sound(&mut app, 100);
        app.add_systems(Update, (start_emitters, finish_voices).chain());
        let looping = app
            .world_mut()
            .spawn((emitter(sound.clone(), Repeat::Loop), Transform::default()))
            .id();
        let ambient = app
            .world_mut()
            .spawn((emitter(sound, Repeat::AsRecorded), Transform::default()))
            .id();
        app.update();
        let mixer = app.world().resource::<MixerHandle>().clone();
        assert_eq!(mixer.with(|mixer| mixer.voice_count()), 2);
        app.world_mut().despawn(looping);
        assert_eq!(mixer.with(|mixer| mixer.voice_count()), 1);
        // The mixer plays the ambient sound out.
        let mut out = vec![0.0; 4096];
        mixer.with(|mixer| mixer.render(&mut out));
        app.update();
        let entity = app.world().entity(ambient);
        assert!(entity.contains::<PlayedOut>() && !entity.contains::<Voice>());
        app.update();
        assert_eq!(mixer.with(|mixer| mixer.voice_count()), 0, "not restarted");
    }

    #[test]
    fn the_menus_follow_only_the_master_volume() {
        let audio = rl_config::AudioSettings {
            master: 0.5,
            effects: 0.2,
            ambient: 0.0,
            ..Default::default()
        };
        assert_eq!(SoundCategory::Ui.gain(&audio), 0.5);
        assert_eq!(SoundCategory::Ambient.gain(&audio), 0.0);
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
                soundlevel: 90.0,
                flags: WaveFlags::DISTANCE_VARIANT,
            }
        );
        let file = sounds.choose("Ambient\\Hum.WAV").expect("a file");
        assert_eq!(file.path, "ambient/hum.wav");
        assert_eq!(sounds.choose("sound/x.mp3").expect("mp3").path, "x.mp3");
        assert!(
            sounds
                .choose("#music/x.mp3")
                .expect("dry")
                .flags
                .contains(WaveFlags::DRY)
        );
        assert!(sounds.choose("Unknown.Entry").is_none());
    }
}
