//! Reader for Valve Pak (VPK) archives, versions 1 and 2.
//!
//! A VPK set consists of a *directory* file (`<stem>_dir.vpk`) holding the file
//! tree, plus numbered data archives (`<stem>_000.vpk`, `<stem>_001.vpk`, ...).
//! A file's bytes are the concatenation of an optional *preload* chunk stored
//! inline in the directory and a span inside one of the data archives (or, for
//! index `0x7fff`, inside the directory file itself after the tree).
//!
//! # Directory layout
//!
//! ```text
//! header   u32 signature (0x55aa1234), u32 version (1|2), u32 tree_size
//!          v2 only: u32 file_data_size, u32 archive_md5_size,
//!                   u32 other_md5_size, u32 signature_size
//! tree     repeat { extension\0
//!            repeat { directory\0
//!              repeat { filename\0  entry }  until empty filename
//!            } until empty directory
//!          } until empty extension
//! entry    u32 crc32, u16 preload_len, u16 archive_index, u32 offset,
//!          u32 length, u16 terminator (0xffff), preload_len bytes
//! ```
//!
//! A single space (`" "`) as directory or extension means "none".
//!
//! The same format is used by base-game content and by Steam Workshop items
//! (`workshop_dir.vpk` + `workshop_NNN.vpk`), so this reader serves both.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::error::{Error, Result};
use crate::io::{ByteReader, read_exact_at};
use crate::mount::Mount;

/// Magic number at the start of every VPK directory file.
pub const SIGNATURE: u32 = 0x55aa_1234;

/// Archive index meaning "the data lives in the directory file itself".
pub const DIR_ARCHIVE_INDEX: u16 = 0x7fff;

/// Terminator that ends every directory entry.
const ENTRY_TERMINATOR: u16 = 0xffff;

/// Header length for version 1 directories.
const HEADER_LEN_V1: u32 = 12;

/// Header length for version 2 directories.
const HEADER_LEN_V2: u32 = 28;

/// Location of one file inside a VPK set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpkEntry {
    /// CRC32 of the complete file contents.
    pub crc: u32,
    /// Which data archive holds the remainder; [`DIR_ARCHIVE_INDEX`] for the
    /// directory file itself.
    pub archive_index: u16,
    /// Byte offset of the remainder within its archive (relative to the end of
    /// the tree for [`DIR_ARCHIVE_INDEX`]).
    pub offset: u32,
    /// Length of the remainder in bytes.
    pub length: u32,
    /// Range of the inline preload bytes within the directory's tree buffer.
    preload: std::ops::Range<usize>,
}

impl VpkEntry {
    /// Total size of the file in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.preload.len() as u64 + u64::from(self.length)
    }
}

/// An opened VPK set, usable as a [`Mount`].
///
/// Opening reads only the directory file; data archives are opened lazily on
/// first access and kept open for positional reads from any thread.
#[derive(Debug)]
pub struct VpkArchive {
    dir_path: PathBuf,
    /// Path prefix for numbered archives: `<dir>/<stem>_`; `None` for a
    /// standalone VPK that has no numbered archives.
    archive_stem: Option<PathBuf>,
    /// Raw tree bytes; entries reference their preload data by range.
    tree: Box<[u8]>,
    /// Absolute offset in the directory file where embedded file data starts.
    embedded_data_offset: u64,
    entries: HashMap<String, VpkEntry>,
    archives: Mutex<HashMap<u16, Arc<File>>>,
}

