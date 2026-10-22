//! Decal textures: the mounted game's decal materials, corrected for Bevy.
//!
//! Source's decal shaders multiply a texture into the surface under an alpha
//! mask in gamma space. Bevy's multiply blend samples colour textures in
//! linear space and applies alpha a second time, which made the raw decal
//! textures much too dark and opaque. [`DecalLoader`] decodes the VMT/VTF
//! itself and bakes the mask into the colour: `1 - alpha * (1 - rgb)` in the
//! authored values, stored in an unorm image with opaque alpha. The result
//! is a plain multiply where transparent texels change nothing.

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

/// Errors from [`DecalLoader`].
#[derive(Debug, Error)]
pub enum DecalLoadError {
    /// The material's bytes could not be read.
    #[error("reading decal material: {0}")]
    Read(#[from] std::io::Error),
    /// The material did not parse.
    #[error(transparent)]
    Parse(#[from] VmtError),
    /// The material names no base texture.
    #[error("decal material has no $basetexture")]
    NoTexture,
    /// The texture could not be read.
    #[error("reading decal texture: {0}")]
    Texture(#[from] ReadAssetBytesError),
    /// The texture did not decode.
    #[error(transparent)]
    Decode(#[from] VtfError),
}

/// A decal's corrected texture.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct DecalAsset {
    /// The texture to multiply into the surface.
    pub image: Handle<Image>,
}

/// Loads a decal material (`.vmt`) into a [`DecalAsset`].
#[derive(Default, TypePath)]
pub struct DecalLoader;

impl AssetLoader for DecalLoader {
    type Asset = DecalAsset;
    type Settings = ();
    type Error = DecalLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<DecalAsset, DecalLoadError> {
        let mut bytes = Vec::new();
        AsyncReadExt::read_to_end(reader, &mut bytes).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let desc = vmt::parse(&text)?;
        let texture = desc.base_texture.ok_or(DecalLoadError::NoTexture)?;
        let path = format!("{SOURCE}://materials/{}.vtf", texture.replace('\\', "/"));
        let vtf_bytes = load_context.read_asset_bytes(path).await?;
        let frames = vtf::decode_frames(&vtf_bytes, 1)?;
        let frame = frames.first().ok_or(DecalLoadError::NoTexture)?;
        let mut pixels = Vec::with_capacity(frame.rgba8.len());
        for pixel in frame.rgba8.as_chunks::<4>().0 {
            let alpha = f32::from(pixel[3]) / 255.0;
            let channel = |value: u8| {
                let value = f32::from(value) / 255.0;
                ((1.0 - alpha * (1.0 - value)) * 255.0).round() as u8
            };
            pixels.extend_from_slice(&[
                channel(pixel[0]),
                channel(pixel[1]),
                channel(pixel[2]),
                255,
            ]);
        }
        let mut image = Image::new(
            Extent3d {
                width: frame.width.max(1),
                height: frame.height.max(1),
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            pixels,
            TextureFormat::Rgba8Unorm,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = ImageSampler::linear();
        let image = load_context.add_labeled_asset("texture".to_owned(), image);
        Ok(DecalAsset { image })
    }

    fn extensions(&self) -> &[&str] {
        &["vmt"]
    }
}
