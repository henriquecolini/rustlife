//! VTF texture loading: `source://**/*.vtf` becomes a Bevy [`Image`].
//!
//! DXT textures (most of the game's) are uploaded as the GPU's BC formats
//! with the file's own mip chain when the GPU samples those, at a quarter to
//! an eighth of the memory of RGBA8; everything else, and every texture on
//! GPUs without BC support, is decoded to RGBA8 with a generated mip chain.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bevy_asset::io::Reader;
use bevy_asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy_image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::vtf::{self, BlockFormat, CompressedTexture, DecodedTexture, VtfError};
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

/// Loads a `.vtf` file into an [`Image`]: block-compressed when the GPU
/// samples BC formats (`bc`), else decoded RGBA8.
#[derive(Default, TypePath)]
pub struct VtfLoader {
    /// Whether the GPU samples BC1–3 textures: set once the GPU is known,
    /// which is after the loader is registered.
    pub bc: Arc<AtomicBool>,
}

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
        if self.bc.load(Ordering::Relaxed)
            && let Some(texture) = vtf::compressed(&bytes)
        {
            return Ok(image_from_blocks(texture));
        }
        Ok(image_from_decoded(vtf::decode(&bytes)?))
    }

    fn extensions(&self) -> &[&str] {
        &["vtf"]
    }
}

/// Maximum anisotropic filtering. World textures are viewed at grazing angles
/// constantly (floors, long corridors), where anisotropy matters most.
const ANISOTROPY: u16 = 8;

/// Wraps decoded VTF pixels in a GPU-ready [`Image`] with a full mip chain and
/// a tiling sampler.
///
/// Colour textures are tagged sRGB and normal maps linear, following the VTF
/// `NORMAL` flag. Source texture coordinates routinely run far outside `0..1`
/// (a wall tiles its texture many times), so the sampler must *repeat*: Bevy's
/// default clamps to the edge, which smears each texture's border pixels across
/// whole surfaces.
fn image_from_decoded(texture: DecodedTexture) -> Image {
    let format = if texture.is_normal_map {
        TextureFormat::Rgba8Unorm
    } else {
        TextureFormat::Rgba8UnormSrgb
    };
    let (width, height) = (texture.width, texture.height);
    let (chain, levels) = mip_chain(width, height, &texture.rgba8);

    // `Image::new` validates the data against a single mip level, so build with
    // the base level and then swap in the whole chain.
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        texture.rgba8,
        format,
        // Drop the CPU copy after upload; world textures are not read back.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(chain);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = tiling_sampler();
    image
}

/// Wraps a texture's stored blocks in an [`Image`] of the matching BC
/// format, with the file's mip chain and a tiling sampler.
fn image_from_blocks(texture: CompressedTexture) -> Image {
    let format = match (texture.format, texture.is_normal_map) {
        (BlockFormat::Dxt1, false) => TextureFormat::Bc1RgbaUnormSrgb,
        (BlockFormat::Dxt1, true) => TextureFormat::Bc1RgbaUnorm,
        (BlockFormat::Dxt3, false) => TextureFormat::Bc2RgbaUnormSrgb,
        (BlockFormat::Dxt3, true) => TextureFormat::Bc2RgbaUnorm,
        (BlockFormat::Dxt5, false) => TextureFormat::Bc3RgbaUnormSrgb,
        (BlockFormat::Dxt5, true) => TextureFormat::Bc3RgbaUnorm,
    };
    let levels = texture.mips.len() as u32;
    let mut image = Image::new_uninit(
        Extent3d {
            width: texture.width,
            height: texture.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(texture.mips.concat());
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = tiling_sampler();
    image
}

/// A repeating, anisotropic sampler: world textures tile and are seen at
/// grazing angles.
fn tiling_sampler() -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        anisotropy_clamp: ANISOTROPY,
        ..ImageSamplerDescriptor::linear()
    })
}

/// Builds a complete mip chain (base level first) for tightly packed RGBA8
/// pixels with a 2×2 box filter, returning the concatenated levels and their
/// count.
///
/// VTFs carry their own mips, but the `vtf` crate only exposes the base level,
/// and regenerating is cheap. The filter averages in the stored (sRGB) space,
/// which slightly darkens high-contrast detail in distant mips — acceptable for
/// world textures, and simple to replace with linear-space filtering later.
fn mip_chain(width: u32, height: u32, base: &[u8]) -> (Vec<u8>, u32) {
    let mut chain = base.to_vec();
    let mut levels = 1;
    let (mut w, mut h) = (width as usize, height as usize);
    let mut previous = base.to_vec();

    while w > 1 || h > 1 {
        let (next_w, next_h) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; next_w * next_h * 4];
        for y in 0..next_h {
            for x in 0..next_w {
                // Sample the 2×2 source block, clamping for odd or 1-wide sizes.
                let xs = [(x * 2).min(w - 1), (x * 2 + 1).min(w - 1)];
                let ys = [(y * 2).min(h - 1), (y * 2 + 1).min(h - 1)];
                for channel in 0..4 {
                    let sum: u32 = ys
                        .iter()
                        .flat_map(|&sy| xs.iter().map(move |&sx| (sy * w + sx) * 4 + channel))
                        .map(|index| u32::from(previous[index]))
                        .sum();
                    next[(y * next_w + x) * 4 + channel] = ((sum + 2) / 4) as u8;
                }
            }
        }
        chain.extend_from_slice(&next);
        previous = next;
        w = next_w;
        h = next_h;
        levels += 1;
    }
    (chain, levels)
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
    fn mip_chain_halves_down_to_one_pixel() {
        let base = vec![10u8; 8 * 4 * 4];
        let (chain, levels) = mip_chain(8, 4, &base);
        // 8x4, 4x2, 2x1, 1x1
        assert_eq!(levels, 4);
        assert_eq!(chain.len(), (8 * 4 + 4 * 2 + 2 + 1) * 4);
        assert!(
            chain.iter().all(|&v| v == 10),
            "uniform input stays uniform"
        );
    }

    #[test]
    fn mip_chain_box_filters_each_block() {
        // 2x2 checker of 0 and 200 averages to 100 at the 1x1 level.
        let base = [
            0, 0, 0, 255, 200, 200, 200, 255, 200, 200, 200, 255, 0, 0, 0, 255,
        ];
        let (chain, levels) = mip_chain(2, 2, &base);
        assert_eq!(levels, 2);
        assert_eq!(&chain[16..], &[100, 100, 100, 255]);
    }

    #[test]
    fn images_tile_and_carry_their_mips() {
        let decoded = DecodedTexture {
            width: 4,
            height: 4,
            rgba8: vec![0u8; 4 * 4 * 4],
            is_normal_map: false,
        };
        let image = image_from_decoded(decoded);
        assert_eq!(image.texture_descriptor.mip_level_count, 3);
        let ImageSampler::Descriptor(sampler) = &image.sampler else {
            panic!("expected an explicit sampler");
        };
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
    }

    #[test]
    fn blocks_become_a_bc_image_with_their_mips() {
        let image = image_from_blocks(CompressedTexture {
            format: BlockFormat::Dxt5,
            width: 8,
            height: 4,
            mips: vec![vec![0; 2 * 16], vec![0; 16], vec![0; 16], vec![0; 16]],
            is_normal_map: false,
        });
        assert_eq!(
            image.texture_descriptor.format,
            TextureFormat::Bc3RgbaUnormSrgb
        );
        assert_eq!(image.texture_descriptor.mip_level_count, 4);
        assert_eq!(image.data.as_ref().map(Vec::len), Some(80));
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
