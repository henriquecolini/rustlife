//! Parsing `gameinfo.txt` and resolving its search paths.
//!
//! Each Source game directory (`hl2mp/`, `hl2/`, ...) has a `gameinfo.txt`
//! whose `FileSystem/SearchPaths` block lists, in priority order, every
//! location the engine searches for content. Entries look like
//!
//! ```text
//! game+mod        hl2mp/custom/*
//! game+mod        hl2mp/hl2mp_pak.vpk
//! game            |all_source_engine_paths|hl2/hl2_textures.vpk
//! mod+mod_write   |gameinfo_path|.
//! ```
//!
//! The key is a `+`-separated list of *path IDs* and the value a location
//! relative to the *base directory* (the folder holding the game executable),
//! with two macros: `|all_source_engine_paths|` (the base directory) and
//! `|gameinfo_path|` (the folder containing `gameinfo.txt`). A trailing `*`
//! means "each VPK and subdirectory in this folder", and a `.vpk` value names a
//! VPK set by its logical name (`pak.vpk` for `pak_dir.vpk`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::keyvalues;

/// The parts of `gameinfo.txt` rustlife uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInfo {
    /// The `game` display name.
    pub name: String,
    /// `FileSystem/SteamAppId`, if present.
    pub app_id: Option<u32>,
    /// Search paths in priority order, as written.
    pub search_paths: Vec<SearchPath>,
    /// Folder containing `gameinfo.txt` (the mod directory).
    pub gameinfo_dir: PathBuf,
    /// The base directory search paths are relative to.
    pub base_dir: PathBuf,
}

/// One `FileSystem/SearchPaths` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPath {
    /// Path IDs such as `game`, `mod`, `platform`, `download`.
    pub ids: Vec<String>,
    /// The location exactly as written.
    pub location: String,
}

impl SearchPath {
    /// Whether this entry serves game content (path ID `game` or `mod`), as
    /// opposed to binaries (`gamebin`) or platform UI files (`platform`).
    #[must_use]
    pub fn is_content(&self) -> bool {
        self.has_id("game") || self.has_id("mod")
    }

    /// Whether the entry carries the given path ID (case-insensitive).
    #[must_use]
    pub fn has_id(&self, id: &str) -> bool {
        self.ids
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(id))
    }
}

/// A search path resolved to concrete on-disk locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLocation {
    /// A directory of loose files.
    Directory(PathBuf),
    /// A VPK set, given by its logical path (`.../pak.vpk`).
    Vpk(PathBuf),
    /// Every VPK and subdirectory directly inside this folder, mounted in
    /// alphabetical order (the `custom/*` convention).
    Wildcard(PathBuf),
}

impl GameInfo {
    /// Loads `<gameinfo_dir>/gameinfo.txt`, taking the parent of
    /// `gameinfo_dir` as the base directory (true for every Steam install).
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or parsed, or lacks a `GameInfo` block.
    pub fn load(gameinfo_dir: &Path) -> Result<Self> {
        let file = gameinfo_dir.join("gameinfo.txt");
        let text = fs::read_to_string(&file).map_err(|e| Error::io(&file, e))?;
        let base_dir = gameinfo_dir.parent().unwrap_or(gameinfo_dir).to_path_buf();
        Self::parse(&text, gameinfo_dir.to_path_buf(), base_dir)
    }

