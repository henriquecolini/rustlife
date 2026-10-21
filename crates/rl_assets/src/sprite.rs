//! Sprites: a material's texture with every animation frame, for effects.
//!
//! Effects (muzzle flashes, sparks, explosions) draw the game's sprite
//! materials — `materials/effects/muzzleflash1.vmt` — on camera-facing
//! quads. Some are animated: `sprites/zerogxplode` is a fireball of many
//! frames in one VTF. [`SpriteLoader`] loads such a material as a
//! [`SpriteAsset`]: its base texture's frames side by side in one image (a
//! frame is then picked by shifting the texture coordinates), and whether
//! it is drawn additively, as most effects are.
//!
//! It is the second loader of `.vmt` files; loading a path as a
//! [`SpriteAsset`] (rather than a material) chooses it.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, ReadAssetBytesError, RenderAssetUsages};
use bevy_image::{Image, ImageSampler};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::vmt::{self, VmtError};
use source_formats::vtf::{self, VtfError};
use thiserror::Error;
use wgpu_types::{Extent3d, TextureDimension, TextureFormat};

use crate::source::SOURCE;

/// Most frames of a sprite kept: far beyond the stock sprites' dozens.
const MAX_FRAMES: usize = 64;
/// Widest a strip of frames may be, pixels: what every GPU can sample.
const MAX_STRIP_WIDTH: u32 = 8192;

/// Errors from [`SpriteLoader`].
#[derive(Debug, Error)]
pub enum SpriteLoadError {
    /// The material's bytes could not be read.
    #[error("reading sprite material: {0}")]
    Read(#[from] std::io::Error),
    /// The material did not parse.
    #[error(transparent)]
    Parse(#[from] VmtError),
    /// The material names no base texture.
    #[error("sprite material has no $basetexture")]
    NoTexture,
    /// The texture could not be read.
    #[error("reading sprite texture: {0}")]
    Texture(#[from] ReadAssetBytesError),
    /// The texture did not decode.
    #[error(transparent)]
    Decode(#[from] VtfError),
}

/// A sprite: its frames side by side, and how it is drawn.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct SpriteAsset {
    /// The frames, left to right, each a full-height column.
    pub image: Handle<Image>,
    /// How many frames.
    pub frames: u32,
    /// Whether it adds its colour to what is behind (`$additive`), rather
    /// than covering it by its alpha.
    pub additive: bool,
}

/// Loads a sprite material (`.vmt`) into a [`SpriteAsset`].
#[derive(Default, TypePath)]
pub struct SpriteLoader;

impl AssetLoader for SpriteLoader {
    type Asset = SpriteAsset;
    type Settings = ();
    type Error = SpriteLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<SpriteAsset, SpriteLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let desc = vmt::parse(&text)?;
        let texture = desc.base_texture.ok_or(SpriteLoadError::NoTexture)?;
        let path = format!("{SOURCE}://materials/{}.vtf", texture.replace('\\', "/"));
        let vtf_bytes = load_context.read_asset_bytes(path).await?;
        let mut frames = vtf::decode_frames(&vtf_bytes, MAX_FRAMES)?;
        let width = frames.first().map_or(1, |frame| frame.width.max(1));
        frames.truncate((MAX_STRIP_WIDTH / width).max(1) as usize);
        let count = frames.len() as u32;
        let image = load_context.add_labeled_asset("frames".to_owned(), strip(&frames));
        Ok(SpriteAsset {
            image,
            frames: count.max(1),
            additive: is_additive(&text),
        })
    }

    fn extensions(&self) -> &[&str] {
        &["vmt"]
    }
}

/// Whether a material is drawn additively: `$additive 1`, or a `Sprite`
/// shader (whose default render mode adds).
fn is_additive(text: &str) -> bool {
    let Ok(document) = source_fs::keyvalues::parse(text) else {
        return false;
    };
    document.root.iter().any(|pair| {
        let Some(block) = pair.value.as_block() else {
            return false;
        };
        block
            .get_str("$additive")
            .is_some_and(|value| value.trim() == "1")
            || pair.key.eq_ignore_ascii_case("sprite")
    })
}

/// The frames side by side in one image (frames of differing sizes, which
/// a well-formed VTF never has, are cropped or padded to the first's).
fn strip(frames: &[vtf::DecodedTexture]) -> Image {
    let (width, height) = frames
        .first()
        .map_or((1, 1), |frame| (frame.width.max(1), frame.height.max(1)));
    let count = frames.len().max(1) as u32;
    let strip_width = width * count;
    let mut pixels = vec![0u8; (strip_width * height * 4) as usize];
    for (index, frame) in frames.iter().enumerate() {
        for y in 0..height.min(frame.height) {
            for x in 0..width.min(frame.width) {
                let from = ((y * frame.width + x) * 4) as usize;
                let to = ((y * strip_width + index as u32 * width + x) * 4) as usize;
                pixels[to..to + 4].copy_from_slice(&frame.rgba8[from..from + 4]);
            }
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: strip_width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_line_up_left_to_right() {
        let frame = |value: u8| vtf::DecodedTexture {
            width: 2,
            height: 1,
            rgba8: vec![value; 2 * 4],
            is_normal_map: false,
        };
        let image = strip(&[frame(10), frame(20), frame(30)]);
        assert_eq!((image.width(), image.height()), (6, 1));
        let data = image.data.expect("pixels");
        let firsts: Vec<u8> = data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[0])
            .collect();
        assert_eq!(firsts, [10, 10, 20, 20, 30, 30]);
    }

    #[test]
    fn additive_materials_and_sprites_add() {
        assert!(is_additive(
            r#""UnlitGeneric" { "$basetexture" "a" "$additive" "1" }"#
        ));
        assert!(is_additive(r#""Sprite" { "$basetexture" "a" }"#));
        assert!(!is_additive(r#""UnlitGeneric" { "$basetexture" "a" }"#));
    }
}
