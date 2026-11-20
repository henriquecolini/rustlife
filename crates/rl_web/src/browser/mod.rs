//! Everything that talks to the browser. Exists only for `wasm32`.
//!
//! The JavaScript half is `js/storage.js`: plumbing for the browser's private
//! filesystem and folder picker. This module wraps it in Rust types and adds
//! the logic around it: an [`OpfsTree`] for `source_fs` to mount, the folder
//! [`intake`], the resumable [`copy`] and [`mount_installed`].

mod boot;
mod copy;
mod error;
mod intake;
mod mount;
mod storage;

pub use boot::{
    choose_folder, import_game, list_maps, play, probe_file, remove_game, start, status,
    texture_bytes,
};
pub use copy::{Progress, run_import};
pub use error::BrowserError;
pub use intake::{PickedFolder, pick_folder};
pub use mount::{InstallStatus, MountError, install_status, mount_installed};
pub use storage::{
    OpfsSource, OpfsTree, StorageEstimate, estimate, read_manifest, remove_install,
    request_persistence, storage_available, write_bytes, write_manifest,
};