impl VpkArchive {
    /// Opens a VPK set.
    ///
    /// `path` may name the directory file (`pak01_dir.vpk`), a standalone VPK,
    /// or the *logical* name used in `gameinfo.txt` (`pak01.vpk`), in which
    /// case `pak01_dir.vpk` is tried first.
    ///
    /// # Errors
    ///
    /// Fails if no matching file exists or the directory is malformed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let dir_path = resolve_dir_path(path.as_ref());
        let bytes = fs::read(&dir_path).map_err(|e| Error::io(&dir_path, e))?;
        Self::from_dir_bytes(dir_path, &bytes)
    }

    /// Parses an already-loaded directory file. `dir_path` is used to locate
    /// data archives and for diagnostics.
    ///
    /// # Errors
    ///
    /// Fails if the directory is malformed.
    pub fn from_dir_bytes(dir_path: PathBuf, bytes: &[u8]) -> Result<Self> {
        let invalid = |reason: String| Error::InvalidVpk {
            path: dir_path.clone(),
            reason,
        };
        let mut reader = ByteReader::new(bytes);

        let signature = reader
            .u32()
            .ok_or_else(|| invalid("file too short".into()))?;
        if signature != SIGNATURE {
            return Err(invalid(format!("bad signature {signature:#010x}")));
        }
        let version = reader
            .u32()
            .ok_or_else(|| invalid("truncated header".into()))?;
        let tree_size = reader
            .u32()
            .ok_or_else(|| invalid("truncated header".into()))?;
        let header_len = match version {
            1 => HEADER_LEN_V1,
            2 => {
                // The v2 extension describes trailing checksum/signature
                // sections, which a read-only loader can ignore.
                reader
                    .bytes(16)
                    .ok_or_else(|| invalid("truncated v2 header".into()))?;
                HEADER_LEN_V2
            }
            other => return Err(invalid(format!("unsupported version {other}"))),
        };

        let tree_start = header_len as usize;
        let tree_end = tree_start
            .checked_add(tree_size as usize)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| invalid(format!("tree size {tree_size} exceeds file length")))?;
        let tree: Box<[u8]> = bytes[tree_start..tree_end].into();
        let entries = parse_tree(&tree).map_err(|offset| {
            invalid(format!("malformed tree near byte {}", tree_start + offset))
        })?;

        let archive_stem = dir_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix("_dir.vpk"))
            .map(|stem| dir_path.with_file_name(format!("{stem}_")));

        Ok(Self {
            dir_path,
            archive_stem,
            tree,
            embedded_data_offset: u64::from(header_len) + u64::from(tree_size),
            entries,
            archives: Mutex::new(HashMap::new()),
        })
    }

    /// Path of the directory file.
    #[must_use]
    pub fn dir_path(&self) -> &Path {
        &self.dir_path
    }

    /// Number of files in the archive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the archive contains no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up the entry for a canonical path.
    #[must_use]
    pub fn entry(&self, path: &str) -> Option<&VpkEntry> {
        self.entries.get(path)
    }

    /// Reads a file's full contents.
    ///
    /// In debug builds the CRC32 is verified, catching truncated downloads and
    /// reader bugs early; release builds skip it for load speed.
    fn read_entry(&self, path: &str, entry: &VpkEntry) -> Result<Vec<u8>> {
        let size = usize::try_from(entry.size()).map_err(|_| Error::InvalidVpk {
            path: self.dir_path.clone(),
            reason: format!("{path} is too large to load"),
        })?;
        let mut data = Vec::with_capacity(size);
        data.extend_from_slice(&self.tree[entry.preload.clone()]);

        if entry.length > 0 {
            let (file, base, file_path) = if entry.archive_index == DIR_ARCHIVE_INDEX {
                (
                    self.open_dir_file()?,
                    self.embedded_data_offset,
                    self.dir_path.clone(),
                )
            } else {
                let (file, file_path) = self.open_archive(entry.archive_index)?;
                (file, 0, file_path)
            };
            let start = data.len();
            data.resize(size, 0);
            read_exact_at(&file, &mut data[start..], base + u64::from(entry.offset))
                .map_err(|e| Error::io(file_path, e))?;
        }

        if cfg!(debug_assertions) {
            let actual = crc32fast::hash(&data);
            if actual != entry.crc {
                return Err(Error::ChecksumMismatch {
                    archive: self.dir_path.clone(),
                    virtual_path: path.to_owned(),
                    expected: entry.crc,
                    actual,
                });
            }
        }
        Ok(data)
    }

    fn open_dir_file(&self) -> Result<Arc<File>> {
        self.cached_file(DIR_ARCHIVE_INDEX, &self.dir_path)
    }

    fn open_archive(&self, index: u16) -> Result<(Arc<File>, PathBuf)> {
        let stem = self
            .archive_stem
            .as_ref()
            .ok_or_else(|| Error::InvalidVpk {
                path: self.dir_path.clone(),
                reason: format!("entry references archive {index} but this is a standalone VPK"),
            })?;
        let mut path = stem.clone().into_os_string();
        path.push(format!("{index:03}.vpk"));
        let path = PathBuf::from(path);
        Ok((self.cached_file(index, &path)?, path))
    }

    fn cached_file(&self, key: u16, path: &Path) -> Result<Arc<File>> {
        // A poisoned lock only means another thread panicked mid-insert; the
        // map itself is still a valid cache, so recover it.
        let mut archives = self.archives.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = archives.get(&key) {
            return Ok(Arc::clone(file));
        }
        let file = Arc::new(File::open(path).map_err(|e| Error::io(path, e))?);
        archives.insert(key, Arc::clone(&file));
        Ok(file)
    }
}

