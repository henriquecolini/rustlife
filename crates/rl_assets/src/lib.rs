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

use bevy_app::{App, Plugin};
use bevy_asset::AssetApp;

pub mod coords;
pub mod map;
pub mod material;
pub mod source;
pub mod texture;

pub use map::{BspLoader, MapAsset, MapLoadError, MapMesh};
pub use material::{SourceMaterial, VmtLoadError, VmtLoader};
pub use source::{SOURCE, SourceAssetPlugin, VfsAssetReader};
pub use texture::{VtfLoadError, VtfLoader};

/// Registers rustlife's asset loaders (VTF textures, BSP maps, and more in
/// later milestones) and the asset types they produce.
///
/// Unlike [`SourceAssetPlugin`], which must be added *before* Bevy's
/// `AssetPlugin`, this plugin registers loaders and must be added *after* it
/// (i.e. after `DefaultPlugins`), once the `AssetServer` and the `Image` and
/// `Mesh` asset types exist.
pub struct SourceLoadersPlugin;

impl Plugin for SourceLoadersPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<MapAsset>();
        app.init_asset::<SourceMaterial>();
        app.register_asset_loader(VtfLoader);
        app.register_asset_loader(VmtLoader);
        app.register_asset_loader(BspLoader);
    }
}
