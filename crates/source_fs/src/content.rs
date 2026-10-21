//! Assembling a complete [`Vfs`] for an installed game.
//!
//! [`mount_game`] is the one-call entry point used by the client, server and
//! tools: it locates the install, reads `gameinfo.txt`, and mounts everything
//! in the order the engine would, plus installed workshop items.
//!
//! # Mount order (highest priority first)
//!
//! 1. Wildcard search paths (`<mod>/custom/*`), each VPK or subfolder in
//!    alphabetical order.
//! 2. Workshop items: the game's own app id first, then each of
//!    [`WorkshopOptions::extra_apps`] in the order given; within an app, by item
//!    id. Placing them below `custom/` keeps a user's deliberate local overrides
//!    on top, as the engine does.
//! 3. The remaining `gameinfo.txt` content search paths, in file order. This
//!    ends with `<mod>/download`, which is labelled [`MountOrigin::Download`].
//!
//! A map's pakfile is mounted on top of all of these at map load time by the
//! map loader, not here.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::gameinfo::{GameInfo, ResolvedLocation};
use crate::games::GameDescriptor;
use crate::loose::LooseDirectory;
use crate::steam::{SteamLibraries, WorkshopItem};
use crate::vfs::{MountOrigin, Vfs};
use crate::vpk::VpkArchive;

/// Steam app id of Half-Life 2: Deathmatch.
pub const HL2DM_APP_ID: u32 = 320;

/// Mod directory of Half-Life 2: Deathmatch inside its install folder.
pub const HL2DM_MOD_DIR: &str = "hl2mp";

/// Which installed workshop content to mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkshopOptions {
    /// Master switch for all workshop content.
    pub enabled: bool,
    /// Additional app ids whose workshop items are mounted after the game's
    /// own, e.g. `[220]` for Half-Life 2 items. Off by default because content
    /// made for other games can override files unexpectedly.
    pub extra_apps: Vec<u32>,
    /// Item ids that are never mounted.
    pub disabled_items: Vec<u64>,
}

impl Default for WorkshopOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            extra_apps: Vec::new(),
            disabled_items: Vec::new(),
        }
    }
}

/// Where to find the game and what to mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentOptions {
    /// Explicit install folder (containing `hl2mp/`). When `None`, Steam
    /// libraries are searched for [`ContentOptions::app_id`].
    pub game_dir: Option<PathBuf>,
    /// Steam app id of the game.
    pub app_id: u32,
    /// Mod directory holding `gameinfo.txt`.
    pub mod_dir: String,
    /// Workshop selection.
    pub workshop: WorkshopOptions,
    /// Steam libraries to search. `None` means [`SteamLibraries::discover`].
    pub steam: Option<SteamLibraries>,
}

impl Default for ContentOptions {
    fn default() -> Self {
        Self {
            game_dir: None,
            app_id: HL2DM_APP_ID,
            mod_dir: HL2DM_MOD_DIR.to_owned(),
            workshop: WorkshopOptions::default(),
            steam: None,
        }
    }
}

impl ContentOptions {
    /// Options for mounting `game`, with its default workshop selection.
    #[must_use]
    pub fn for_game(game: &GameDescriptor) -> Self {
        Self {
            app_id: game.app_id,
            mod_dir: game.mod_dir.to_owned(),
            ..Self::default()
        }
    }
}

/// The result of [`mount_game`].
#[derive(Debug)]
pub struct MountedGame {
    /// The assembled filesystem.
    pub vfs: Vfs,
    /// What was mounted into it.
    pub info: MountedInfo,
}

impl std::ops::Deref for MountedGame {
    type Target = MountedInfo;

    fn deref(&self) -> &MountedInfo {
        &self.info
    }
}

/// What [`mount_game_into`] mounted.
#[derive(Debug)]
pub struct MountedInfo {
    /// The install folder the game was found in.
    pub game_dir: PathBuf,
    /// The parsed `gameinfo.txt`.
    pub gameinfo: GameInfo,
    /// Workshop items that were mounted.
    pub workshop_items: Vec<WorkshopItem>,
}

/// Locates the game and mounts all of its content.
///
/// Individual locations that are missing or unreadable are skipped with a log
/// message, so a single broken workshop item cannot prevent the game from
/// starting.
///
/// # Errors
///
/// Returns [`Error::GameNotFound`] if the install cannot be located, or an
/// error if its `gameinfo.txt` is unreadable.
pub fn mount_game(options: &ContentOptions) -> Result<MountedGame> {
    let vfs = Vfs::new();
    let info = mount_game_into(&vfs, options)?;
    Ok(MountedGame { vfs, info })
}

