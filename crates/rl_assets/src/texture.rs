//! VTF texture loading: `source://**/*.vtf` becomes a Bevy [`Image`].
//!
//! DXT textures (most of the game's) are uploaded as the GPU's BC formats
//! with the file's own mip chain when the GPU samples those, at a quarter to
//! an eighth of the memory of RGBA8; everything else, and every texture on
//! GPUs without BC support, is decoded to RGBA8 with a generated mip chain.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

use bevy_asset::io::Reader;
use bevy_asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy_image::{Image, ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use serde::{Deserialize, Serialize};
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

/// How detailed the textures load, shared between the settings and the
/// loader: textures loaded afterwards drop their largest mips and filter
/// anisotropically as set here.
#[derive(Debug, Clone, bevy_ecs::resource::Resource)]
pub struct TextureDetail {
    drop_mips: Arc<AtomicU32>,
    anisotropy: Arc<AtomicU16>,
}

impl Default for TextureDetail {
    fn default() -> Self {
        Self {
            drop_mips: Arc::new(AtomicU32::new(0)),
            anisotropy: Arc::new(AtomicU16::new(ANISOTROPY)),
        }
    }
}

impl TextureDetail {
    /// Sets how many of a texture's largest mips are left out (never its
    /// last) and the anisotropic filtering, `1..=16`.
    pub fn set(&self, drop_mips: u32, anisotropy: u16) {
        self.drop_mips.store(drop_mips, Ordering::Relaxed);
        self.anisotropy
            .store(anisotropy.clamp(1, 16), Ordering::Relaxed);
    }

    fn drop_mips(&self) -> u32 {
        self.drop_mips.load(Ordering::Relaxed)
    }

    fn anisotropy(&self) -> u16 {
        self.anisotropy.load(Ordering::Relaxed)
    }
}

/// How a texture is loaded, beyond how detailed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VtfSettings {
    /// Load it as a `$selfillum` mask: the colour times the alpha, which is
    /// how much the surface shines with its own light. (Always RGBA8.)
    pub self_illum: bool,
}

/// Loads a `.vtf` file into an [`Image`]: block-compressed when the GPU
/// samples BC formats (`bc`), else decoded RGBA8.
#[derive(Default, TypePath)]
pub struct VtfLoader {
    /// Whether the GPU samples BC1–3 textures: set once the GPU is known,
    /// which is after the loader is registered.
    pub bc: Arc<AtomicBool>,
    /// How detailed to load.
    pub detail: TextureDetail,
}

impl AssetLoader for VtfLoader {
    type Asset = Image;
    type Settings = VtfSettings;
    type Error = VtfLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &VtfSettings,
        _load_context: &mut LoadContext<'_>,
    ) -> Result<Image, VtfLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        if settings.self_illum {
            let mut texture = vtf::decode(&bytes)?;
            masked_by_alpha(&mut texture);
            return Ok(image_from_decoded(texture, &self.detail));
        }
        if self.bc.load(Ordering::Relaxed)
            && let Some(texture) = vtf::compressed(&bytes)
        {
            return Ok(image_from_blocks(texture, &self.detail));
        }
        Ok(image_from_decoded(vtf::decode(&bytes)?, &self.detail))
    }

    fn extensions(&self) -> &[&str] {
        &["vtf"]
    }
}

/// Multiplies each pixel's colour by its alpha and makes it opaque: the
/// colour a `$selfillum` surface shines with.
fn masked_by_alpha(texture: &mut DecodedTexture) {
    for pixel in texture.rgba8.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u16::from(*channel) * alpha / 255) as u8;
        }
        pixel[3] = 255;
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
fn image_from_decoded(texture: DecodedTexture, detail: &TextureDetail) -> Image {
    let format = if texture.is_normal_map {
        TextureFormat::Rgba8Unorm
    } else {
        TextureFormat::Rgba8UnormSrgb
    };
    let (width, height) = (texture.width, texture.height);
    let (chain, levels) = mip_chain(width, height, &texture.rgba8);
    let (width, height, chain, levels) =
        drop_levels(width, height, chain, levels, detail.drop_mips());

    // `Image::new` validates the data against a single mip level, so build with
    // the base level and then swap in the whole chain.
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        vec![0; width as usize * height as usize * 4],
        format,
        // Drop the CPU copy after upload; world textures are not read back.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(chain);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = tiling_sampler(detail.anisotropy());
    image
}

/// Wraps a texture's stored blocks in an [`Image`] of the matching BC
/// format, with the file's mip chain and a tiling sampler.
fn image_from_blocks(mut texture: CompressedTexture, detail: &TextureDetail) -> Image {
    let format = match (texture.format, texture.is_normal_map) {
        (BlockFormat::Dxt1, false) => TextureFormat::Bc1RgbaUnormSrgb,
        (BlockFormat::Dxt1, true) => TextureFormat::Bc1RgbaUnorm,
        (BlockFormat::Dxt3, false) => TextureFormat::Bc2RgbaUnormSrgb,
        (BlockFormat::Dxt3, true) => TextureFormat::Bc2RgbaUnorm,
        (BlockFormat::Dxt5, false) => TextureFormat::Bc3RgbaUnormSrgb,
        (BlockFormat::Dxt5, true) => TextureFormat::Bc3RgbaUnorm,
    };
    // BC textures keep whole blocks: never go below 4×4 pixels.
    for _ in 0..detail.drop_mips() {
        if texture.mips.len() < 2 || texture.width < 8 || texture.height < 8 {
            break;
        }
        texture.mips.remove(0);
        texture.width /= 2;
        texture.height /= 2;
    }
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
    image.sampler = tiling_sampler(detail.anisotropy());
    image
}