impl Mount for VpkArchive {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        match self.entries.get(path) {
            Some(entry) => self.read_entry(path, entry).map(Some),
            None => Ok(None),
        }
    }

    fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    fn for_each_file(&self, visit: &mut dyn FnMut(&str)) {
        for path in self.entries.keys() {
            visit(path);
        }
    }

    fn describe(&self) -> String {
        self.dir_path.display().to_string()
    }
}

/// Maps a logical VPK name to the file that actually holds the directory.
fn resolve_dir_path(path: &Path) -> PathBuf {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return path.to_path_buf();
    };
    if name.ends_with("_dir.vpk") {
        return path.to_path_buf();
    }
    if let Some(stem) = name.strip_suffix(".vpk") {
        let dir_variant = path.with_file_name(format!("{stem}_dir.vpk"));
        if dir_variant.is_file() {
            return dir_variant;
        }
    }
    path.to_path_buf()
}

/// Parses the directory tree. On failure returns the byte offset (within the
/// tree) at which parsing stopped.
fn parse_tree(tree: &[u8]) -> std::result::Result<HashMap<String, VpkEntry>, usize> {
    let mut reader = ByteReader::new(tree);
    let mut entries = HashMap::new();
    loop {
        let extension = reader.cstr().ok_or(reader.position())?;
        if extension.is_empty() {
            break;
        }
        loop {
            let directory = reader.cstr().ok_or(reader.position())?;
            if directory.is_empty() {
                break;
            }
            loop {
                let filename = reader.cstr().ok_or(reader.position())?;
                if filename.is_empty() {
                    break;
                }
                let entry = parse_entry(&mut reader).ok_or(reader.position())?;
                let path = crate::path::normalize(&compose_path(&directory, &filename, &extension));
                entries.insert(path, entry);
            }
        }
    }
    Ok(entries)
}

fn parse_entry(reader: &mut ByteReader<'_>) -> Option<VpkEntry> {
    let crc = reader.u32()?;
    let preload_len = usize::from(reader.u16()?);
    let archive_index = reader.u16()?;
    let offset = reader.u32()?;
    let length = reader.u32()?;
    if reader.u16()? != ENTRY_TERMINATOR {
        return None;
    }
    let preload_start = reader.position();
    reader.bytes(preload_len)?;
    Some(VpkEntry {
        crc,
        archive_index,
        offset,
        length,
        preload: preload_start..preload_start + preload_len,
    })
}

