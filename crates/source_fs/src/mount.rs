//! The [`Mount`] abstraction: one searchable source of content files.

use std::fmt;

use crate::error::Result;

/// A read-only source of content files addressed by canonical virtual path.
///
/// Implementations exist for VPK archives ([`crate::vpk::VpkArchive`]), loose
/// directories ([`crate::loose::LooseDirectory`]) and zip archives such as a
/// map's embedded pakfile ([`crate::zip_mount::ZipMount`]).
///
/// All `path` arguments are expected to be canonical (see
/// [`crate::path::normalize`]); [`crate::Vfs`] normalizes before dispatching.
pub trait Mount: Send + Sync + fmt::Debug {
    /// Reads the whole file at `path`, or returns `Ok(None)` if this mount does
    /// not contain it.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read (I/O failure,
    /// truncated archive, checksum mismatch).
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>>;

    /// Returns whether this mount contains a file at `path`.
    fn contains(&self, path: &str) -> bool;

    /// Calls `visit` with the canonical path of every file in the mount, in
    /// unspecified order.
    fn for_each_file(&self, visit: &mut dyn FnMut(&str));

    /// A short human-readable description (usually the on-disk location) for
    /// logs and tooling.
    fn describe(&self) -> String;
}
