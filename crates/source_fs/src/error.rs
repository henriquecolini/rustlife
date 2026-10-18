//! Error type shared by every part of the virtual filesystem.

use std::io;
use std::path::PathBuf;

use crate::keyvalues::KvError;

/// Convenience alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while discovering, mounting or reading content.
///
/// Variants carry the on-disk path (and where relevant the virtual path or byte
/// offset) so that a log line is enough to locate a corrupt or missing file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An operating-system I/O failure.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// File or directory being accessed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },

    /// A VPK directory file is malformed.
    #[error("invalid VPK {path}: {reason}")]
    InvalidVpk {
        /// The `_dir.vpk` file.
        path: PathBuf,
        /// Human-readable description including the offending offset.
        reason: String,
    },

    /// A file read from an archive does not match its recorded checksum.
    #[error(
        "checksum mismatch for {virtual_path} in {archive}: expected {expected:08x}, got {actual:08x}"
    )]
    ChecksumMismatch {
        /// Archive the entry was read from.
        archive: PathBuf,
        /// Virtual path of the entry.
        virtual_path: String,
        /// CRC32 recorded in the directory.
        expected: u32,
        /// CRC32 of the bytes actually read.
        actual: u32,
    },

    /// A KeyValues document (gameinfo, manifest, ...) failed to parse.
    #[error("failed to parse {path}: {source}")]
    KeyValues {
        /// The document being parsed.
        path: PathBuf,
        /// Parser error with line and column.
        #[source]
        source: KvError,
    },

    /// A KeyValues document parsed but lacks a required key.
    #[error("{path} is missing required key `{key}`")]
    MissingKey {
        /// The document being inspected.
        path: PathBuf,
        /// Dotted path of the missing key.
        key: String,
    },

    /// A zip archive (BSP pakfile) is malformed.
    #[error("invalid zip archive {context}: {source}")]
    Zip {
        /// Which archive, e.g. `pakfile of maps/dm_lockdown.bsp`.
        context: String,
        /// Underlying error.
        #[source]
        source: zip::result::ZipError,
    },

    /// No installation of the requested game could be located.
    #[error("{0}")]
    GameNotFound(String),
}

impl Error {
    /// Builds an [`Error::Io`] for `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
