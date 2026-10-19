//! Mount for in-memory zip archives, used for map pakfiles.
//!
//! Every BSP may embed a zip archive (lump 40, the *pakfile*) carrying
//! map-specific content: cubemap textures baked by `buildcubemaps`, patched
//! materials, custom models. While a map is loaded the engine mounts that
//! archive above every other search path, and so does rustlife.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{Mutex, PoisonError};

use zip::ZipArchive;

use crate::error::{Error, Result};
use crate::mount::Mount;
use crate::path::normalize;

/// A zip archive held in memory and exposed as a [`Mount`].
pub struct ZipMount {
    label: String,
    /// Canonical path -> index within the archive.
    index: HashMap<String, usize>,
    /// `ZipArchive` needs `&mut` to read; pakfiles are small and read
    /// infrequently, so a mutex is simpler than per-thread archive clones.
    archive: Mutex<ZipArchive<Cursor<Vec<u8>>>>,
}

impl std::fmt::Debug for ZipMount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZipMount")
            .field("label", &self.label)
            .field("files", &self.index.len())
            .finish_non_exhaustive()
    }
}

impl ZipMount {
    /// Opens a zip archive from its raw bytes. `label` identifies it in logs,
    /// for example `"pakfile of maps/dm_lockdown.bsp"`.
    ///
    /// # Errors
    ///
    /// Fails if the bytes are not a readable zip archive.
    pub fn from_bytes(label: impl Into<String>, bytes: Vec<u8>) -> Result<Self> {
        let label = label.into();
        let archive = ZipArchive::new(Cursor::new(bytes)).map_err(|source| Error::Zip {
            context: label.clone(),
            source,
        })?;
        let mut index = HashMap::with_capacity(archive.len());
        for position in 0..archive.len() {
            // zip 9's `name_for_index` yields `Option<Result<Cow<str>>>`: skip
            // entries with no name or an undecodable one rather than failing the
            // whole mount over a single bad record.
            let Some(Ok(name)) = archive.name_for_index(position) else {
                continue;
            };
            if name.ends_with('/') {
                continue; // directory entry
            }
            // On duplicate (case-insensitive) names the first entry wins.
            index.entry(normalize(&name)).or_insert(position);
        }
        Ok(Self {
            label,
            index,
            archive: Mutex::new(archive),
        })
    }

    /// Number of files in the archive.
    #[must_use]
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the archive contains no files.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

impl Mount for ZipMount {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let Some(&position) = self.index.get(path) else {
            return Ok(None);
        };
        let mut archive = self.archive.lock().unwrap_or_else(PoisonError::into_inner);
        let zip_error = |source| Error::Zip {
            context: format!("{} ({path})", self.label),
            source,
        };
        let mut file = archive.by_index(position).map_err(zip_error)?;
        let mut data = Vec::with_capacity(usize::try_from(file.size()).unwrap_or(0));
        file.read_to_end(&mut data)
            .map_err(|e| Error::io(format!("{}/{path}", self.label), e))?;
        Ok(Some(data))
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
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_entries_case_insensitively() {
        let bytes = build_zip(&[
            ("materials/maps/dm_test/Cubemap.VTF", b"cube"),
            ("readme.txt", b"hi"),
        ]);
        let mount = ZipMount::from_bytes("test pak", bytes).unwrap();
        assert_eq!(mount.len(), 2);
        assert_eq!(
            mount
                .read("materials/maps/dm_test/cubemap.vtf")
                .unwrap()
                .unwrap(),
            b"cube"
        );
        assert_eq!(mount.read("missing").unwrap(), None);
        assert!(mount.contains("readme.txt"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(
            ZipMount::from_bytes("bad", b"not a zip".to_vec()),
            Err(Error::Zip { .. })
        ));
    }
}
