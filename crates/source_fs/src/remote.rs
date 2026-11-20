//! Content that arrives asynchronously: VPKs and loose files read through
//! [`ByteSource`]s, and the assembly of a whole game from a [`RemoteTree`].
//!
//! This is the browser's counterpart of [`crate::vpk::VpkArchive`],
//! [`crate::loose::LooseDirectory`] and [`crate::content::mount_game`]: the
//! same VPK format, the same `gameinfo.txt` search paths and the same mount
//! order, but every byte is fetched asynchronously from wherever the platform
//! keeps the player's files. Nothing here knows about browsers, so it is all
//! tested natively, over [`crate::MemorySource`] and [`DirectoryTree`].
//!
//! # Synchronous reads
//!
//! [`Mount::read`] cannot wait. A remote mount therefore answers it only for
//! files that are *resident* (see [`Mount::make_resident`]) and otherwise
//! fails with [`Error::NotResident`]; [`Mount::read_async`] always works.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::byte_source::{BoxFuture, ByteSource, FileSource};
use crate::content::is_mountable_vpk;
use crate::error::{Error, Result};
use crate::gameinfo::{GameInfo, ResolvedLocation};
use crate::mount::Mount;
use crate::path::normalize;
use crate::resident::ResidentCache;
use crate::vfs::{MountOrigin, Vfs};
use crate::vpk::{DIR_ARCHIVE_INDEX, VpkEntry, VpkHeader, VpkIndex};

/// How many bytes of files a remote mount keeps resident, unpinned, unless
/// told otherwise.
pub const DEFAULT_RESIDENT_BUDGET: usize = 128 * 1024 * 1024;

/// A VPK set whose directory and data archives are [`ByteSource`]s.
#[derive(Debug)]
pub struct RemoteVpk {
    index: VpkIndex,
    dir: Arc<dyn ByteSource>,
    archives: HashMap<u16, Arc<dyn ByteSource>>,
    resident: ResidentCache,
}

impl RemoteVpk {
    /// Opens the set whose directory file is `dir`, reading only its header
    /// and tree. `archives` holds the numbered data archives by index
    /// (`<stem>_000.vpk` is 0); a set with none, or with data embedded in the
    /// directory, passes an empty map. At most `budget` bytes of files stay
    /// resident at once, not counting pinned ones.
    ///
    /// # Errors
    ///
    /// Fails if the directory cannot be read or is malformed.
    pub async fn open(
        dir: Arc<dyn ByteSource>,
        archives: HashMap<u16, Arc<dyn ByteSource>>,
        budget: usize,
    ) -> Result<Self> {
        let dir_path = PathBuf::from(dir.describe());
        let read = |len: u64| {
            let dir = Arc::clone(&dir);
            let dir_path = dir_path.clone();
            async move {
                let len = usize::try_from(len.min(dir.len())).unwrap_or(usize::MAX);
                dir.read_at(0, len)
                    .await
                    .map_err(|source| Error::io(dir_path, source))
            }
        };
        let head = read(VpkHeader::MAX_LEN as u64).await?;
        let header = VpkHeader::parse(&dir_path, &head)?;
        let bytes = read(header.parse_len()).await?;
        Ok(Self {
            index: VpkIndex::parse(dir_path, &bytes)?,
            dir,
            archives,
            resident: ResidentCache::new(budget),
        })
    }

    /// Number of files in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the set contains no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.len() == 0
    }

    /// Bytes of files currently resident.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        self.resident.bytes()
    }

    /// Looks up the entry for a canonical path.
    #[must_use]
    pub fn entry(&self, path: &str) -> Option<&VpkEntry> {
        self.index.entry(path)
    }

    async fn fetch(&self, path: &str, entry: &VpkEntry) -> Result<Vec<u8>> {
        let mut remainder = Vec::new();
        if let Some((archive, offset, len)) = self.index.remainder(entry) {
            let source = if archive == DIR_ARCHIVE_INDEX {
                &self.dir
            } else {
                self.archives
                    .get(&archive)
                    .ok_or_else(|| self.index.missing_archive(entry))?
            };
            remainder = source
                .read_at(offset, len)
                .await
                .map_err(|source_error| Error::io(source.describe(), source_error))?;
        }
        self.index.assemble(path, entry, remainder)
    }
}