/// Reassembles a full path from its three tree components, honouring the
/// single-space "none" convention.
fn compose_path(directory: &str, filename: &str, extension: &str) -> String {
    let mut path = String::new();
    if directory != " " {
        path.push_str(directory);
        path.push('/');
    }
    path.push_str(filename);
    if extension != " " {
        path.push('.');
        path.push_str(extension);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::VpkBuilder;
    use proptest::prelude::*;

    #[test]
    fn reads_files_from_numbered_archives_and_preload() {
        let dir = tempfile::tempdir().unwrap();
        let path = VpkBuilder::new()
            .file("materials/concrete/wall.vmt", b"\"LightmappedGeneric\" {}")
            .file_with_preload("sound/a.wav", b"RIFFdata-after-preload", 4)
            .file("noext", b"x")
            .file("root.txt", b"root")
            .write(dir.path(), "pak01")
            .unwrap();
        assert!(path.ends_with("pak01_dir.vpk"));

        let vpk = VpkArchive::open(dir.path().join("pak01.vpk")).unwrap();
        assert_eq!(vpk.len(), 4);
        assert_eq!(
            vpk.read("materials/concrete/wall.vmt").unwrap().unwrap(),
            b"\"LightmappedGeneric\" {}"
        );
        assert_eq!(
            vpk.read("sound/a.wav").unwrap().unwrap(),
            b"RIFFdata-after-preload"
        );
        assert_eq!(vpk.entry("sound/a.wav").unwrap().size(), 22);
        assert_eq!(vpk.read("noext").unwrap().unwrap(), b"x");
        assert_eq!(vpk.read("root.txt").unwrap().unwrap(), b"root");
        assert_eq!(vpk.read("missing.txt").unwrap(), None);
        assert!(vpk.contains("root.txt"));

        let mut listed = Vec::new();
        vpk.for_each_file(&mut |p| listed.push(p.to_owned()));
        listed.sort();
        assert_eq!(
            listed,
            [
                "materials/concrete/wall.vmt",
                "noext",
                "root.txt",
                "sound/a.wav"
            ]
        );
    }

    #[test]
    fn reads_data_embedded_in_directory_file() {
        let dir = tempfile::tempdir().unwrap();
        VpkBuilder::new()
            .embed_in_dir(true)
            .file("a/b.txt", b"embedded")
            .write(dir.path(), "solo")
            .unwrap();
        let vpk = VpkArchive::open(dir.path().join("solo_dir.vpk")).unwrap();
        assert_eq!(vpk.read("a/b.txt").unwrap().unwrap(), b"embedded");
    }

    #[test]
    fn supports_version_one_directories() {
        let dir = tempfile::tempdir().unwrap();
        VpkBuilder::new()
            .version(1)
            .file("x.txt", b"v1")
            .write(dir.path(), "old")
            .unwrap();
        let vpk = VpkArchive::open(dir.path().join("old_dir.vpk")).unwrap();
        assert_eq!(vpk.read("x.txt").unwrap().unwrap(), b"v1");
    }

    #[test]
    fn normalizes_mixed_case_tree_paths() {
        let dir = tempfile::tempdir().unwrap();
        VpkBuilder::new()
            .file("Models/Props/Barrel.MDL", b"m")
            .write(dir.path(), "p")
            .unwrap();
        let vpk = VpkArchive::open(dir.path().join("p_dir.vpk")).unwrap();
        assert!(vpk.contains("models/props/barrel.mdl"));
    }

    #[test]
    fn rejects_bad_signature_version_and_truncation() {
        let bad_sig = [0u8; 12];
        assert!(matches!(
            VpkArchive::from_dir_bytes("x".into(), &bad_sig),
            Err(Error::InvalidVpk { .. })
        ));

        let mut bad_version = SIGNATURE.to_le_bytes().to_vec();
        bad_version.extend_from_slice(&3u32.to_le_bytes());
        bad_version.extend_from_slice(&0u32.to_le_bytes());
        let error = VpkArchive::from_dir_bytes("x".into(), &bad_version).unwrap_err();
        assert!(
            error.to_string().contains("unsupported version 3"),
            "{error}"
        );

        let mut oversized_tree = SIGNATURE.to_le_bytes().to_vec();
        oversized_tree.extend_from_slice(&1u32.to_le_bytes());
        oversized_tree.extend_from_slice(&1000u32.to_le_bytes());
        assert!(VpkArchive::from_dir_bytes("x".into(), &oversized_tree).is_err());
    }

    #[test]
    fn missing_data_archive_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        VpkBuilder::new()
            .file("x.txt", b"data")
            .write(dir.path(), "gone")
            .unwrap();
        fs::remove_file(dir.path().join("gone_000.vpk")).unwrap();
        let vpk = VpkArchive::open(dir.path().join("gone_dir.vpk")).unwrap();
        assert!(matches!(vpk.read("x.txt"), Err(Error::Io { .. })));
    }

    #[test]
    #[cfg(debug_assertions)]
    fn detects_corrupted_data_in_debug_builds() {
        let dir = tempfile::tempdir().unwrap();
        VpkBuilder::new()
            .file("x.txt", b"good")
            .write(dir.path(), "c")
            .unwrap();
        fs::write(dir.path().join("c_000.vpk"), b"evil").unwrap();
        let vpk = VpkArchive::open(dir.path().join("c_dir.vpk")).unwrap();
        assert!(matches!(
            vpk.read("x.txt"),
            Err(Error::ChecksumMismatch { .. })
        ));
    }

    proptest! {
        #[test]
        fn parser_never_panics(tail in proptest::collection::vec(any::<u8>(), 0..256), version in 1u32..=2) {
            let mut bytes = SIGNATURE.to_le_bytes().to_vec();
            bytes.extend_from_slice(&version.to_le_bytes());
            bytes.extend_from_slice(&(tail.len() as u32).to_le_bytes());
            if version == 2 {
                bytes.extend_from_slice(&[0; 16]);
            }
            bytes.extend_from_slice(&tail);
            let _ = VpkArchive::from_dir_bytes("fuzz_dir.vpk".into(), &bytes);
        }

        #[test]
        fn round_trips_arbitrary_files(
            files in proptest::collection::btree_map(
                "[a-z]{1,6}(/[a-z]{1,6}){0,2}\\.[a-z]{1,3}",
                proptest::collection::vec(any::<u8>(), 0..64),
                1..8,
            )
        ) {
            let dir = tempfile::tempdir().unwrap();
            let mut builder = VpkBuilder::new();
            for (path, data) in &files {
                builder = builder.file(path, data);
            }
            builder.write(dir.path(), "rt").unwrap();
            let vpk = VpkArchive::open(dir.path().join("rt_dir.vpk")).unwrap();
            for (path, data) in &files {
                let read = vpk.read(path).unwrap();
                prop_assert_eq!(read.as_ref(), Some(data));
            }
        }
    }
}