/// A repeating, anisotropic sampler: world textures tile and are seen at
/// grazing angles.
fn tiling_sampler(anisotropy: u16) -> ImageSampler {
    ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        address_mode_w: ImageAddressMode::Repeat,
        anisotropy_clamp: anisotropy,
        ..ImageSamplerDescriptor::linear()
    })
}

/// Leaves out the first `drop` levels of a mip `chain` (never the last).
fn drop_levels(
    mut width: u32,
    mut height: u32,
    chain: Vec<u8>,
    mut levels: u32,
    drop: u32,
) -> (u32, u32, Vec<u8>, u32) {
    let mut start = 0;
    for _ in 0..drop {
        if levels < 2 {
            break;
        }
        start += width as usize * height as usize * 4;
        width = (width / 2).max(1);
        height = (height / 2).max(1);
        levels -= 1;
    }
    (width, height, chain[start..].to_vec(), levels)
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
        let image = image_from_decoded(decoded, &TextureDetail::default());
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
        let image = image_from_decoded(decoded, &TextureDetail::default());
        assert_eq!(image.texture_descriptor.mip_level_count, 3);
        let ImageSampler::Descriptor(sampler) = &image.sampler else {
            panic!("expected an explicit sampler");
        };
        assert_eq!(sampler.address_mode_u, ImageAddressMode::Repeat);
        assert_eq!(sampler.address_mode_v, ImageAddressMode::Repeat);
    }

    fn blocks() -> CompressedTexture {
        CompressedTexture {
            format: BlockFormat::Dxt5,
            width: 8,
            height: 4,
            mips: vec![vec![0; 2 * 16], vec![0; 16], vec![0; 16], vec![0; 16]],
            is_normal_map: false,
        }
    }

    #[test]
    fn lower_detail_drops_the_largest_mips_but_keeps_whole_blocks() {
        let detail = TextureDetail::default();
        detail.set(2, 4);
        // 8x4 cannot halve into less than a block.
        let image = image_from_blocks(blocks(), &detail);
        assert_eq!((image.width(), image.height()), (8, 4));
        let mut big = blocks();
        big.width = 32;
        big.height = 32;
        big.mips = vec![
            vec![0; 64 * 16],
            vec![0; 16 * 16],
            vec![0; 4 * 16],
            vec![0; 16],
        ];
        let image = image_from_blocks(big, &detail);
        assert_eq!((image.width(), image.height()), (8, 8));
        assert_eq!(image.texture_descriptor.mip_level_count, 2);
        let ImageSampler::Descriptor(sampler) = &image.sampler else {
            panic!("expected an explicit sampler");
        };
        assert_eq!(sampler.anisotropy_clamp, 4);
    }

    #[test]
    fn decoded_textures_drop_mips_and_keep_the_last() {
        let detail = TextureDetail::default();
        detail.set(1, 8);
        let decoded = DecodedTexture {
            width: 8,
            height: 8,
            rgba8: vec![7u8; 8 * 8 * 4],
            is_normal_map: false,
        };
        let image = image_from_decoded(decoded, &detail);
        assert_eq!((image.width(), image.height()), (4, 4));
        assert_eq!(image.texture_descriptor.mip_level_count, 3);
        detail.set(9, 8);
        let tiny = DecodedTexture {
            width: 2,
            height: 2,
            rgba8: vec![7u8; 16],
            is_normal_map: false,
        };
        let image = image_from_decoded(tiny, &detail);
        assert_eq!(image.texture_descriptor.mip_level_count, 1);
    }

    #[test]
    fn blocks_become_a_bc_image_with_their_mips() {
        let image = image_from_blocks(blocks(), &TextureDetail::default());
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
        let image = image_from_decoded(decoded, &TextureDetail::default());
        assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    }

    #[test]
    fn a_self_illum_mask_is_the_colour_where_the_alpha_says_so() {
        let mut texture = DecodedTexture {
            width: 2,
            height: 1,
            rgba8: vec![200, 100, 50, 255, 200, 100, 50, 0],
            is_normal_map: false,
        };
        masked_by_alpha(&mut texture);
        assert_eq!(texture.rgba8, [200, 100, 50, 255, 0, 0, 0, 255]);
        let mut half = DecodedTexture {
            width: 1,
            height: 1,
            rgba8: vec![200, 100, 50, 128],
            is_normal_map: false,
        };
        masked_by_alpha(&mut half);
        assert_eq!(half.rgba8, [100, 50, 25, 255]);
    }
}