impl Mount for RemoteVpk {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        if !self.index.contains(path) {
            return Ok(None);
        }
        match self.resident.get(path) {
            Some(data) => Ok(Some(data.to_vec())),
            None => Err(Error::NotResident {
                path: path.to_owned(),
            }),
        }
    }

    fn read_async<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            let Some(entry) = self.index.entry(path) else {
                return Ok(None);
            };
            if let Some(data) = self.resident.get(path) {
                return Ok(Some(data.to_vec()));
            }
            self.fetch(path, entry).await.map(Some)
        })
    }

    fn make_resident<'a>(&'a self, path: &'a str, pinned: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some(entry) = self.index.entry(path) else {
                return Ok(());
            };
            if self.resident.contains(path) {
                if pinned {
                    self.resident.pin(path);
                }
                return Ok(());
            }
            let data = self.fetch(path, entry).await?;
            self.resident.insert(path, data.into(), pinned);
            Ok(())
        })
    }

    fn contains(&self, path: &str) -> bool {
        self.index.contains(path)
    }

    fn for_each_file(&self, visit: &mut dyn FnMut(&str)) {
        for path in self.index.paths() {
            visit(path);
        }
    }

    fn describe(&self) -> String {
        self.index.dir_path().display().to_string()
    }
}

/// A set of loose files, each a [`ByteSource`], served under virtual paths.
#[derive(Debug)]
pub struct RemoteFiles {
    label: String,
    files: HashMap<String, Arc<dyn ByteSource>>,
    resident: ResidentCache,
}

impl RemoteFiles {
    /// A mount of `files` (virtual path, source), described as `label`. Paths
    /// are normalised; on a collision the first one wins, as in
    /// [`crate::loose::LooseDirectory`]. At most `budget` bytes stay resident
    /// at once, not counting pinned files.
    pub fn new(
        label: impl Into<String>,
        files: impl IntoIterator<Item = (String, Arc<dyn ByteSource>)>,
        budget: usize,
    ) -> Self {
        let mut map = HashMap::new();
        for (path, source) in files {
            map.entry(normalize(&path)).or_insert(source);
        }
        Self {
            label: label.into(),
            files: map,
            resident: ResidentCache::new(budget),
        }
    }

    /// Number of files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether there are no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    async fn fetch(source: &Arc<dyn ByteSource>) -> Result<Vec<u8>> {
        let len = usize::try_from(source.len()).map_err(|_| {
            Error::io(
                source.describe(),
                std::io::Error::other("file too large to load"),
            )
        })?;
        source
            .read_at(0, len)
            .await
            .map_err(|error| Error::io(source.describe(), error))
    }
}

