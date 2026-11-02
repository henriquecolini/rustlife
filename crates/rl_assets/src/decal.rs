//! Decal textures: the mounted game's decal materials, corrected for Bevy.
//!
//! A decal's shader says how it is drawn ([`DecalBlend`]).
//!
//! `DecalModulate` multiplies a texture into the surface under an alpha mask
//! in gamma space. Bevy's multiply blend samples colour textures in linear
//! space and applies alpha a second time, which made the raw decal textures
//! much too dark and opaque. [`DecalLoader`] decodes the VMT/VTF itself and
//! bakes the mask into the colour: `1 + alpha * (2 * rgb - 1)` (the shader's 2x
//! modulation) in the authored values, stored in an unorm image with opaque
//! alpha. The result is a plain multiply where transparent texels change
//! nothing.
//!
//! The rest (`LightmappedGeneric` with `$decal`, most graffiti and grime) are
//! ordinary translucent textures, drawn over the surface and lit like it.

use bevy_asset::io::Reader;
use bevy_asset::{Asset, AssetLoader, Handle, LoadContext, ReadAssetBytesError, RenderAssetUsages};
use bevy_image::{Image, ImageSampler};
use bevy_reflect::TypePath;
use futures_lite::AsyncReadExt;
use source_formats::vmt::{self, MaterialDesc, MaterialKind, VmtError};
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

/// What a `DecalModulate` texel multiplies the surface by, as a byte: the
/// shader modulates the framebuffer by *twice* the texel's colour, which it
/// first blends with mid-grey (no change) by the texel's alpha:
/// `2 · lerp(0.5, rgb, alpha) = 1 + alpha · (2 · rgb - 1)`. Bevy's multiply
/// cannot brighten, so the part above 1 (the pale rims of a bullet mark) is
/// lost.
fn modulation(rgb: u8, alpha: u8) -> u8 {
    let (rgb, alpha) = (f32::from(rgb) / 255.0, f32::from(alpha) / 255.0);
    ((1.0 + alpha * (2.0 * rgb - 1.0)).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// How a decal's texture meets the surface under it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecalBlend {
    /// Multiplied into the surface: the texture is baked for that.
    Multiply,
    /// Blended over the surface by its alpha.
    Blend,
    /// Drawn where its alpha reaches this value.
    Mask(f32),
}

/// A decal's material, ready to draw.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct DecalAsset {
    /// Its texture, as [`DecalBlend`] needs it.
    pub image: Handle<Image>,
    /// How it is drawn.
    pub blend: DecalBlend,
    /// Whether it is lit by the lightmap of the surface under it.
    pub lit: bool,
    /// Its size on a surface, `[width, height]` in units: the texture's size
    /// times `$decalscale`.
    pub size: [f32; 2],
}

/// How a decal of this material is drawn, and whether the lightmap lights it.
/// A multiplying decal takes its light from the surface it darkens, and an
/// `UnlitGeneric` one is fullbright, as that shader is on any surface. Every
/// other decal is lit by the lightmap under it.
fn drawn_as(desc: &MaterialDesc) -> (DecalBlend, bool) {
    if desc.kind == MaterialKind::DecalModulate {
        return (DecalBlend::Multiply, false);
    }
    (
        desc.alpha_test.map_or(DecalBlend::Blend, DecalBlend::Mask),
        desc.kind != MaterialKind::UnlitGeneric,
    )
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
        let texture = desc
            .base_texture
            .as_ref()
            .ok_or(DecalLoadError::NoTexture)?;
        let path = format!("{SOURCE}://materials/{}.vtf", texture.replace('\\', "/"));
        let (blend, lit) = drawn_as(&desc);
        let vtf_bytes = load_context.read_asset_bytes(path.clone()).await?;
        if blend != DecalBlend::Multiply {
            // The ordinary texture loader's image: block-compressed, with mips.
            let (width, height) = vtf::dimensions(&vtf_bytes).ok_or(DecalLoadError::NoTexture)?;
            return Ok(DecalAsset {
                image: load_context.load(path),
                blend,
                lit,
                size: decal_size(width, height, &desc),
            });
        }
        let frames = vtf::decode_frames(&vtf_bytes, 1)?;
        let frame = frames.first().ok_or(DecalLoadError::NoTexture)?;
        let mut pixels = Vec::with_capacity(frame.rgba8.len());
        for pixel in frame.rgba8.as_chunks::<4>().0 {
            let channel = |value: u8| modulation(value, pixel[3]);
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
        let size = decal_size(frame.width, frame.height, &desc);
        let image = load_context.add_labeled_asset("texture".to_owned(), image);
        Ok(DecalAsset {
            image,
            blend,
            lit,
            size,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["vmt"]
    }
}

/// A decal's size on a surface, units.
fn decal_size(width: u32, height: u32, desc: &MaterialDesc) -> [f32; 2] {
    [width, height].map(|side| side as f32 * desc.decal_scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc(text: &str) -> MaterialDesc {
        vmt::parse(text).expect("parses")
    }

    #[test]
    fn a_decals_shader_chooses_how_it_is_drawn() {
        let modulate = desc(r#""DecalModulate" { "$basetexture" "a" }"#);
        assert_eq!(drawn_as(&modulate), (DecalBlend::Multiply, false));
        let graffiti =
            desc(r#""LightmappedGeneric" { "$basetexture" "a" "$decal" 1 "$translucent" 1 }"#);
        assert_eq!(drawn_as(&graffiti), (DecalBlend::Blend, true));
        let model = desc(r#""VertexLitGeneric" { "$basetexture" "a" "$translucent" 1 }"#);
        assert_eq!(drawn_as(&model), (DecalBlend::Blend, true));
        let unlit = desc(r#""UnlitGeneric" { "$basetexture" "a" "$translucent" 1 }"#);
        assert_eq!(drawn_as(&unlit), (DecalBlend::Blend, false));
        let cut = desc(r#""LightmappedGeneric" { "$basetexture" "a" "$alphatest" 1 }"#);
        assert!(matches!(drawn_as(&cut), (DecalBlend::Mask(_), true)));
    }

    #[test]
    fn a_decal_is_as_large_as_its_texture_scaled() {
        let scaled = desc("\"DecalModulate\"\n{\n\"$basetexture\" \"a\"\n\"$decalscale\" 0.25\n}");
        assert_eq!(decal_size(128, 64, &scaled), [32.0, 16.0]);
    }

    #[test]
    fn transparent_texels_change_nothing_and_mid_grey_too() {
        assert_eq!(modulation(0, 0), 255);
        assert_eq!(modulation(200, 0), 255);
        assert_eq!(modulation(128, 255), 255, "mid-grey is no change");
    }

    #[test]
    fn dark_opaque_texels_darken_by_twice_their_colour() {
        assert_eq!(modulation(0, 255), 0);
        assert_eq!(modulation(64, 255), 128);
        // Half transparent: halfway between no change and that.
        assert_eq!(modulation(0, 128), 127);
    }

    #[test]
    fn bright_texels_cannot_brighten_a_multiply() {
        assert_eq!(modulation(255, 255), 255);
    }
}
