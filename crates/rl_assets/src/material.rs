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
pub use source_formats::vmt::WaterFog;
use source_formats::vmt::{self, MaterialDesc, VmtError};
use thiserror::Error;

use crate::source::SOURCE;

/// A resolved Source material: the textures it references and how it should be
/// drawn. The client turns this into a `StandardMaterial`.
#[derive(Asset, TypePath, Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SourceMaterial {
    /// `$basetexture` as a loaded image, if any.
    pub base_color: Option<Handle<Image>>,
    /// `$basetexture2`, which a `WorldVertexTransition` blends towards by
    /// vertex alpha; `None` for every other shader.
    pub base_color2: Option<Handle<Image>>,
    /// `$bumpmap` (normal map) as a loaded image, if any.
    pub normal: Option<Handle<Image>>,
    /// Whether the surface is translucent (`$translucent` or water).
    pub translucent: bool,
    /// `$alphatest` reference value, if alpha testing is enabled.
    pub alpha_test: Option<f32>,
    /// Whether back-face culling is disabled (`$nocull`).
    pub no_cull: bool,
    /// A water material's fog (its colour tints the surface).
    pub water_fog: Option<WaterFog>,
    /// `$selfillum`: the surface glows with its own colour, whatever the light.
    pub self_illum: bool,
    /// `$selfillumtint`, which the glow is multiplied by (white if unset).
    pub self_illum_tint: Option<[f32; 3]>,
    /// A `Sine` proxy on `$selfillumtint`: the glow pulses, as a SLAM's
    /// light blinks.
    pub self_illum_pulse: Option<SinePulse>,
    /// `$additive`: the surface adds its colour to what is behind it, lit by
    /// nothing (energy effects).
    pub additive: bool,
    /// A `TextureScroll` proxy: how fast (repeats per second) and which way
    /// (degrees) the texture slides.
    pub scroll: Option<(f32, f32)>,
}

/// A `Sine` proxy: a value that swings between `min` and `max`, once every
/// `period` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SinePulse {
    /// The least the value gets.
    pub min: f32,
    /// The most.
    pub max: f32,
    /// Seconds for a full swing.
    pub period: f32,
}

impl SinePulse {
    /// The value at `seconds`: `min` and `max` mixed by the sine of a full
    /// turn per `period`, taken from 0 to 1, as the engine's `Sine` proxy does.
    #[must_use]
    pub fn at(&self, seconds: f32) -> f32 {
        if self.period <= 0.0 {
            return self.max;
        }
        let swing = f32::midpoint((std::f32::consts::TAU * seconds / self.period).sin(), 1.0);
        self.min + (self.max - self.min) * swing
    }
}

/// The number after `key` in `text` (lower-cased), skipping quotes and
/// spaces.
fn number_after(text: &str, key: &str) -> Option<f32> {
    let at = text.find(key)? + key.len();
    text[at..]
        .trim_start_matches(|c: char| c == '"' || c.is_whitespace())
        .split(|c: char| c == '"' || c.is_whitespace())
        .next()?
        .parse()
        .ok()
}

/// `$selfillumtint` of a material's text: a colour (`[1 1 3]` or
/// `{255 255 255}`, the braces' form being 0 to 255), or one number for all
/// three channels.
fn self_illum_tint(text: &str) -> Option<[f32; 3]> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("$selfillumtint")? + "$selfillumtint".len();
    let rest = lower[at..].trim_start_matches(|c: char| c == '"' || c.is_whitespace());
    let line = rest.lines().next()?;
    let braces = line.trim_start().starts_with('{');
    let numbers: Vec<f32> = line
        .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    let scale = if braces { 1.0 / 255.0 } else { 1.0 };
    match numbers.as_slice() {
        [r, g, b, ..] => Some([r * scale, g * scale, b * scale]),
        [one] => Some([one * scale; 3]),
        _ => None,
    }
}