impl Mount for RemoteFiles {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        if !self.files.contains_key(path) {
            return Ok(None);
        }
        match self.resident.get(path) {
            Some(data) => Ok(Some(data.to_vec())),
            None => Err(Error::NotResident {
                path: path.to_owned(),
            }),
        }
    }

    fn read_async<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            let Some(source) = self.files.get(path) else {
                return Ok(None);
            };
            if let Some(data) = self.resident.get(path) {
                return Ok(Some(data.to_vec()));
            }
            Self::fetch(source).await.map(Some)
        })
    }

    fn make_resident<'a>(&'a self, path: &'a str, pinned: bool) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Some(source) = self.files.get(path) else {
                return Ok(());
            };
            if self.resident.contains(path) {
                if pinned {
                    self.resident.pin(path);
                }
                return Ok(());
            }
            let data = Self::fetch(source).await?;
            self.resident.insert(path, data.into(), pinned);
            Ok(())
        })
    }

    fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    fn for_each_file(&self, visit: &mut dyn FnMut(&str)) {
        for path in self.files.keys() {
            visit(path);
        }
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// The files of an install, wherever the platform keeps them: the browser's
/// private storage after an import, or a local folder ([`DirectoryTree`]).
pub trait RemoteTree: Send + Sync + fmt::Debug {
    /// Every file, as a slash-separated path relative to the install folder
    /// (the folder that contains `hl2mp/`), in the case it was stored.
    fn files(&self) -> Vec<String>;

    /// The file at `path`, one of those [`RemoteTree::files`] returned.
    fn open(&self, path: &str) -> Option<Arc<dyn ByteSource>>;
}

/// A [`RemoteTree`] over a local folder. Used by tests and tools to exercise
/// the asynchronous mounts against real files.
#[derive(Debug)]
pub struct DirectoryTree {
    files: BTreeMap<String, Arc<dyn ByteSource>>,
}

impl DirectoryTree {
    /// Indexes `root` recursively, opening every file.
    ///
    /// # Errors
    ///
    /// Fails if `root` cannot be read.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        let mut files = BTreeMap::new();
        collect_files(root, "", &mut files)?;
        Ok(Self { files })
    }
}

fn collect_files(
    dir: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, Arc<dyn ByteSource>>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, &relative, files)?;
        } else if let Ok(source) = FileSource::open(&path) {
            files.insert(relative, Arc::new(source));
        }
    }
    Ok(())
}

impl RemoteTree for DirectoryTree {
    fn files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn open(&self, path: &str) -> Option<Arc<dyn ByteSource>> {
        self.files.get(path).cloned()
    }
}

/// What [`mount_remote_game`] mounted.
#[derive(Debug)]
pub struct MountedRemote {
    /// The parsed `gameinfo.txt`.
    pub gameinfo: GameInfo,
    /// How many mounts were added to the filesystem.
    pub mounts: usize,
}

/// Mounts the game whose mod folder is `mod_dir` (e.g. `hl2mp`) from `tree`
/// into `vfs`, below the existing mounts: the asynchronous counterpart of
/// [`crate::content::mount_game_into`], with the same priority order. Steam
/// Workshop content is not mounted.
///
/// A search path that is not in `tree` is skipped, so a partial import still
/// mounts. At most `budget` bytes of files stay resident per mount.
///
/// # Errors
///
/// Fails if `<mod_dir>/gameinfo.txt` is missing, unreadable or malformed.
pub async fn mount_remote_game(
    vfs: &Vfs,
    tree: &dyn RemoteTree,
    mod_dir: &str,
    budget: usize,
) -> Result<MountedRemote> {
    let index = TreeIndex::new(tree);
    let gameinfo_path = format!("{mod_dir}/gameinfo.txt");
    let source = index.open(&gameinfo_path).ok_or_else(|| {
        Error::GameNotFound(format!(
            "{gameinfo_path} was not found in the imported files; import the Half-Life 2: \
             Deathmatch install folder (the one that contains {mod_dir}/)"
        ))
    })?;
    let bytes = source
        .read_at(0, usize::try_from(source.len()).unwrap_or(usize::MAX))
        .await
        .map_err(|error| Error::io(&gameinfo_path, error))?;
    let gameinfo = GameInfo::parse(
        &String::from_utf8_lossy(&bytes),
        PathBuf::from(mod_dir),
        PathBuf::new(),
    )?;
    tracing::info!(game = %gameinfo.name, "mounting imported game content");

    let locations = gameinfo.content_locations();
    let wildcard_count = locations
        .iter()
        .take_while(|(_, location)| matches!(location, ResolvedLocation::Wildcard(_)))
        .count();
    let (wildcards, rest) = locations.split_at(wildcard_count);

    let mut mounts = 0;
    for (_, location) in wildcards {
        mounts += mount_location(vfs, &index, location, &MountOrigin::Custom, budget).await;
    }
    for (search_path, location) in rest {
        let origin = if search_path.has_id("download") {
            MountOrigin::Download
        } else if matches!(location, ResolvedLocation::Wildcard(_)) {
            MountOrigin::Custom
        } else {
            MountOrigin::BaseGame
        };
        mounts += mount_location(vfs, &index, location, &origin, budget).await;
    }
    Ok(MountedRemote { gameinfo, mounts })
}

