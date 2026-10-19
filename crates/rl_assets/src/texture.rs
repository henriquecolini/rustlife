//! VTF texture loading: `source://**/*.vtf` becomes a Bevy [`Image`].

use bevy_asset::io::Reader;
use bevy_asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy_image::Image;
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::vtf::{self, DecodedTexture, VtfError};
use thiserror::Error;
use wgpu_types::{Extent3d, TextureDimension, TextureFormat};

/// Errors from [`VtfLoader`].
#[derive(Debug, Error)]
pub enum VtfLoadError {
    /// The asset bytes could not be read from the reader.
    #[error("reading VTF bytes: {0}")]
    Read(#[from] std::io::Error),
    /// The bytes are not a decodable VTF.
    #[error(transparent)]
    Decode(#[from] VtfError),
}

/// Loads a `.vtf` file into an [`Image`] with the decoded RGBA8 pixels.
#[derive(Default, TypePath)]
pub struct VtfLoader;

impl AssetLoader for VtfLoader {
    type Asset = Image;
    type Settings = ();
    type Error = VtfLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Image, VtfLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        Ok(image_from_decoded(vtf::decode(&bytes)?))
    }

    fn extensions(&self) -> &[&str] {
        &["vtf"]
    }
}

/// Wraps decoded VTF pixels in a GPU-ready [`Image`].
///
/// Colour textures are tagged sRGB and normal maps linear, following the VTF
/// `NORMAL` flag. The material layer may later rebuild a texture with the
/// opposite colour space when the shader binding demands it.
fn image_from_decoded(texture: DecodedTexture) -> Image {
    let format = if texture.is_normal_map {
        TextureFormat::Rgba8Unorm
    } else {
        TextureFormat::Rgba8UnormSrgb
    };
    Image::new(
        Extent3d {
            width: texture.width,
            height: texture.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        texture.rgba8,
        format,
        // Drop the CPU copy after upload; world textures are not read back.
        RenderAssetUsages::RENDER_WORLD,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_texture_becomes_an_srgb_image() {
        let decoded = DecodedTexture {
            width: 2,
            height: 2,
            rgba8: vec![0u8; 2 * 2 * 4],
            is_normal_map: false,
        };
        let image = image_from_decoded(decoded);
        assert_eq!(image.width(), 2);
        assert_eq!(image.height(), 2);
        assert_eq!(
            image.texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
    }

    #[test]
    fn normal_map_becomes_a_linear_image() {
        let decoded = DecodedTexture {
            width: 1,
            height: 1,
            rgba8: vec![0u8; 4],
            is_normal_map: true,
        };
        let image = image_from_decoded(decoded);
        assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    }
}