/// Like [`mount_game`], but mounts below the existing mounts of `vfs`, so
/// several games can share one filesystem and a live one can be remounted.
///
/// # Errors
///
/// See [`mount_game`].
pub fn mount_game_into(vfs: &Vfs, options: &ContentOptions) -> Result<MountedInfo> {
    let steam = options
        .steam
        .clone()
        .unwrap_or_else(SteamLibraries::discover);
    let game_dir = locate_game_dir(options, &steam)?;
    let gameinfo = GameInfo::load(&game_dir.join(&options.mod_dir))?;
    tracing::info!(game = %gameinfo.name, dir = %game_dir.display(), "mounting game content");

    let workshop_items = if options.workshop.enabled {
        std::iter::once(options.app_id)
            .chain(
                options
                    .workshop
                    .extra_apps
                    .iter()
                    .copied()
                    .filter(|&id| id != options.app_id),
            )
            .flat_map(|app_id| steam.workshop_items(app_id))
            .filter(|item| !options.workshop.disabled_items.contains(&item.item_id))
            .collect()
    } else {
        Vec::new()
    };

    let locations = gameinfo.content_locations();
    let wildcard_count = locations
        .iter()
        .take_while(|(_, location)| matches!(location, ResolvedLocation::Wildcard(_)))
        .count();
    let (wildcards, rest) = locations.split_at(wildcard_count);

    for (_, location) in wildcards {
        mount_location(vfs, location, &MountOrigin::Custom);
    }
    for item in &workshop_items {
        mount_workshop_item(vfs, item);
    }
    for (search_path, location) in rest {
        let origin = if search_path.has_id("download") {
            MountOrigin::Download
        } else if matches!(location, ResolvedLocation::Wildcard(_)) {
            MountOrigin::Custom
        } else {
            MountOrigin::BaseGame
        };
        mount_location(vfs, location, &origin);
    }

    Ok(MountedInfo {
        game_dir,
        gameinfo,
        workshop_items,
    })
}

/// One game to mount: which, and how to find it.
#[derive(Debug, Clone)]
pub struct GamePlan {
    /// The game.
    pub game: GameDescriptor,
    /// Where it is and which workshop content comes with it.
    pub options: ContentOptions,
}

/// The outcome of mounting one [`GamePlan`].
#[derive(Debug)]
pub struct GameMount {
    /// The game.
    pub game: GameDescriptor,
    /// What was mounted, or why the game could not be.
    pub outcome: Result<MountedInfo>,
}

/// Mounts every game in `plans` into `vfs`, in order (earlier games take
/// priority), and reports each one's outcome. A game that is not installed
/// does not stop the others from mounting.
pub fn mount_games(vfs: &Vfs, plans: &[GamePlan]) -> Vec<GameMount> {
    plans
        .iter()
        .map(|plan| GameMount {
            game: plan.game,
            outcome: mount_game_into(vfs, &plan.options),
        })
        .collect()
}

fn locate_game_dir(options: &ContentOptions, steam: &SteamLibraries) -> Result<PathBuf> {
    let gameinfo_exists = |dir: &Path| dir.join(&options.mod_dir).join("gameinfo.txt").is_file();
    if let Some(dir) = &options.game_dir {
        return if gameinfo_exists(dir) {
            Ok(dir.clone())
        } else {
            Err(Error::GameNotFound(format!(
                "{} does not contain {}/gameinfo.txt; point the game directory at the \
                 Half-Life 2: Deathmatch install folder",
                dir.display(),
                options.mod_dir
            )))
        };
    }
    steam
        .find_app(options.app_id)
        .map(|app| app.game_dir)
        .filter(|dir| gameinfo_exists(dir))
        .ok_or_else(|| {
            Error::GameNotFound(format!(
                "could not find an installation of Steam app {} in any Steam library \
                 (searched {} libraries). Install the game through Steam, or point rustlife \
                 at its install folder",
                options.app_id,
                steam.libraries().len()
            ))
        })
}

fn mount_location(vfs: &Vfs, location: &ResolvedLocation, origin: &MountOrigin) {
    match location {
        ResolvedLocation::Directory(dir) => mount_directory(vfs, dir, origin),
        ResolvedLocation::Vpk(path) => mount_vpk(vfs, path, origin),
        ResolvedLocation::Wildcard(dir) => {
            for entry in sorted_entries(dir) {
                if entry.is_dir() {
                    mount_directory(vfs, &entry, origin);
                } else if is_mountable_vpk(&entry) {
                    mount_vpk(vfs, &entry, origin);
                }
            }
        }
    }
}

