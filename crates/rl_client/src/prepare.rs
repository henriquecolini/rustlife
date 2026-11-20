//! Making a game's files resident before the client reads them.
//!
//! In a browser the game's files arrive asynchronously (see
//! `source_fs::remote`), but a good part of the client reads them
//! synchronously: the sound scripts at startup, a map's surface materials
//! when it loads. Those reads can only succeed on files already in memory, so
//! the browser build fetches them first: [`startup`] once before the app
//! runs, and [`map`] before each map is entered.
//!
//! Native content reads synchronously, so for it all of this is a no-op.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, block_on, futures_lite::future};
use rl_data::GameData;
use source_formats::{soundscape, soundscript, vmt};
use source_fs::Vfs;

use crate::net::NetStatus;
use crate::pcf_particles::{PARTICLE_MANIFEST, particle_manifest_files};
use crate::player::Gameplay;
use crate::{AppState, CurrentMap, GameVfs};

/// Scripts the client reads once at startup.
const STARTUP_FILES: [&str; 5] = [
    "scripts/dsp_presets.txt",
    "scripts/surfaceproperties.txt",
    "scripts/game_sounds_manifest.txt",
    "scripts/soundscapes_manifest.txt",
    PARTICLE_MANIFEST,
];

/// Whether the game's files arrive asynchronously (a browser), so that a map
/// must be [prepared](map) before it is entered.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct AsyncContent(pub bool);

/// The state that starts a game on `map`: [`AppState::Preparing`] first when
/// content arrives asynchronously, else straight [`AppState::InGame`].
///
/// Preparing is repeated for every start, even of a map prepared before: files
/// made resident for another map may have been dropped to make room.
#[must_use]
pub(crate) fn state_for_game(content: AsyncContent) -> AppState {
    if content.0 {
        AppState::Preparing
    } else {
        AppState::InGame
    }
}

/// The file at `path` as text, if it is resident and exists.
fn text(vfs: &Vfs, path: &str) -> Option<String> {
    let bytes = vfs.read(path).ok().flatten()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Makes resident, and keeps, the files the client reads at startup: the
/// scripts above and every file their manifests list (sound scripts,
/// soundscapes, particle systems).
///
/// # Errors
///
/// Fails if the game's content cannot be fetched.
pub async fn startup(vfs: &Vfs) -> Result<(), String> {
    vfs.make_resident(&STARTUP_FILES, true)
        .await
        .map_err(|error| error.to_string())?;

    let mut listed = Vec::new();
    if let Some(manifest) = text(vfs, "scripts/game_sounds_manifest.txt") {
        listed.extend(soundscript::manifest_files(&manifest));
    }
    if let Some(manifest) = text(vfs, "scripts/soundscapes_manifest.txt") {
        listed.extend(soundscape::manifest_files(&manifest));
    }
    // The particle files the manifest lists, or every one without a manifest,
    // as the particle library loads them.
    let particles = text(vfs, PARTICLE_MANIFEST).and_then(|text| particle_manifest_files(&text));
    listed.extend(
        vfs.list("particles/*.pcf")
            .into_iter()
            .map(|file| file.path)
            .filter(|path| {
                particles
                    .as_ref()
                    .is_none_or(|files| files.contains(&path.to_ascii_lowercase()))
            }),
    );
    vfs.make_resident(&listed, true)
        .await
        .map_err(|error| error.to_string())?;
    // Reads made while nothing was resident yet are not worth reporting.
    vfs.take_misses();
    Ok(())
}

/// Makes resident everything entering map `name` reads: its level (the map,
/// its models and their collision), the map's own sound script, and the
/// materials its surfaces are classified by.
///
/// # Errors
///
/// Fails if the map cannot be found or loaded.
pub async fn map(vfs: &Vfs, name: &str, data: &GameData) -> Result<(), String> {
    rl_level::load_async(vfs, name, data)
        .await
        .map_err(|error| error.to_string())?;

    let mut wanted = BTreeSet::from([format!("maps/{name}_level_sounds.txt")]);
    let materials: BTreeSet<String> = vfs
        .read(&format!("maps/{name}.bsp"))
        .ok()
        .flatten()
        .and_then(|bsp| source_formats::collision::load(&bsp).ok())
        .map(|collision| {
            collision
                .materials
                .iter()
                .map(|material| format!("materials/{}.vmt", material.replace('\\', "/")))
                .collect()
        })
        .unwrap_or_default();
    vfs.make_resident(&materials.iter().collect::<Vec<_>>(), false)
        .await
        .map_err(|error| error.to_string())?;
    // A material that patches another is read together with its base.
    for material in &materials {
        if let Some(Some(base)) = text(vfs, material).map(|text| vmt::patch_include(&text).ok()) {
            wanted.extend(base.map(|base| base.replace('\\', "/")));
        }
    }
    vfs.make_resident(&wanted.iter().collect::<Vec<_>>(), false)
        .await
        .map_err(|error| error.to_string())?;
    vfs.take_misses();
    Ok(())
}

/// Fetches a map's files in the background, for a map the server is about to
/// change to: the server's loader asks it, gets "not yet" until the files are
/// there, then loads the map for real.
#[derive(Clone)]
pub(crate) struct Fetcher {
    vfs: Arc<Vfs>,
    data: GameData,
    state: Arc<Mutex<FetchState>>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum FetchState {
    #[default]
    Idle,
    /// Fetching this map.
    Fetching(String),
    /// This map's files are resident.
    Ready(String),
    /// Fetching this map failed.
    Failed(String, String),
}

/// What a request to fetch a map comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fetched {
    /// Resident now.
    Ready,
    /// On its way; ask again later.
    Pending,
    /// It cannot be had.
    Failed(String),
}

impl Fetcher {
    pub(crate) fn new(vfs: Arc<Vfs>, data: GameData) -> Self {
        Self {
            vfs,
            data,
            state: Arc::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FetchState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Where fetching `name` stands, starting it if it has not been.
    pub(crate) fn request(&self, name: &str) -> Fetched {
        let mut state = self.state();
        match &*state {
            FetchState::Ready(ready) if ready == name => return Fetched::Ready,
            FetchState::Failed(failed, reason) if failed == name => {
                return Fetched::Failed(reason.clone());
            }
            FetchState::Fetching(_) => return Fetched::Pending,
            _ => {}
        }
        *state = FetchState::Fetching(name.to_owned());
        drop(state);
        let this = self.clone();
        let name = name.to_owned();
        IoTaskPool::get()
            .spawn(async move {
                let outcome = map(&this.vfs, &name, &this.data).await;
                *this.state() = match outcome {
                    Ok(()) => FetchState::Ready(name),
                    Err(reason) => FetchState::Failed(name, reason),
                };
            })
            .detach();
        Fetched::Pending
    }

    /// Forgets a finished fetch, so a later one starts again.
    pub(crate) fn reset(&self) {
        let mut state = self.state();
        if !matches!(&*state, FetchState::Fetching(_)) {
            *state = FetchState::Idle;
        }
    }
}

/// The map being prepared before it is entered.
#[derive(Resource, Default)]
struct Preparing(Option<Task<Result<(), String>>>);

/// Prepares a map's files when the game is entered from the menu, in
/// [`AppState::Preparing`].
pub(crate) struct PreparePlugin {
    /// Whether content arrives asynchronously; otherwise nothing is done.
    pub(crate) content: AsyncContent,
}

impl Plugin for PreparePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.content)
            .init_resource::<Preparing>()
            .add_systems(OnEnter(AppState::Preparing), begin)
            .add_systems(Update, finish.run_if(in_state(AppState::Preparing)))
            .add_systems(
                OnExit(AppState::Preparing),
                |mut preparing: ResMut<Preparing>| {
                    preparing.0 = None;
                },
            );
        if self.content.0 {
            app.add_systems(Update, warn_of_misses);
        }
    }
}

