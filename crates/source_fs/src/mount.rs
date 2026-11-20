//! The [`Mount`] abstraction: one searchable source of content files.

use std::fmt;

use crate::byte_source::BoxFuture;
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
    /// truncated archive, checksum mismatch), or [`crate::Error::NotResident`]
    /// if the mount has the file but only fetches it asynchronously and it has
    /// not been made resident (see [`Mount::make_resident`]).
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>>;

    /// Reads the whole file at `path` without blocking, or `Ok(None)` if this
    /// mount does not contain it.
    ///
    /// Mounts whose bytes arrive asynchronously (a browser's storage) fetch
    /// here. The file is *not* kept: callers that want to read it
    /// synchronously later use [`Mount::make_resident`]. The default, right
    /// for mounts that read synchronously, just calls [`Mount::read`].
    ///
    /// # Errors
    ///
    /// As [`Mount::read`].
    fn read_async<'a>(&'a self, path: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>>> {
        Box::pin(std::future::ready(self.read(path)))
    }

    /// Makes the file at `path` readable by [`Mount::read`] without waiting,
    /// keeping it in memory (and, when `pinned`, never dropping it to make
    /// room). A file the mount does not contain is ignored.
    ///
    /// For mounts that read synchronously every file already is, which is the
    /// default.
    ///
    /// # Errors
    ///
    /// Fails if the file exists but cannot be fetched.
    fn make_resident<'a>(&'a self, path: &'a str, pinned: bool) -> BoxFuture<'a, Result<()>> {
        let _ = (path, pinned);
        Box::pin(std::future::ready(Ok(())))
    }

    /// Returns whether this mount contains a file at `path`.
    fn contains(&self, path: &str) -> bool;

    /// Calls `visit` with the canonical path of every file in the mount, in
    /// unspecified order.
    fn for_each_file(&self, visit: &mut dyn FnMut(&str));

    /// A short human-readable description (usually the on-disk location) for
    /// logs and tooling.
    fn describe(&self) -> String;
}
