//! Mount for plain directories of loose files.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::mount::Mount;
use crate::path::normalize;

/// A directory on disk exposed as a [`Mount`].
///
/// Source content paths are case-insensitive, but Linux filesystems are not,
/// and loose content (especially community content) frequently has mixed-case
/// names. The directory is therefore indexed once when mounted, mapping each
/// canonical (lowercase) path to its real on-disk path. Files added to the
/// directory afterwards are not visible until it is remounted, which mirrors
/// the engine scanning `custom/` only at boot.
///
/// Symbolic links to files are followed; symbolic links to directories are not,
/// which rules out cycles.
#[derive(Debug)]
pub struct LooseDirectory {
    root: PathBuf,
    index: HashMap<String, PathBuf>,
}

impl LooseDirectory {
    /// Indexes `root` recursively.
    ///
    /// # Errors
    ///
    /// Fails if `root` itself cannot be read. Unreadable subdirectories are
    /// skipped with a warning so that one bad permission does not hide a whole
    /// mount.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let mut index = HashMap::new();
        let metadata = fs::metadata(&root).map_err(|e| Error::io(&root, e))?;
        if !metadata.is_dir() {
            let error = io::Error::new(io::ErrorKind::NotADirectory, "not a directory");
            return Err(Error::io(&root, error));
        }
        index_directory(&root, "", &mut index);
        Ok(Self { root, index })
    }

    /// The directory this mount exposes.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Number of indexed files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the directory contains no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Returns the real on-disk path for a canonical virtual path.
    #[must_use]
    pub fn real_path(&self, path: &str) -> Option<&Path> {
        self.index.get(path).map(PathBuf::as_path)
    }
}

fn index_directory(dir: &Path, prefix: &str, index: &mut HashMap<String, PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(directory = %dir.display(), %error, "skipping unreadable directory");
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let virtual_path = if prefix.is_empty() {
            normalize(&name)
        } else {
            normalize(&format!("{prefix}/{name}"))
        };
        if file_type.is_dir() {
            index_directory(&entry.path(), &virtual_path, index);
        } else if file_type.is_file() || entry.path().is_file() {
            // On a case collision (`Wall.vtf` and `wall.vtf`) keep the first
            // one seen; which one is unspecified, as it is in the engine.
            index.entry(virtual_path).or_insert_with(|| entry.path());
        }
    }
}

impl Mount for LooseDirectory {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        match self.index.get(path) {
            Some(real) => fs::read(real).map(Some).map_err(|e| Error::io(real, e)),
            None => Ok(None),
        }
    }

    fn contains(&self, path: &str) -> bool {
        self.index.contains_key(path)
    }

    fn for_each_file(&self, visit: &mut dyn FnMut(&str)) {
        for path in self.index.keys() {
            visit(path);
        }
    }

    fn describe(&self) -> String {
        self.root.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_recursively_and_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Materials/Concrete")).unwrap();
        fs::write(dir.path().join("Materials/Concrete/Wall01.VMT"), b"vmt").unwrap();
        fs::write(dir.path().join("readme.txt"), b"hi").unwrap();

        let mount = LooseDirectory::open(dir.path()).unwrap();
        assert_eq!(mount.len(), 2);
        assert!(mount.contains("materials/concrete/wall01.vmt"));
        assert_eq!(
            mount
                .read("materials/concrete/wall01.vmt")
                .unwrap()
                .unwrap(),
            b"vmt"
        );
        assert_eq!(mount.read("nope.txt").unwrap(), None);
        assert!(
            mount
                .real_path("readme.txt")
                .unwrap()
                .ends_with("readme.txt")
        );
    }

    #[test]
    fn missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            LooseDirectory::open(dir.path().join("absent")),
            Err(Error::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_directory_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/file.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/loop")).unwrap();
        let mount = LooseDirectory::open(dir.path()).unwrap();
        assert_eq!(mount.len(), 1);
    }
}