/// The files of a tree, by normalised path, for case-insensitive lookup.
struct TreeIndex<'a> {
    tree: &'a dyn RemoteTree,
    /// Normalised path to the path as the tree stores it.
    paths: BTreeMap<String, String>,
}

impl<'a> TreeIndex<'a> {
    fn new(tree: &'a dyn RemoteTree) -> Self {
        let mut paths = BTreeMap::new();
        for path in tree.files() {
            paths.entry(normalize(&path)).or_insert(path);
        }
        Self { tree, paths }
    }

    fn open(&self, path: &str) -> Option<Arc<dyn ByteSource>> {
        self.tree.open(self.paths.get(&normalize(path))?)
    }

    fn contains(&self, path: &str) -> bool {
        self.paths.contains_key(&normalize(path))
    }

    /// The files under `dir` (any depth) as (path relative to `dir`, source).
    fn under(&self, dir: &str) -> Vec<(String, Arc<dyn ByteSource>)> {
        let prefix = dir_prefix(dir);
        self.paths
            .iter()
            .filter_map(|(path, original)| {
                let relative = path.strip_prefix(&prefix)?;
                Some((relative.to_owned(), self.tree.open(original)?))
            })
            .collect()
    }

    /// The immediate children of `dir`, sorted: (name, is a directory).
    fn children(&self, dir: &str) -> Vec<(String, bool)> {
        let prefix = dir_prefix(dir);
        let mut children: BTreeMap<String, bool> = BTreeMap::new();
        for path in self.paths.keys() {
            let Some(relative) = path.strip_prefix(&prefix) else {
                continue;
            };
            match relative.split_once('/') {
                Some((directory, _)) => {
                    children.insert(directory.to_owned(), true);
                }
                None => {
                    children.entry(relative.to_owned()).or_insert(false);
                }
            }
        }
        children.into_iter().collect()
    }
}

/// `dir` as a normalised prefix of its files' paths (`hl2mp/`, or nothing for
/// the root).
fn dir_prefix(dir: &str) -> String {
    let dir = normalize(dir);
    if dir.is_empty() {
        dir
    } else {
        format!("{dir}/")
    }
}

/// A [`PathBuf`] from `gameinfo.txt` as a slash-separated virtual path.
fn virtual_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Mounts one search path, returning how many mounts it added.
async fn mount_location(
    vfs: &Vfs,
    index: &TreeIndex<'_>,
    location: &ResolvedLocation,
    origin: &MountOrigin,
    budget: usize,
) -> usize {
    match location {
        ResolvedLocation::Directory(dir) => usize::from(mount_directory(
            vfs,
            index,
            &virtual_path(dir),
            origin,
            budget,
        )),
        ResolvedLocation::Vpk(path) => {
            usize::from(mount_vpk(vfs, index, &virtual_path(path), origin, budget).await)
        }
        ResolvedLocation::Wildcard(dir) => {
            let dir = virtual_path(dir);
            let mut added = 0;
            for (name, is_directory) in index.children(&dir) {
                let child = if dir.is_empty() {
                    name.clone()
                } else {
                    format!("{dir}/{name}")
                };
                if is_directory {
                    added += usize::from(mount_directory(vfs, index, &child, origin, budget));
                } else if is_mountable_vpk(Path::new(&name)) {
                    added += usize::from(mount_vpk(vfs, index, &child, origin, budget).await);
                }
            }
            added
        }
    }
}

