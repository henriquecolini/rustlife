//! Locating Steam libraries, installed apps and workshop items.
//!
//! Steam keeps a list of *library folders* in
//! `<steam root>/steamapps/libraryfolders.vdf`. Each library has
//! `steamapps/appmanifest_<appid>.acf` files describing installed apps (whose
//! files live in `steamapps/common/<installdir>`) and a
//! `steamapps/workshop/content/<appid>/<itemid>/` directory per subscribed
//! workshop item.
//!
//! Nothing here talks to the Steam client; discovery is purely file-based, so
//! it works offline and with Steam closed.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::keyvalues::{self, ParseOptions, Value};

/// The set of Steam library folders found on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteamLibraries {
    libraries: Vec<PathBuf>,
}

/// An installed Steam app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInstall {
    /// Steam app id, e.g. `320` for Half-Life 2: Deathmatch.
    pub app_id: u32,
    /// Library folder the app is installed in.
    pub library: PathBuf,
    /// The app's install directory (`<library>/steamapps/common/<installdir>`).
    pub game_dir: PathBuf,
}

/// A downloaded workshop item.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkshopItem {
    /// App the item belongs to.
    pub app_id: u32,
    /// Published file id of the item.
    pub item_id: u64,
    /// Directory holding the item's files.
    pub path: PathBuf,
}

impl SteamLibraries {
    /// Discovers libraries from every Steam root that exists in the standard
    /// per-platform locations (see [`default_steam_roots`]).
    #[must_use]
    pub fn discover() -> Self {
        Self::from_roots(default_steam_roots())
    }

    /// Discovers libraries listed by the given Steam roots. Each root is itself
    /// treated as a library. Duplicates (including symlinked aliases such as
    /// `~/.steam/steam` -> `~/.local/share/Steam`) are removed.
    pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut libraries = Self::default();
        for root in roots {
            libraries.add(root.clone());
            for library in read_library_folders(&root) {
                libraries.add(library);
            }
        }
        libraries
    }

    /// Creates a library set from explicit library folders, without reading
    /// any `libraryfolders.vdf`.
    pub fn from_libraries(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut libraries = Self::default();
        for path in paths {
            libraries.add(path);
        }
        libraries
    }

    fn add(&mut self, path: PathBuf) {
        if !path.join("steamapps").is_dir() {
            return;
        }
        let canonical = fs::canonicalize(&path).unwrap_or(path);
        if !self.libraries.contains(&canonical) {
            self.libraries.push(canonical);
        }
    }

    /// The discovered library folders, in discovery order.
    #[must_use]
    pub fn libraries(&self) -> &[PathBuf] {
        &self.libraries
    }

    /// Finds an installed app by id, using the first library whose manifest
    /// lists it and whose install directory exists.
    #[must_use]
    pub fn find_app(&self, app_id: u32) -> Option<AppInstall> {
        self.libraries.iter().find_map(|library| {
            let manifest = library
                .join("steamapps")
                .join(format!("appmanifest_{app_id}.acf"));
            let text = fs::read_to_string(&manifest).ok()?;
            let document = keyvalues::parse_with(&text, &vdf_options()).ok()?;
            let install_dir = document.root.get_block("AppState")?.get_str("installdir")?;
            let game_dir = library.join("steamapps/common").join(install_dir);
            game_dir.is_dir().then(|| AppInstall {
                app_id,
                library: library.clone(),
                game_dir,
            })
        })
    }

    /// Lists downloaded workshop items for `app_id` across every library,
    /// sorted by item id. If an item appears in several libraries the first
    /// library wins.
    #[must_use]
    pub fn workshop_items(&self, app_id: u32) -> Vec<WorkshopItem> {
        let mut items = BTreeMap::new();
        for library in &self.libraries {
            let content = library
                .join("steamapps/workshop/content")
                .join(app_id.to_string());
            let Ok(entries) = fs::read_dir(&content) else {
                continue;
            };
            for entry in entries.flatten() {
                let Some(item_id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<u64>().ok())
                else {
                    continue;
                };
                if entry.path().is_dir() {
                    items.entry(item_id).or_insert_with(|| WorkshopItem {
                        app_id,
                        item_id,
                        path: entry.path(),
                    });
                }
            }
        }
        items.into_values().collect()
    }
}

/// VDF and ACF files written by Steam escape backslashes in Windows paths.
fn vdf_options() -> ParseOptions {
    ParseOptions {
        escapes: true,
        ..ParseOptions::default()
    }
}

/// Reads the library list from `<root>/steamapps/libraryfolders.vdf`.
///
/// Two layouts exist in the wild: the current one, where each numbered key
/// holds a block with a `path` entry, and the legacy one, where the numbered
/// key's value is the path itself.
fn read_library_folders(root: &Path) -> Vec<PathBuf> {
    let file = root.join("steamapps/libraryfolders.vdf");
    let Ok(text) = fs::read_to_string(&file) else {
        return Vec::new();
    };
    let document = match keyvalues::parse_with(&text, &vdf_options()) {
        Ok(document) => document,
        Err(error) => {
            tracing::warn!(file = %file.display(), %error, "ignoring malformed libraryfolders.vdf");
            return Vec::new();
        }
    };
    let Some(folders) = document.root.get_block("libraryfolders") else {
        return Vec::new();
    };
    folders
        .iter()
        .filter(|pair| pair.key.parse::<u32>().is_ok())
        .filter_map(|pair| match &pair.value {
            Value::String(path) => Some(PathBuf::from(path)),
            Value::Block(block) => block.get_str("path").map(PathBuf::from),
        })
        .collect()
}

