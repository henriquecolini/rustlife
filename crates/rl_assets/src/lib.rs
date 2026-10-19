//! Bevy asset integration for Source engine content.
//!
//! This crate bridges the rustlife virtual filesystem (`source_fs`) to Bevy's
//! asset system. It registers a custom [`AssetSource`](bevy_asset::io::AssetSource)
//! under the name `source`, so any asset addressed as `source://<path>` is
//! served out of the player's mounted Half-Life 2: Deathmatch install instead
//! of from a directory on disk. Every content load in rustlife — a texture, a
//! material, a map, a model, a sound — goes through this source, which means
//! the higher layers never touch the filesystem or care where a file physically
//! lives (base game, Workshop item, map pakfile, ...).
//!
//! Loaders that turn those bytes into Bevy assets (VTF → `Image`,
//! VMT → material, ...) build on [`source::VfsAssetReader`] and are added in
//! later milestones.

#![forbid(unsafe_code)]

pub mod source;

pub use source::{SOURCE, SourceAssetPlugin, VfsAssetReader};
