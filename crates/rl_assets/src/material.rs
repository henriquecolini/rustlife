//! VMT material loading: `source://materials/foo.vmt` becomes a
//! [`SourceMaterial`] — texture handles plus the flags the renderer needs.
//!
//! This stays free of `bevy_pbr`: it resolves the material (including `patch`
//! shaders, by reading the included VMT through the asset system) and loads its
//! textures as `Image` handles, but leaves building a `StandardMaterial` to the
//! client. That keeps the asset layer usable by the headless server.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, ReadAssetBytesError};
use bevy_image::Image;
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::vmt::{self, MaterialDesc, VmtError};
use thiserror::Error;

use crate::source::SOURCE;

/// A resolved Source material: the textures it references and how it should be
/// drawn. The client turns this into a `StandardMaterial`.
#[derive(Asset, TypePath, Debug, Clone, Default)]
pub struct SourceMaterial {
    /// `$basetexture` as a loaded image, if any.
    pub base_color: Option<Handle<Image>>,
    /// `$bumpmap` (normal map) as a loaded image, if any.
    pub normal: Option<Handle<Image>>,
    /// Whether the surface is translucent (`$translucent` or water).
    pub translucent: bool,
    /// `$alphatest` reference value, if alpha testing is enabled.
    pub alpha_test: Option<f32>,
    /// Whether back-face culling is disabled (`$nocull`).
    pub no_cull: bool,
}

/// Errors from [`VmtLoader`].
#[derive(Debug, Error)]
pub enum VmtLoadError {
    /// The asset bytes could not be read from the reader.
    #[error("reading VMT bytes: {0}")]
    Read(#[from] std::io::Error),
    /// The VMT (or its patch base) did not parse.
    #[error("parsing VMT: {0}")]
    Parse(#[from] VmtError),
    /// A `patch` material's included base VMT could not be read.
    #[error("reading included VMT: {0}")]
    Include(#[from] ReadAssetBytesError),
}

/// Loads a `.vmt` file into a [`SourceMaterial`].
#[derive(Default, TypePath)]
pub struct VmtLoader;

impl AssetLoader for VmtLoader {
    type Asset = SourceMaterial;
    type Settings = ();
    type Error = VmtLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<SourceMaterial, VmtLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();

        let desc = resolve(load_context, &text).await?;

        Ok(SourceMaterial {
            base_color: desc
                .base_texture
                .as_deref()
                .map(|t| load_context.load(texture_path(t))),
            normal: desc
                .bump_map
                .as_deref()
                .map(|t| load_context.load(texture_path(t))),
            translucent: desc.translucent,
            alpha_test: desc.alpha_test,
            no_cull: desc.no_cull,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["vmt"]
    }
}

/// Parses the material, following a `patch` to its base by reading the included
/// VMT's bytes through the asset system (so it resolves out of the same mounted
/// content).
async fn resolve(
    load_context: &mut LoadContext<'_>,
    text: &str,
) -> Result<MaterialDesc, VmtLoadError> {
    if let Some(include) = vmt::patch_include(text)? {
        let base_bytes = load_context.read_asset_bytes(source_path(&include)).await?;
        let base_text = String::from_utf8_lossy(&base_bytes).into_owned();
        Ok(vmt::parse_resolved(text, move |_| Ok(base_text))?)
    } else {
        Ok(vmt::parse(text)?)
    }
}

/// `materials/<name>.vtf` under the `source` asset source, with backslashes
/// normalised to forward slashes (the VFS lower-cases the rest).
fn texture_path(texture: &str) -> String {
    format!("{SOURCE}://materials/{}.vtf", texture.replace('\\', "/"))
}

/// A VMT `include` path (already rooted, e.g. `materials/brick/wall.vmt`) under
/// the `source` asset source.
fn source_path(include: &str) -> String {
    format!("{SOURCE}://{}", include.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_and_include_paths_use_the_source() {
        assert_eq!(
            texture_path("concrete\\floor001a"),
            "source://materials/concrete/floor001a.vtf"
        );
        assert_eq!(
            source_path("materials/brick/wall.vmt"),
            "source://materials/brick/wall.vmt"
        );
    }
}
