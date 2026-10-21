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
//! Loaders turn those bytes into Bevy assets: VTF → `Image`, VMT →
//! [`SourceMaterial`], BSP → [`MapAsset`], MDL → [`ModelAsset`], VHV →
//! [`VertexLightingAsset`] and WAV/MP3 → [`SoundAsset`].

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bevy_app::{App, Plugin};
use bevy_asset::AssetApp;

pub mod coords;
pub mod irradiance;
pub mod map;
pub mod material;
pub mod model;
pub mod skeleton;
pub mod sound;
pub mod source;
pub mod texture;
pub mod vertex_lighting;

pub use map::{
    BspLoader, IrradianceVolumeData, MapAsset, MapBrushEntity, MapLoadError, MapMesh, PlacedProp,
    Spawn,
};
pub use material::{SourceMaterial, VmtLoadError, VmtLoader};
pub use model::{MdlLoader, ModelAsset, ModelLoadError, ModelPart};
pub use skeleton::{Joint, SequenceClips, Skeleton};
pub use sound::{SoundAsset, SoundLoadError, SoundLoader};
pub use source::{SOURCE, SourceAssetPlugin, VfsAssetReader};
pub use source_formats::props::PropKind;
pub use texture::{VtfLoadError, VtfLoader};
pub use vertex_lighting::{VertexLightingAsset, VhvLoadError, VhvLoader};

/// Registers rustlife's asset loaders (VTF textures, VMT materials, BSP
/// maps, MDL models, vertex lighting and sounds) and the asset types they
/// produce.
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
        app.init_asset::<ModelAsset>();
        app.init_asset::<VertexLightingAsset>();
        app.init_asset::<SoundAsset>();
        let bc = Arc::new(AtomicBool::new(false));
        app.insert_resource(BlockCompression(Arc::clone(&bc)));
        app.register_asset_loader(VtfLoader { bc });
        app.register_asset_loader(VmtLoader);
        app.register_asset_loader(BspLoader);
        app.register_asset_loader(MdlLoader);
        app.register_asset_loader(VhvLoader);
        app.register_asset_loader(SoundLoader);
    }

    /// Tells the texture loader, once the GPU is known, whether to keep
    /// block-compressed textures as they are.
    fn finish(&self, app: &mut App) {
        let bc = app
            .world()
            .get_resource::<bevy_image::CompressedImageFormatSupport>()
            .is_some_and(|support| support.0.contains(bevy_image::CompressedImageFormats::BC));
        if let Some(flag) = app.world().get_resource::<BlockCompression>() {
            flag.0.store(bc, Ordering::Relaxed);
        }
        tracing::info!(
            bc,
            "textures {}",
            if bc {
                "stay block-compressed"
            } else {
                "are decoded"
            }
        );
    }
}

/// Whether the texture loader uploads BC textures, shared with it.
#[derive(bevy_ecs::resource::Resource)]
struct BlockCompression(Arc<AtomicBool>);