    /// Parses `gameinfo.txt` text.
    ///
    /// # Errors
    ///
    /// Fails on syntax errors or a missing `GameInfo` block.
    pub fn parse(text: &str, gameinfo_dir: PathBuf, base_dir: PathBuf) -> Result<Self> {
        let file = gameinfo_dir.join("gameinfo.txt");
        let document = keyvalues::parse(text).map_err(|source| Error::KeyValues {
            path: file.clone(),
            source,
        })?;
        let info = document
            .root
            .get_block("GameInfo")
            .ok_or_else(|| Error::MissingKey {
                path: file.clone(),
                key: "GameInfo".into(),
            })?;

        let file_system = info.get_block("FileSystem");
        let search_paths = file_system
            .and_then(|fs| fs.get_block("SearchPaths"))
            .map(|block| {
                block
                    .iter()
                    .filter_map(|pair| {
                        Some(SearchPath {
                            ids: pair.key.split('+').map(str::to_owned).collect(),
                            location: pair.value.as_str()?.to_owned(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            name: info.get_str("game").unwrap_or_default().to_owned(),
            app_id: file_system
                .and_then(|fs| fs.get_str("SteamAppId"))
                .and_then(|id| id.trim().parse().ok()),
            search_paths,
            gameinfo_dir,
            base_dir,
        })
    }

    /// Resolves every *content* search path to on-disk locations, preserving
    /// priority order and dropping duplicates (HL2DM lists its own mod folder
    /// twice, once as `|gameinfo_path|.` and once as `hl2mp`).
    ///
    /// Locations are returned whether or not they exist; mounting skips the
    /// missing ones (HL2DM references `hl2mp_english.vpk`, which is not
    /// shipped).
    #[must_use]
    pub fn content_locations(&self) -> Vec<(SearchPath, ResolvedLocation)> {
        let mut seen: Vec<PathBuf> = Vec::new();
        let mut resolved = Vec::new();
        for search_path in self.search_paths.iter().filter(|sp| sp.is_content()) {
            let location = self.resolve(&search_path.location);
            let key = match &location {
                ResolvedLocation::Directory(p)
                | ResolvedLocation::Vpk(p)
                | ResolvedLocation::Wildcard(p) => {
                    fs::canonicalize(p).unwrap_or_else(|_| lexically_normalize(p))
                }
            };
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            resolved.push((search_path.clone(), location));
        }
        resolved
    }

    /// Expands macros and classifies one search path location.
    #[must_use]
    pub fn resolve(&self, location: &str) -> ResolvedLocation {
        let (root, rest) = if let Some(rest) = strip_macro(location, "|gameinfo_path|") {
            (self.gameinfo_dir.as_path(), rest)
        } else if let Some(rest) = strip_macro(location, "|all_source_engine_paths|") {
            (self.base_dir.as_path(), rest)
        } else {
            (self.base_dir.as_path(), location)
        };
        let rest = rest.replace('\\', "/");
        let rest = rest.trim_end_matches('/');

        if let Some(folder) = rest.strip_suffix('*') {
            return ResolvedLocation::Wildcard(join_relative(root, folder));
        }
        let path = join_relative(root, rest);
        if rest.to_ascii_lowercase().ends_with(".vpk") {
            ResolvedLocation::Vpk(path)
        } else {
            ResolvedLocation::Directory(path)
        }
    }
}

fn strip_macro<'a>(location: &'a str, name: &str) -> Option<&'a str> {
    let head = location.get(..name.len())?;
    head.eq_ignore_ascii_case(name)
        .then(|| &location[name.len()..])
}

fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative.split('/').filter(|s| !s.is_empty() && *s != ".") {
        path.push(segment);
    }
    path
}

/// Removes `.` components without touching the filesystem, for comparing
/// paths that do not exist.
fn lexically_normalize(path: &Path) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::SAMPLE_GAMEINFO;

    fn sample() -> GameInfo {
        GameInfo::parse(SAMPLE_GAMEINFO, "/games/dm/mod".into(), "/games/dm".into()).unwrap()
    }

    #[test]
    fn parses_name_app_id_and_search_paths() {
        let info = sample();
        assert_eq!(info.name, "Sample DM");
        assert_eq!(info.app_id, Some(320));
        assert_eq!(info.search_paths.len(), 9);
        assert_eq!(info.search_paths[0].ids, ["game", "mod"]);
        assert_eq!(info.search_paths[0].location, "mod/custom/*");
    }

    #[test]
    fn resolves_macros_wildcards_and_vpks() {
        let info = sample();
        assert_eq!(
            info.resolve("mod/custom/*"),
            ResolvedLocation::Wildcard("/games/dm/mod/custom".into())
        );
        assert_eq!(
            info.resolve("|all_source_engine_paths|shared/shared_pak.vpk"),
            ResolvedLocation::Vpk("/games/dm/shared/shared_pak.vpk".into())
        );
        assert_eq!(
            info.resolve("|gameinfo_path|."),
            ResolvedLocation::Directory("/games/dm/mod".into())
        );
        assert_eq!(
            info.resolve("|GAMEINFO_PATH|sub\\dir"),
            ResolvedLocation::Directory("/games/dm/mod/sub/dir".into())
        );
    }

    #[test]
    fn content_locations_skip_non_content_and_duplicates() {
        let locations = sample().content_locations();
        let paths: Vec<_> = locations
            .iter()
            .map(|(_, location)| location.clone())
            .collect();
        assert_eq!(
            paths,
            [
                ResolvedLocation::Wildcard("/games/dm/mod/custom".into()),
                ResolvedLocation::Vpk("/games/dm/mod/mod_pak.vpk".into()),
                ResolvedLocation::Vpk("/games/dm/shared/shared_pak.vpk".into()),
                // `|gameinfo_path|.` and `mod` are the same folder: listed once.
                ResolvedLocation::Directory("/games/dm/mod".into()),
                ResolvedLocation::Directory("/games/dm/shared".into()),
                ResolvedLocation::Directory("/games/dm/mod/download".into()),
            ]
        );
        assert!(locations.last().unwrap().0.has_id("download"));
    }

    #[test]
    fn missing_gameinfo_block_is_an_error() {
        let error = GameInfo::parse("\"Other\" {}", "/x".into(), "/".into()).unwrap_err();
        assert!(matches!(error, Error::MissingKey { .. }));
    }

    #[test]
    fn tolerates_missing_file_system_block() {
        let info = GameInfo::parse("GameInfo { game X }", "/x".into(), "/".into()).unwrap();
        assert_eq!(info.name, "X");
        assert!(info.search_paths.is_empty());
        assert_eq!(info.app_id, None);
    }
}