/// Candidate Steam root directories for the current platform that exist.
///
/// * Linux: native (`~/.local/share/Steam`, `$XDG_DATA_HOME/Steam`), the legacy
///   `~/.steam/steam` and `~/.steam/root` symlinks, Flatpak and Snap installs.
/// * macOS: `~/Library/Application Support/Steam`.
/// * Windows: `Program Files (x86)\Steam` and `Program Files\Steam`. Custom
///   install locations recorded only in the registry are not detected; use the
///   `--game-dir` option or config file for those.
#[must_use]
pub fn default_steam_roots() -> Vec<PathBuf> {
    let home = home_dir();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "linux") {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            candidates.push(PathBuf::from(data_home).join("Steam"));
        }
        if let Some(home) = &home {
            for relative in [
                ".local/share/Steam",
                ".steam/steam",
                ".steam/root",
                ".var/app/com.valvesoftware.Steam/.local/share/Steam",
                "snap/steam/common/.local/share/Steam",
            ] {
                candidates.push(home.join(relative));
            }
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = &home {
            candidates.push(home.join("Library/Application Support/Steam"));
        }
    } else if cfg!(windows) {
        for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(dir) = std::env::var_os(variable) {
                candidates.push(PathBuf::from(dir).join("Steam"));
            }
        }
    }
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(variable)
        .filter(|v| !v.is_empty())
        .map(|v: OsString| PathBuf::from(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::SteamFixture;

    #[test]
    fn finds_apps_across_libraries() {
        let root = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut steam = SteamFixture::new(root.path());
        steam.add_library(second.path()).unwrap();
        let game = steam
            .install_app(second.path(), 320, "Half-Life 2 Deathmatch")
            .unwrap();

        let libraries = SteamLibraries::from_roots([root.path().to_path_buf()]);
        assert_eq!(libraries.libraries().len(), 2);
        let app = libraries.find_app(320).unwrap();
        assert_eq!(
            app.game_dir,
            fs::canonicalize(second.path())
                .unwrap()
                .join("steamapps/common/Half-Life 2 Deathmatch")
        );
        assert!(
            app.game_dir
                .ends_with(game.strip_prefix(second.path()).unwrap())
        );
        assert_eq!(libraries.find_app(220), None);
    }

    #[test]
    fn manifest_without_install_dir_on_disk_is_ignored() {
        let root = tempfile::tempdir().unwrap();
        let steam = SteamFixture::new(root.path());
        let game = steam.install_app(root.path(), 320, "Gone").unwrap();
        fs::remove_dir(&game).unwrap();
        assert_eq!(
            SteamLibraries::from_roots([root.path().to_path_buf()]).find_app(320),
            None
        );
    }

    #[test]
    fn reads_legacy_library_folder_format() {
        let root = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::create_dir_all(second.path().join("steamapps")).unwrap();
        fs::create_dir_all(root.path().join("steamapps")).unwrap();
        let escaped = second.path().display().to_string().replace('\\', "\\\\");
        fs::write(
            root.path().join("steamapps/libraryfolders.vdf"),
            format!("\"LibraryFolders\"\n{{\n\t\"TimeNextStatsReport\"\t\"1\"\n\t\"1\"\t\"{escaped}\"\n}}"),
        )
        .unwrap();
        let libraries = SteamLibraries::from_roots([root.path().to_path_buf()]);
        assert_eq!(libraries.libraries().len(), 2);
    }

    #[test]
    fn deduplicates_aliased_roots() {
        let root = tempfile::tempdir().unwrap();
        SteamFixture::new(root.path())
            .write_library_folders()
            .unwrap();
        let libraries =
            SteamLibraries::from_roots([root.path().to_path_buf(), root.path().join(".")]);
        assert_eq!(libraries.libraries().len(), 1);
    }

    #[test]
    fn lists_workshop_items_sorted_and_deduplicated() {
        let root = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut steam = SteamFixture::new(root.path());
        steam.add_library(second.path()).unwrap();
        steam.workshop_item(root.path(), 320, 30).unwrap();
        steam.workshop_item(root.path(), 320, 10).unwrap();
        steam.workshop_item(second.path(), 320, 10).unwrap();
        steam.workshop_item(second.path(), 220, 99).unwrap();
        fs::create_dir_all(
            root.path()
                .join("steamapps/workshop/content/320/not-a-number"),
        )
        .unwrap();

        let libraries = SteamLibraries::from_roots([root.path().to_path_buf()]);
        let items = libraries.workshop_items(320);
        let ids: Vec<_> = items.iter().map(|item| item.item_id).collect();
        assert_eq!(ids, [10, 30]);
        assert!(
            items[0]
                .path
                .starts_with(fs::canonicalize(root.path()).unwrap())
        );
        assert_eq!(libraries.workshop_items(220).len(), 1);
    }
}
