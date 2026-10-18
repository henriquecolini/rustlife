//! Read-only access to Source engine game content.
//!
//! rustlife never ships game assets. Instead it reads them at runtime from the
//! player's own legally installed copy of Half-Life 2: Deathmatch, the same
//! way the original engine does. This crate is the bottom layer of that
//! pipeline:
//!
//! * [`steam`] finds Steam library folders, installed apps and downloaded
//!   workshop items, purely from files on disk;
//! * [`gameinfo`] parses `gameinfo.txt` and resolves its search paths;
//! * [`vpk`], [`loose`] and [`zip_mount`] implement [`Mount`] for VPK
//!   archives, loose directories and map pakfiles;
//! * [`Vfs`] stacks mounts by priority with case-insensitive lookup;
//! * [`content::mount_game`] ties it together into a ready-to-use [`Vfs`];
//! * [`keyvalues`] parses Valve's KeyValues text format, used by most of the
//!   above and by higher layers (materials, soundscripts, entity data).
//!
//! # Example
//!
//! ```no_run
//! use source_fs::content::{mount_game, ContentOptions};
//!
//! let game = mount_game(&ContentOptions::default())?;
//! for map in game.vfs.list("maps/*.bsp") {
//!     println!("{} ({})", map.path, map.origin);
//! }
//! # Ok::<(), source_fs::Error>(())
//! ```
//!
//! # Paths
//!
//! All virtual paths are canonicalized by [`path::normalize`]: lowercase,
//! forward slashes, no leading or trailing separators. Public APIs accept any
//! spelling and normalize internally.

#![forbid(unsafe_code)]

pub mod content;
pub mod error;
pub mod gameinfo;
mod io;
pub mod keyvalues;
pub mod loose;
pub mod mount;
pub mod path;
pub mod steam;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod vfs;
pub mod vpk;
pub mod zip_mount;

pub use error::{Error, Result};
pub use loose::LooseDirectory;
pub use mount::Mount;
pub use vfs::{ListedFile, MountId, MountInfo, MountOrigin, Vfs};
pub use vpk::VpkArchive;
pub use zip_mount::ZipMount;