fn begin(
    mut preparing: ResMut<Preparing>,
    vfs: Res<GameVfs>,
    map_name: Res<CurrentMap>,
    gameplay: Res<Gameplay>,
) {
    let (vfs, name, data) = (Arc::clone(&vfs.0), map_name.0.clone(), gameplay.0.clone());
    preparing.0 = Some(IoTaskPool::get().spawn(async move { map(&vfs, &name, &data).await }));
}

fn finish(
    mut preparing: ResMut<Preparing>,
    mut next: ResMut<NextState<AppState>>,
    mut status: ResMut<NetStatus>,
) {
    let Some(task) = preparing.0.as_mut() else {
        return;
    };
    let Some(outcome) = block_on(future::poll_once(task)) else {
        return;
    };
    match outcome {
        Ok(()) => next.set(AppState::InGame),
        Err(reason) => {
            warn!("cannot prepare the map: {reason}");
            *status = NetStatus::Failed(reason);
            next.set(AppState::MainMenu);
        }
    }
}

/// With content that arrives asynchronously, a synchronous read of a file
/// that was not prepared fails; say which, since it is a missed
/// preparation, not a missing file.
fn warn_of_misses(vfs: Res<GameVfs>) {
    for path in vfs.0.take_misses() {
        warn!("{path} was read before it was loaded");
    }
}

#[cfg(test)]
mod tests {
    use bevy::tasks::block_on;
    use source_fs::remote::{DEFAULT_RESIDENT_BUDGET, DirectoryTree, mount_remote_game};

    use super::*;

    /// With the install mounted the way the browser mounts it, after
    /// [`startup`] every synchronous startup read finds its file.
    #[test]
    #[ignore = "requires an HL2DM install; set RUSTLIFE_TEST_GAME_DIR"]
    fn startup_makes_every_startup_read_succeed() {
        let Ok(dir) = std::env::var("RUSTLIFE_TEST_GAME_DIR") else {
            return;
        };
        let vfs = Vfs::new();
        let tree = DirectoryTree::open(std::path::Path::new(&dir)).expect("index the install");
        block_on(mount_remote_game(
            &vfs,
            &tree,
            "hl2mp",
            DEFAULT_RESIDENT_BUDGET,
        ))
        .expect("mount the install");
        block_on(startup(&vfs)).expect("prepare the startup files");

        let mut missing = Vec::new();
        for path in STARTUP_FILES {
            if !matches!(vfs.read(path), Ok(Some(_))) {
                missing.push(path.to_owned());
            }
        }
        let sounds = text(&vfs, "scripts/game_sounds_manifest.txt").expect("sound manifest");
        for path in soundscript::manifest_files(&sounds) {
            if !matches!(vfs.read(&path), Ok(Some(_))) {
                missing.push(path);
            }
        }
        let manifest = text(&vfs, PARTICLE_MANIFEST).expect("particle manifest");
        for path in particle_manifest_files(&manifest).expect("a readable manifest") {
            if !matches!(vfs.read(&path), Ok(Some(_))) {
                missing.push(path);
            }
        }
        assert!(
            missing.is_empty(),
            "not resident after startup: {missing:?}"
        );
        assert!(vfs.take_misses().is_empty());
    }
}