/// A workshop item may ship VPKs (`workshop_dir.vpk`), loose content folders,
/// or both. VPKs are mounted first, then the item folder itself if it holds
/// anything besides VPKs.
fn mount_workshop_item(vfs: &Vfs, item: &WorkshopItem) {
    let origin = MountOrigin::Workshop {
        app_id: item.app_id,
        item_id: item.item_id,
    };
    let entries = sorted_entries(&item.path);
    for vpk in entries.iter().filter(|entry| is_mountable_vpk(entry)) {
        mount_vpk(vfs, vpk, &origin);
    }
    let has_loose_content = entries.iter().any(|entry| {
        entry.is_dir() || entry.extension().is_none_or(|extension| extension != "vpk")
    });
    if has_loose_content {
        mount_directory(vfs, &item.path, &origin);
    }
}

fn mount_directory(vfs: &Vfs, dir: &Path, origin: &MountOrigin) {
    if !dir.is_dir() {
        tracing::debug!(dir = %dir.display(), "search path directory does not exist; skipping");
        return;
    }
    match LooseDirectory::open(dir) {
        Ok(mount) => {
            vfs.mount_back(mount, origin.clone());
        }
        Err(error) => tracing::warn!(%error, "skipping unreadable directory"),
    }
}

fn mount_vpk(vfs: &Vfs, path: &Path, origin: &MountOrigin) {
    match VpkArchive::open(path) {
        Ok(vpk) => {
            vfs.mount_back(vpk, origin.clone());
        }
        Err(Error::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(vpk = %path.display(), "search path VPK does not exist; skipping");
        }
        Err(error) => tracing::warn!(%error, "skipping unreadable VPK"),
    }
}