fn mount_directory(
    vfs: &Vfs,
    index: &TreeIndex<'_>,
    dir: &str,
    origin: &MountOrigin,
    budget: usize,
) -> bool {
    let files = index.under(dir);
    if files.is_empty() {
        tracing::debug!(dir, "search path directory is not imported; skipping");
        return false;
    }
    vfs.mount_back(RemoteFiles::new(dir, files, budget), origin.clone());
    true
}

async fn mount_vpk(
    vfs: &Vfs,
    index: &TreeIndex<'_>,
    path: &str,
    origin: &MountOrigin,
    budget: usize,
) -> bool {
    // As on disk: `pak01.vpk` in gameinfo means `pak01_dir.vpk` if it exists.
    let dir_path = match path.to_ascii_lowercase().strip_suffix(".vpk") {
        Some(stem) if !stem.ends_with("_dir") && index.contains(&format!("{stem}_dir.vpk")) => {
            format!("{stem}_dir.vpk")
        }
        _ => path.to_owned(),
    };
    let Some(dir) = index.open(&dir_path) else {
        tracing::debug!(vpk = path, "search path VPK is not imported; skipping");
        return false;
    };
    let archives = numbered_archives(index, &dir_path);
    match RemoteVpk::open(dir, archives, budget).await {
        Ok(vpk) => {
            vfs.mount_back(vpk, origin.clone());
            true
        }
        Err(error) => {
            tracing::warn!(%error, "skipping unreadable VPK");
            false
        }
    }
}

