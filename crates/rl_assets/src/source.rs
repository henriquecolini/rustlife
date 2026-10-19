//! The `source://` asset source: a Bevy [`AssetReader`] backed by the VFS.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy_app::{App, Plugin};
use bevy_asset::AssetApp;
use bevy_asset::io::{
    AssetReader, AssetReaderError, AssetSourceBuilder, ErasedAssetReader, PathStream, Reader,
    VecReader,
};
use source_fs::Vfs;
use source_fs::path::normalize;

/// The name of the asset source rustlife content is loaded through. An asset
/// path of `source://materials/foo.vmt` resolves to the VFS path
/// `materials/foo.vmt`.
pub const SOURCE: &str = "source";

/// A Bevy [`AssetReader`] that serves files from a [`Vfs`].
///
/// Cloning is cheap — it only clones the `Arc` — which matters because Bevy
/// constructs a fresh reader (through the closure given to
/// [`AssetSourceBuilder::new`]) for each worker it spins up.
#[derive(Clone)]
pub struct VfsAssetReader {
    vfs: Arc<Vfs>,
}

impl VfsAssetReader {
    /// Creates a reader over `vfs`.
    #[must_use]
    pub fn new(vfs: Arc<Vfs>) -> Self {
        Self { vfs }
    }

    /// Converts a Bevy asset path into a canonical VFS path.
    fn vfs_path(path: &Path) -> String {
        normalize(&path.to_string_lossy())
    }
}

// The VFS answers every query synchronously (reads are from memory-mapped or
// already-open archives), so these `async fn` bodies contain no `.await`. The
// `async fn` signature is nonetheless the form `AssetReader` documents and
// requires, so the lint against "pointless" async does not apply here.
#[allow(clippy::unused_async_trait_impl)]
impl AssetReader for VfsAssetReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        match self.vfs.read(&Self::vfs_path(path)) {
            Ok(Some(bytes)) => Ok(VecReader::new(bytes)),
            Ok(None) => Err(AssetReaderError::NotFound(path.to_path_buf())),
            Err(error) => Err(AssetReaderError::Io(Arc::new(io::Error::other(
                error.to_string(),
            )))),
        }
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        // rustlife ships no `.meta` sidecar files; every asset uses the default
        // loader settings. Reporting `NotFound` is how Bevy is told to do that.
        // The turbofish fixes the otherwise-unconstrained `impl Reader` type.
        Err::<VecReader, _>(AssetReaderError::NotFound(path.to_path_buf()))
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        let prefix = Self::vfs_path(path);
        let glob = if prefix.is_empty() {
            "**".to_owned()
        } else {
            format!("{prefix}/**")
        };
        let entries: Vec<PathBuf> = self
            .vfs
            .list(&glob)
            .into_iter()
            .map(|file| PathBuf::from(file.path))
            .collect();
        Ok(Box::new(futures_lite::stream::iter(entries)))
    }

    async fn is_directory<'a>(&'a self, path: &'a Path) -> Result<bool, AssetReaderError> {
        let prefix = Self::vfs_path(path);
        // A VFS path is a "directory" if any mounted file lives under it. There
        // is no cheaper query than listing one level, but `is_directory` is only
        // consulted for folder loads, which rustlife does not use on the hot
        // path.
        Ok(!self.vfs.list(&format!("{prefix}/*")).is_empty())
    }
}

/// Registers the [`SOURCE`] asset source so `source://` paths load from `vfs`.
///
/// Add this to the `App` **before** Bevy's `AssetPlugin` (i.e. before
/// `DefaultPlugins`), or the registration is ignored and a warning is logged;
/// this is a Bevy requirement for custom asset sources.
///
/// Bevy may build a reader per asset-loading worker, so the registered closure
/// clones the `Arc<Vfs>` each time it is called.
pub struct SourceAssetPlugin {
    vfs: Arc<Vfs>,
}

impl SourceAssetPlugin {
    /// Creates the plugin serving content from `vfs`.
    #[must_use]
    pub fn new(vfs: Arc<Vfs>) -> Self {
        Self { vfs }
    }
}

impl Plugin for SourceAssetPlugin {
    fn build(&self, app: &mut App) {
        let vfs = self.vfs.clone();
        app.register_asset_source(
            SOURCE,
            AssetSourceBuilder::new(move || -> Box<dyn ErasedAssetReader> {
                Box::new(VfsAssetReader::new(vfs.clone()))
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::AsyncReadExt;
    use futures_lite::future::block_on;
    use source_fs::{LooseDirectory, MountOrigin};

    /// A VFS with a single loose directory containing `materials/x.vmt`.
    fn fixture() -> (tempfile::TempDir, Arc<Vfs>) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("materials")).expect("mkdir");
        std::fs::write(dir.path().join("materials/x.vmt"), b"hello").expect("write");
        let vfs = Arc::new(Vfs::new());
        vfs.mount_back(
            LooseDirectory::open(dir.path()).expect("open"),
            MountOrigin::BaseGame,
        );
        (dir, vfs)
    }

    // The tests call the trait methods through `AssetReader::` to disambiguate
    // from the blanket `ErasedAssetReader` impl, which also provides inherent
    // methods of the same names (with different, boxed signatures).

    #[test]
    fn reads_an_existing_file() {
        let (_dir, vfs) = fixture();
        let reader = VfsAssetReader::new(vfs);
        let bytes = block_on(async {
            let mut reader = AssetReader::read(&reader, Path::new("materials/x.vmt"))
                .await
                .expect("read");
            let mut buffer = Vec::new();
            AsyncReadExt::read_to_end(&mut reader, &mut buffer)
                .await
                .expect("drain");
            buffer
        });
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn mixed_case_paths_resolve() {
        let (_dir, vfs) = fixture();
        let reader = VfsAssetReader::new(vfs);
        let found = block_on(AssetReader::read(&reader, Path::new("Materials/X.VMT")));
        assert!(found.is_ok(), "VFS lookup is case-insensitive");
    }

    #[test]
    fn missing_file_is_not_found() {
        let (_dir, vfs) = fixture();
        let reader = VfsAssetReader::new(vfs);
        let error = block_on(AssetReader::read(&reader, Path::new("maps/nope.bsp"))).err();
        assert!(matches!(error, Some(AssetReaderError::NotFound(_))));
    }

    #[test]
    fn directory_detection_and_listing() {
        let (_dir, vfs) = fixture();
        let reader = VfsAssetReader::new(vfs);
        assert!(block_on(AssetReader::is_directory(&reader, Path::new("materials"))).expect("dir"));
        assert!(
            !block_on(AssetReader::is_directory(
                &reader,
                Path::new("materials/x.vmt")
            ))
            .expect("dir")
        );
    }
}