/// The `Sine` proxy that drives `$selfillumtint` in a material's text.
fn self_illum_pulse(text: &str) -> Option<SinePulse> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find("\"sine\"")?;
    let block = &lower[at..];
    let end = block.find('}').unwrap_or(block.len());
    let block = &block[..end];
    if !block.contains("selfillumtint") {
        return None;
    }
    Some(SinePulse {
        min: number_after(block, "sinemin").unwrap_or(0.0),
        max: number_after(block, "sinemax").unwrap_or(1.0),
        period: number_after(block, "sineperiod").unwrap_or(1.0),
    })
}

/// The `TextureScroll` proxy of a material's text: `(rate, angle in degrees)`.
fn texture_scroll(text: &str) -> Option<(f32, f32)> {
    let lower = text.to_ascii_lowercase();
    let value = |key: &str| -> Option<f32> {
        let at = lower.find(key)? + key.len();
        lower[at..]
            .trim_start_matches(|c: char| c == '"' || c.is_whitespace())
            .split(|c: char| c == '"' || c.is_whitespace())
            .next()?
            .parse()
            .ok()
    };
    let rate = value("texturescrollrate")?;
    Some((rate, value("texturescrollangle").unwrap_or(0.0)))
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
            base_color2: desc
                .base_texture2
                .as_deref()
                .map(|t| load_context.load(texture_path(t))),
            normal: desc
                .bump_map
                .as_deref()
                .map(|t| load_context.load(texture_path(t))),
            translucent: desc.translucent,
            alpha_test: desc.alpha_test,
            no_cull: desc.no_cull,
            water_fog: desc.water_fog,
            self_illum: desc.self_illum,
            self_illum_tint: self_illum_tint(&text),
            self_illum_pulse: self_illum_pulse(&text),
            scroll: texture_scroll(&text),
            additive: crate::sprite::is_additive(&text)
                && !text
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("\"sprite\""),
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

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn a_texture_scroll_proxy_is_read() {
        let text = r#""UnlitGeneric" { "Proxies" { "TextureScroll" {
            "texturescrollvar" "$basetexturetransform"
            "texturescrollrate" -1
            "texturescrollangle" 45 } } }"#;
        assert_eq!(texture_scroll(text), Some((-1.0, 45.0)));
        assert_eq!(texture_scroll(r#""UnlitGeneric" { }"#), None);
    }

    #[test]
    fn a_self_illum_tint_is_a_colour_or_a_number() {
        let tint = |value: &str| self_illum_tint(&format!("{{ \"$selfillumtint\" \"{value}\" }}"));
        assert_eq!(tint("[1 1 3]"), Some([1.0, 1.0, 3.0]));
        let braces = tint("{255 0 51}").expect("a colour");
        assert!(
            (braces[0] - 1.0).abs() < 1e-6 && braces[1] == 0.0 && (braces[2] - 0.2).abs() < 1e-6
        );
        assert_eq!(tint("2"), Some([2.0; 3]));
        assert_eq!(self_illum_tint(r#"{ "$selfillum" 1 }"#), None);
    }

    #[test]
    fn a_sine_proxy_on_the_tint_pulses_it() {
        let text = r#""VertexLitGeneric" { "$selfillum" 1 "Proxies" { "Sine" {
            "resultVar" "$selfIllumTint" "sinemin" 0.10 "sinemax" 1.00 "sineperiod" 0.30 } } }"#;
        let pulse = self_illum_pulse(text).expect("a pulse");
        assert_eq!(
            pulse,
            SinePulse {
                min: 0.1,
                max: 1.0,
                period: 0.3
            }
        );
        // Mid-swing at the start, brightest a quarter of a period in.
        assert!((pulse.at(0.0) - 0.55).abs() < 1e-5);
        assert!((pulse.at(0.075) - 1.0).abs() < 1e-5);
        assert!((pulse.at(0.225) - 0.1).abs() < 1e-5);
        // A sine on something else does not.
        assert_eq!(
            self_illum_pulse(&text.replace("selfIllumTint", "alpha")),
            None
        );
    }
}