/// The data archives of the VPK set whose directory file is `dir_path`:
/// `<stem>_000.vpk`, `<stem>_001.vpk`, ... next to it, by index.
fn numbered_archives(index: &TreeIndex<'_>, dir_path: &str) -> HashMap<u16, Arc<dyn ByteSource>> {
    let normalised = normalize(dir_path);
    let Some(stem) = normalised.strip_suffix("_dir.vpk") else {
        return HashMap::new();
    };
    let (folder, file_stem) = stem.rsplit_once('/').unwrap_or(("", stem));
    let names: BTreeSet<u16> = index
        .children(folder)
        .into_iter()
        .filter(|(_, is_directory)| !is_directory)
        .filter_map(|(name, _)| {
            let number = name
                .strip_prefix(file_stem)?
                .strip_prefix('_')?
                .strip_suffix(".vpk")?;
            (number.len() == 3).then(|| number.parse().ok())?
        })
        .collect();
    names
        .into_iter()
        .filter_map(|number| {
            let path = if folder.is_empty() {
                format!("{file_stem}_{number:03}.vpk")
            } else {
                format!("{folder}/{file_stem}_{number:03}.vpk")
            };
            Some((number, index.open(&path)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use futures_lite::future::block_on;
    use proptest::prelude::*;

    use super::*;
    use crate::byte_source::MemorySource;
    use crate::testing::{SAMPLE_GAMEINFO, VpkBuilder};
    use crate::vpk::VpkArchive;

    fn source(path: &Path) -> Arc<dyn ByteSource> {
        Arc::new(FileSource::open(path).unwrap())
    }

    /// The numbered archives next to `dir_file`, opened as sources.
    fn archives_of(dir_file: &Path, count: u16) -> HashMap<u16, Arc<dyn ByteSource>> {
        let stem = dir_file
            .to_str()
            .unwrap()
            .strip_suffix("_dir.vpk")
            .unwrap()
            .to_owned();
        (0..count)
            .map(|number| {
                (
                    number,
                    source(Path::new(&format!("{stem}_{number:03}.vpk"))),
                )
            })
            .collect()
    }

    fn sample_vpk(dir: &Path) -> PathBuf {
        VpkBuilder::new()
            .file("materials/concrete/wall.vmt", b"\"LightmappedGeneric\" {}")
            .file_with_preload("sound/a.wav", b"RIFFdata-after-preload", 4)
            .file("noext", b"x")
            .file("root.txt", b"root")
            .write(dir, "pak01")
            .unwrap()
    }

    #[test]
    fn a_remote_vpk_reads_what_the_native_one_does() {
        let dir = tempfile::tempdir().unwrap();
        let dir_file = sample_vpk(dir.path());
        let native = VpkArchive::open(&dir_file).unwrap();
        let remote = block_on(RemoteVpk::open(
            source(&dir_file),
            archives_of(&dir_file, 1),
            1 << 20,
        ))
        .unwrap();
        assert_eq!(remote.len(), native.len());
        let mut paths = Vec::new();
        native.for_each_file(&mut |path| paths.push(path.to_owned()));
        for path in &paths {
            assert!(remote.contains(path));
            assert_eq!(
                block_on(remote.read_async(path)).unwrap(),
                native.read(path).unwrap(),
                "{path}"
            );
        }
        assert_eq!(block_on(remote.read_async("missing.txt")).unwrap(), None);
        assert_eq!(remote.read("missing.txt").unwrap(), None);
    }

    #[test]
    fn a_remote_vpk_with_embedded_data_and_version_one_reads() {
        for version in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let dir_file = VpkBuilder::new()
                .version(version)
                .embed_in_dir(true)
                .file("a/b.txt", b"embedded data")
                .file_with_preload("c.txt", b"preloaded and embedded", 3)
                .write(dir.path(), "emb")
                .unwrap();
            let remote =
                block_on(RemoteVpk::open(source(&dir_file), HashMap::new(), 1 << 20)).unwrap();
            assert_eq!(
                block_on(remote.read_async("a/b.txt")).unwrap().unwrap(),
                b"embedded data"
            );
            assert_eq!(
                block_on(remote.read_async("c.txt")).unwrap().unwrap(),
                b"preloaded and embedded"
            );
        }
    }

    #[test]
    fn a_synchronous_read_needs_the_file_to_be_resident() {
        let dir = tempfile::tempdir().unwrap();
        let dir_file = sample_vpk(dir.path());
        let remote = block_on(RemoteVpk::open(
            source(&dir_file),
            archives_of(&dir_file, 1),
            1 << 20,
        ))
        .unwrap();
        let error = remote.read("root.txt").unwrap_err();
        assert!(matches!(error, Error::NotResident { ref path } if path == "root.txt"));

        block_on(remote.make_resident("root.txt", false)).unwrap();
        assert_eq!(remote.read("root.txt").unwrap().unwrap(), b"root");
        assert_eq!(remote.resident_bytes(), 4);
        // Making an unknown file resident is not an error.
        block_on(remote.make_resident("nope", false)).unwrap();
    }

    #[test]
    fn resident_files_are_dropped_to_stay_within_the_budget_unless_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let dir_file = VpkBuilder::new()
            .file("a.bin", &[1; 60])
            .file("b.bin", &[2; 60])
            .file("c.bin", &[3; 60])
            .write(dir.path(), "pak")
            .unwrap();
        let remote = block_on(RemoteVpk::open(
            source(&dir_file),
            archives_of(&dir_file, 1),
            100,
        ))
        .unwrap();
        block_on(remote.make_resident("a.bin", true)).unwrap();
        block_on(remote.make_resident("b.bin", false)).unwrap();
        block_on(remote.make_resident("c.bin", false)).unwrap();
        assert!(remote.read("a.bin").unwrap().is_some(), "pinned stays");
        assert!(matches!(
            remote.read("b.bin"),
            Err(Error::NotResident { .. })
        ));
        assert!(remote.read("c.bin").unwrap().is_some());
        // An evicted file can still be read asynchronously.
        assert_eq!(
            block_on(remote.read_async("b.bin")).unwrap().unwrap(),
            [2; 60]
        );
    }

    #[test]
    fn a_missing_data_archive_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let dir_file = sample_vpk(dir.path());
        let remote = block_on(RemoteVpk::open(source(&dir_file), HashMap::new(), 1 << 20)).unwrap();
        let error = block_on(remote.read_async("root.txt")).unwrap_err();
        assert!(matches!(error, Error::InvalidVpk { .. }), "{error}");
    }

    #[test]
    fn a_corrupt_directory_is_an_error_not_a_panic() {
        let junk: Arc<dyn ByteSource> = Arc::new(MemorySource::new("junk_dir.vpk", vec![1, 2, 3]));
        assert!(block_on(RemoteVpk::open(junk, HashMap::new(), 10)).is_err());
        let empty: Arc<dyn ByteSource> = Arc::new(MemorySource::new("empty_dir.vpk", Vec::new()));
        assert!(block_on(RemoteVpk::open(empty, HashMap::new(), 10)).is_err());
    }

    #[test]
    fn remote_files_serve_normalised_paths() {
        let files: Vec<(String, Arc<dyn ByteSource>)> = vec![
            (
                "Maps/DM_A.bsp".to_owned(),
                Arc::new(MemorySource::new("a", b"map".to_vec())),
            ),
            (
                "scripts\\x.txt".to_owned(),
                Arc::new(MemorySource::new("b", b"script".to_vec())),
            ),
        ];
        let mount = RemoteFiles::new("test", files, 1 << 10);
        assert_eq!(mount.len(), 2);
        assert!(mount.contains("maps/dm_a.bsp"));
        assert_eq!(
            block_on(mount.read_async("maps/dm_a.bsp"))
                .unwrap()
                .unwrap(),
            b"map"
        );
        assert!(matches!(
            mount.read("scripts/x.txt"),
            Err(Error::NotResident { .. })
        ));
        block_on(mount.make_resident("scripts/x.txt", false)).unwrap();
        assert_eq!(mount.read("scripts/x.txt").unwrap().unwrap(), b"script");
        assert_eq!(mount.describe(), "test");
    }

    /// Builds the install the content tests mount: a mod folder with
    /// `gameinfo.txt`, loose maps, and two VPKs, as the sample game lays out.
    fn install(root: &Path) {
        let mod_dir = root.join("mod");
        fs::create_dir_all(mod_dir.join("maps")).unwrap();
        fs::create_dir_all(mod_dir.join("custom/my_override/materials")).unwrap();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(mod_dir.join("gameinfo.txt"), SAMPLE_GAMEINFO).unwrap();
        fs::write(mod_dir.join("maps/dm_base.bsp"), "bsp").unwrap();
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
            .write(&root.join("shared"), "shared_pak")
            .unwrap();
    }

    #[test]
    fn mounting_a_remote_game_matches_mounting_it_from_disk() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path());

        // The reference: the ordinary mount of the same folder.
        let native = Vfs::new();
        crate::content::mount_game_into(
            &native,
            &crate::content::ContentOptions {
                game_dir: Some(tmp.path().to_path_buf()),
                mod_dir: "mod".into(),
                workshop: crate::content::WorkshopOptions {
                    enabled: false,
                    ..Default::default()
                },
                steam: Some(crate::steam::SteamLibraries::from_roots(
                    Vec::<PathBuf>::new(),
                )),
                ..Default::default()
            },
        )
        .unwrap();

        let remote = Vfs::new();
        let tree = DirectoryTree::open(tmp.path()).unwrap();
        let mounted = block_on(mount_remote_game(&remote, &tree, "mod", 1 << 20)).unwrap();
        assert!(mounted.mounts >= 3, "mounted {}", mounted.mounts);

        // Every file the plain mount serves comes out the same, with the same
        // winner where several mounts have it.
        for file in native.list("**") {
            assert_eq!(
                block_on(remote.read_async(&file.path)).unwrap(),
                native.read(&file.path).unwrap(),
                "{}",
                file.path
            );
        }
        // The custom override wins over both packs, as on disk.
        assert_eq!(
            block_on(remote.read_async("materials/shared.vmt"))
                .unwrap()
                .unwrap(),
            b"custom"
        );
        assert_eq!(
            block_on(remote.read_async("materials/mod_only.vmt"))
                .unwrap()
                .unwrap(),
            b"mod_pak"
        );
        assert!(remote.exists("maps/dm_base.bsp"));
    }

    #[test]
    fn the_filesystem_reports_misses_until_files_are_made_resident() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path());
        let vfs = Vfs::new();
        let tree = DirectoryTree::open(tmp.path()).unwrap();
        block_on(mount_remote_game(&vfs, &tree, "mod", 1 << 20)).unwrap();

        assert!(matches!(
            vfs.read("materials/mod_only.vmt"),
            Err(Error::NotResident { .. })
        ));
        assert!(matches!(
            vfs.read("maps/dm_base.bsp"),
            Err(Error::NotResident { .. })
        ));
        assert_eq!(
            vfs.take_misses(),
            ["maps/dm_base.bsp", "materials/mod_only.vmt"]
        );
        assert!(vfs.take_misses().is_empty(), "taking forgets them");

        block_on(vfs.make_resident(
            &[
                "materials/mod_only.vmt",
                "maps/dm_base.bsp",
                "not/there.txt",
            ],
            false,
        ))
        .unwrap();
        assert_eq!(
            vfs.read("materials/mod_only.vmt").unwrap().unwrap(),
            b"mod_pak"
        );
        assert_eq!(vfs.read("maps/dm_base.bsp").unwrap().unwrap(), b"bsp");
        assert!(vfs.take_misses().is_empty());
    }

    #[test]
    fn a_game_with_no_gameinfo_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let tree = DirectoryTree::open(tmp.path()).unwrap();
        let error = block_on(mount_remote_game(&Vfs::new(), &tree, "mod", 10)).unwrap_err();
        assert!(matches!(error, Error::GameNotFound(_)), "{error}");
    }

    #[test]
    fn a_partial_import_still_mounts_what_it_has() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path());
        fs::remove_dir_all(tmp.path().join("shared")).unwrap();
        let vfs = Vfs::new();
        let tree = DirectoryTree::open(tmp.path()).unwrap();
        block_on(mount_remote_game(&vfs, &tree, "mod", 1 << 20)).unwrap();
        assert!(vfs.exists("materials/mod_only.vmt"));
        assert!(vfs.exists("maps/dm_base.bsp"));
    }

    proptest! {
        /// Whatever files a VPK holds, reading each one through a remote mount
        /// (asynchronously, or resident then synchronously) gives its bytes.
        #[test]
        fn every_file_round_trips(
            files in proptest::collection::btree_map(
                "[a-z]{1,6}(/[a-z]{1,6}){0,2}\\.[a-z]{1,3}",
                (proptest::collection::vec(any::<u8>(), 0..200), 0_usize..40),
                1..8,
            ),
        ) {
            // A path that is a prefix of another's directory cannot coexist.
            prop_assume!(files.keys().all(|a| files.keys().all(|b| a == b || !b.starts_with(&format!("{a}/")))));
            let dir = tempfile::tempdir().unwrap();
            let mut builder = VpkBuilder::new();
            for (path, (data, preload)) in &files {
                builder = builder.file_with_preload(path, data, *preload);
            }
            let dir_file = builder.write(dir.path(), "p").unwrap();
            let remote = block_on(RemoteVpk::open(source(&dir_file), archives_of(&dir_file, 1), 1 << 20)).unwrap();
            for (path, (data, _)) in &files {
                prop_assert_eq!(&block_on(remote.read_async(path)).unwrap().unwrap(), data);
                block_on(remote.make_resident(path, false)).unwrap();
                prop_assert_eq!(&remote.read(path).unwrap().unwrap(), data);
            }
        }
    }
}
