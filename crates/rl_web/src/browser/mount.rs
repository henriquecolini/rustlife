//! Mounting the imported install.

use source_fs::Vfs;
use source_fs::remote::{MountedRemote, mount_remote_game};

use super::error::BrowserError;
use super::storage::{OpfsTree, read_manifest};
use crate::manifest::ImportStatus;

/// What the browser holds of the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    /// Nothing has been imported.
    Empty,
    /// An import started and did not finish.
    Partial {
        /// Files copied so far.
        files: usize,
        /// Bytes copied so far.
        bytes: u64,
    },
    /// The game is fully imported.
    Complete {
        /// Files imported.
        files: usize,
        /// Bytes imported.
        bytes: u64,
    },
}

/// Why the imported game could not be mounted.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    /// Nothing has been imported yet.
    #[error("the game files have not been imported yet")]
    NothingImported,
    /// An import was interrupted; run it again to finish.
    #[error("the import did not finish; run it again to continue")]
    Incomplete,
    /// The browser failed, or the import record is damaged.
    #[error("cannot read the imported files: {0}")]
    Storage(String),
    /// The files are there but are not a usable game.
    #[error(transparent)]
    Game(#[from] source_fs::Error),
}

/// What has been imported.
///
/// # Errors
///
/// Fails if the browser's storage cannot be read or the record is damaged.
pub async fn install_status() -> Result<InstallStatus, BrowserError> {
    Ok(match read_manifest().await? {
        None => InstallStatus::Empty,
        Some(manifest) => {
            let (files, bytes) = (manifest.len(), manifest.total_bytes());
            match manifest.status() {
                ImportStatus::Partial if manifest.is_empty() => InstallStatus::Empty,
                ImportStatus::Partial => InstallStatus::Partial { files, bytes },
                ImportStatus::Complete => InstallStatus::Complete { files, bytes },
            }
        }
    })
}

/// Mounts the imported game (its mod folder `mod_dir`) into `vfs`, below any
/// mounts already there.
///
/// # Errors
///
/// See [`MountError`].
pub async fn mount_installed(
    vfs: &Vfs,
    mod_dir: &str,
    budget: usize,
) -> Result<MountedRemote, MountError> {
    let manifest = read_manifest()
        .await
        .map_err(|error| MountError::Storage(error.to_string()))?
        .ok_or(MountError::NothingImported)?;
    if manifest.status() != ImportStatus::Complete {
        return Err(MountError::Incomplete);
    }
    let tree = OpfsTree::from_manifest(&manifest);
    Ok(mount_remote_game(vfs, &tree, mod_dir, budget).await?)
}