/// Whether a file is the entry point of a VPK set: a `*_dir.vpk`, or a
/// standalone `.vpk` that is not one of the numbered data archives.
fn is_mountable_vpk(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".vpk") else {
        return false;
    };
    if stem.ends_with("_dir") {
        return true;
    }
    // `<stem>_NNN.vpk` is a data archive belonging to `<stem>_dir.vpk`.
    let is_numbered_archive = stem
        .rsplit_once('_')
        .is_some_and(|(_, suffix)| suffix.len() == 3 && suffix.bytes().all(|b| b.is_ascii_digit()));
    !is_numbered_archive
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{SAMPLE_GAMEINFO, SteamFixture, VpkBuilder};

    /// Builds a fake install of the sample game with content in every kind of
    /// location, returning (steam root guard, options).
    fn fixture() -> (tempfile::TempDir, ContentOptions) {
        let tmp = tempfile::tempdir().unwrap();
        let steam = SteamFixture::new(tmp.path());
        let game = steam.install_app(tmp.path(), 320, "Sample").unwrap();
        let mod_dir = game.join("mod");
        fs::create_dir_all(mod_dir.join("custom/my_override/materials")).unwrap();
        fs::create_dir_all(mod_dir.join("download/maps")).unwrap();
        fs::create_dir_all(mod_dir.join("maps")).unwrap();
        fs::create_dir_all(game.join("shared/materials")).unwrap();
        fs::write(mod_dir.join("gameinfo.txt"), SAMPLE_GAMEINFO).unwrap();

        // Same file at several priorities to verify ordering.
        fs::write(
            mod_dir.join("custom/my_override/materials/shared.vmt"),
            "custom",
        )
        .unwrap();
        VpkBuilder::new()
            .file("materials/shared.vmt", b"mod_pak")
            .file("materials/mod_only.vmt", b"mod_pak")
            .write(&mod_dir, "mod_pak")
            .unwrap();
        VpkBuilder::new()
            .file("materials/shared.vmt", b"shared_pak")
            .file("materials/workshop.vmt", b"shared_pak")
            .write(&game.join("shared"), "shared_pak")
            .unwrap();
        fs::write(game.join("shared/materials/loose.vmt"), "loose").unwrap();
        fs::write(mod_dir.join("maps/dm_base.bsp"), "bsp").unwrap();
        fs::write(mod_dir.join("download/maps/dm_downloaded.bsp"), "bsp").unwrap();

        let item = steam.workshop_item(tmp.path(), 320, 1234).unwrap();
        VpkBuilder::new()
            .file("materials/workshop.vmt", b"workshop")
            .file("maps/dm_workshop.bsp", b"bsp")
            .write(&item, "workshop")
            .unwrap();
        let hl2_item = steam.workshop_item(tmp.path(), 220, 99).unwrap();
        fs::create_dir_all(hl2_item.join("maps")).unwrap();
        fs::write(hl2_item.join("maps/hl2_map.bsp"), "bsp").unwrap();

        let options = ContentOptions {
            mod_dir: "mod".into(),
            steam: Some(SteamLibraries::from_roots([tmp.path().to_path_buf()])),
            ..ContentOptions::default()
        };
        (tmp, options)
    }

    #[test]
    fn mounts_in_engine_priority_order() {
        let (_tmp, options) = fixture();
        let game = mount_game(&options).unwrap();
        let vfs = &game.vfs;
        assert_eq!(game.gameinfo.name, "Sample DM");
        assert_eq!(
            vfs.read_string("materials/shared.vmt").unwrap().unwrap(),
            "custom"
        );
        assert_eq!(
            vfs.read_string("materials/workshop.vmt").unwrap().unwrap(),
            "workshop"
        );
        assert_eq!(
            vfs.read_string("materials/mod_only.vmt").unwrap().unwrap(),
            "mod_pak"
        );
        assert_eq!(
            vfs.read_string("materials/loose.vmt").unwrap().unwrap(),
            "loose"
        );

        let origins: Vec<_> = vfs.mount_list().into_iter().map(|m| m.origin).collect();
        assert_eq!(origins.first(), Some(&MountOrigin::Custom));
        assert_eq!(
            origins[1],
            MountOrigin::Workshop {
                app_id: 320,
                item_id: 1234
            }
        );
        assert_eq!(origins.last(), Some(&MountOrigin::Download));
    }

    #[test]
    fn mounts_into_an_existing_filesystem_and_can_be_cleared() {
        let (_tmp, options) = fixture();
        let vfs = Vfs::new();
        let info = mount_game_into(&vfs, &options).unwrap();
        assert!(info.game_dir.ends_with("Sample"), "{:?}", info.game_dir);
        assert!(vfs.exists("maps/dm_base.bsp"));

        vfs.clear();
        assert!(!vfs.exists("maps/dm_base.bsp"));
        mount_game_into(&vfs, &options).unwrap();
        assert!(
            vfs.exists("maps/dm_base.bsp"),
            "remounting restores content"
        );
    }

    #[test]
    fn labels_maps_by_origin() {
        let (_tmp, options) = fixture();
        let game = mount_game(&options).unwrap();
        let maps: Vec<_> = game
            .vfs
            .list("maps/*.bsp")
            .into_iter()
            .map(|f| (f.path, f.origin))
            .collect();
        assert_eq!(
            maps,
            [
                ("maps/dm_base.bsp".to_owned(), MountOrigin::BaseGame),
                ("maps/dm_downloaded.bsp".to_owned(), MountOrigin::Download),
                (
                    "maps/dm_workshop.bsp".to_owned(),
                    MountOrigin::Workshop {
                        app_id: 320,
                        item_id: 1234
                    }
                ),
            ]
        );
    }

    #[test]
    fn extra_workshop_apps_are_opt_in() {
        let (_tmp, mut options) = fixture();
        assert!(!mount_game(&options).unwrap().vfs.exists("maps/hl2_map.bsp"));

        options.workshop.extra_apps = vec![220];
        let game = mount_game(&options).unwrap();
        assert_eq!(game.workshop_items.len(), 2);
        assert_eq!(
            game.vfs.locate("maps/hl2_map.bsp"),
            Some(MountOrigin::Workshop {
                app_id: 220,
                item_id: 99
            })
        );
    }

    #[test]
    fn workshop_can_be_disabled_globally_or_per_item() {
        let (_tmp, mut options) = fixture();
        options.workshop.disabled_items = vec![1234];
        assert!(
            !mount_game(&options)
                .unwrap()
                .vfs
                .exists("maps/dm_workshop.bsp")
        );

        options.workshop = WorkshopOptions {
            enabled: false,
            ..WorkshopOptions::default()
        };
        assert!(mount_game(&options).unwrap().workshop_items.is_empty());
    }

    #[test]
    fn explicit_game_dir_overrides_steam() {
        let (tmp, mut options) = fixture();
        options.game_dir = Some(tmp.path().join("steamapps/common/Sample"));
        options.steam = Some(SteamLibraries::default());
        assert!(mount_game(&options).is_ok());

        options.game_dir = Some(tmp.path().to_path_buf());
        let error = mount_game(&options).unwrap_err();
        assert!(matches!(error, Error::GameNotFound(_)));
    }

    #[test]
    fn missing_install_reports_game_not_found() {
        let options = ContentOptions {
            steam: Some(SteamLibraries::default()),
            ..ContentOptions::default()
        };
        let error = mount_game(&options).unwrap_err();
        assert!(error.to_string().contains("320"), "{error}");
    }

    #[test]
    fn recognises_vpk_entry_points() {
        assert!(is_mountable_vpk(Path::new("pak01_dir.vpk")));
        assert!(is_mountable_vpk(Path::new("standalone.vpk")));
        assert!(is_mountable_vpk(Path::new("WORKSHOP_DIR.VPK")));
        assert!(!is_mountable_vpk(Path::new("pak01_000.vpk")));
        assert!(!is_mountable_vpk(Path::new("readme.txt")));
    }
}
