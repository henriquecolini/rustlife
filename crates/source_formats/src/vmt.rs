//! Parsing Valve Material (VMT) files into an engine-independent description.
//!
//! A VMT names a shader (`LightmappedGeneric`, `VertexLitGeneric`, `Water`, ...)
//! and a set of `$parameters`. rustlife does not reimplement Source's shaders;
//! instead it reduces each material to a [`MaterialDesc`] — the shader *family*
//! ([`MaterialKind`]) plus the handful of parameters the renderer acts on — and
//! builds an appropriate Bevy material from that in the `rl_assets` layer.
//!
//! # Patches
//!
//! Many world materials are `patch` shaders: a thin file that `include`s a base
//! material and overrides a few parameters. Resolving a patch means reading
//! another VMT, which is I/O this pure crate does not do itself. [`parse`]
//! therefore leaves a patch unresolved (reporting [`MaterialKind::Patch`]),
//! while [`parse_resolved`] takes a loader callback — supplied by the
//! VFS-aware caller — that fetches the included file's text.
//!
//! # Fallback blocks
//!
//! Parameters inside DirectX level blocks (`">=dx90" { ... }`) are applied
//! before parsing, as the engine applies them on DX9-class hardware (see
//! the `vmt_fallback` module): many model materials set their base texture
//! only there.

use thiserror::Error;
use vmt_parser::VdfError;
use vmt_parser::material::Material;

use crate::vmt_fallback;

/// The family of shader a material uses, reduced to the set rustlife's renderer
/// knows how to build. Shaders outside this set render with the unlit fallback
/// ([`MaterialKind::Other`]); an unresolved `patch` is [`MaterialKind::Patch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialKind {
    /// `LightmappedGeneric`: world brushes lit by the baked lightmap.
    LightmappedGeneric,
    /// `WorldVertexTransition`: two-texture blend driven by vertex alpha, used
    /// by displacements.
    WorldVertexTransition,
    /// `VertexLitGeneric`: models and props lit per vertex / by ambient cubes.
    VertexLitGeneric,
    /// `UnlitGeneric` (and `UnlitTwoTexture`): fullbright surfaces.
    UnlitGeneric,
    /// `Water`: a translucent, fogged water surface.
    Water,
    /// `Sky`: a 2D skybox face.
    Sky,
    /// `Refract`: a refracting surface (glass, heat haze).
    Refract,
    /// A recognised shader rustlife has no special handling for yet.
    Other,
    /// A `patch` that was parsed but not resolved (see [`parse_resolved`]).
    Patch,
}

/// An engine-independent summary of a parsed material: everything the renderer
/// needs, and nothing tied to the `vmt-parser` crate's types.
///
/// Texture fields hold the material-relative path exactly as written in the
/// VMT (lower-cased by the parser), e.g. `concrete/concretefloor001a`. Turning
/// that into a mountable VFS path (`materials/.../*.vtf`) is the `rl_assets`
/// layer's job, since only it knows the virtual filesystem.
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialDesc {
    /// The shader family to build a Bevy material for.
    pub kind: MaterialKind,
    /// `$basetexture`, if any.
    pub base_texture: Option<String>,
    /// `$basetexture2`: the second texture a `WorldVertexTransition` material
    /// blends towards by vertex alpha.
    pub base_texture2: Option<String>,
    /// `$bumpmap` (normal map), if any.
    pub bump_map: Option<String>,
    /// `$surfaceprop`, used by the physics/material layer for friction and
    /// impact sounds.
    pub surface_prop: Option<String>,
    /// Whether the surface is translucent (`$translucent`, or inherent to the
    /// shader as with water).
    pub translucent: bool,
    /// The `$alphatest` reference value, if alpha testing is enabled.
    pub alpha_test: Option<f32>,
    /// Whether back-face culling is disabled (`$nocull`).
    pub no_cull: bool,
    /// A `Water` material's fog: what is seen through it and from under it.
    pub water_fog: Option<WaterFog>,
}

/// A water surface's fog: the colour water fades to with depth, and the
/// distances over which it does, units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaterFog {
    /// Linear RGB, `0..=1`.
    pub color: [f32; 3],
    /// Where the fog starts.
    pub start: f32,
    /// Where it is complete.
    pub end: f32,
}

/// Fog of water without one of its own: murky green, as Source's default.
const DEFAULT_WATER_FOG: WaterFog = WaterFog {
    color: [0.06, 0.08, 0.06],
    start: 0.0,
    end: 512.0,
};

/// Errors from parsing a VMT.
#[derive(Debug, Error)]
pub enum VmtError {
    /// The text is not valid VMT / KeyValues, or does not match any known
    /// shader schema. The message comes from the underlying parser.
    #[error("could not parse VMT: {0}")]
    Parse(String),
}

impl From<VdfError> for VmtError {
    fn from(error: VdfError) -> Self {
        Self::Parse(error.to_string())
    }
}

/// Parses VMT `text` into a [`MaterialDesc`] without resolving patches.
///
/// A `patch` material parses successfully but yields [`MaterialKind::Patch`]
/// with no textures; use [`parse_resolved`] when patches must be followed.
///
/// # Errors
///
/// Returns [`VmtError::Parse`] on malformed input.
pub fn parse(text: &str) -> Result<MaterialDesc, VmtError> {
    let mut desc = describe(&read(text)?);
    if desc.kind == MaterialKind::Water {
        desc.water_fog = Some(water_fog(&[text]));
    }
    Ok(desc)
}

/// The fog of a water material from its text (a patch's first, then its
/// base's: the patch's values win). Colours are written `{r g b}` in bytes
/// or `[r g b]` in `0..=1`, both in sRGB.
fn water_fog(texts: &[&str]) -> WaterFog {
    let mut fog = DEFAULT_WATER_FOG;
    let mut found = [false; 3];
    for text in texts {
        let Ok(document) = source_fs::keyvalues::parse(text) else {
            continue;
        };
        let Some(block) = document.root.iter().find_map(|pair| pair.value.as_block()) else {
            continue;
        };
        // A patch keeps its values under `replace` or `insert`.
        let blocks = [
            Some(block),
            block.get_block("replace"),
            block.get_block("insert"),
        ];
        for block in blocks.into_iter().flatten() {
            if !found[0]
                && let Some(color) = block.get_str("$fogcolor").and_then(parse_color)
            {
                fog.color = color;
                found[0] = true;
            }
            for (slot, key) in [(1, "$fogstart"), (2, "$fogend")] {
                if found[slot] {
                    continue;
                }
                if let Some(value) = block
                    .get_str(key)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                {
                    found[slot] = true;
                    if slot == 1 {
                        fog.start = value;
                    } else {
                        fog.end = value;
                    }
                }
            }
        }
    }
    if fog.end <= fog.start {
        fog.end = fog.start + 1.0;
    }
    fog
}

/// An sRGB colour, `{r g b}` (bytes) or `[r g b]` (fractions), as linear
/// RGB.
fn parse_color(text: &str) -> Option<[f32; 3]> {
    let text = text.trim();
    let (inner, scale) =
        if let Some(inner) = text.strip_prefix('{').and_then(|t| t.strip_suffix('}')) {
            (inner, 255.0)
        } else if let Some(inner) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            (inner, 1.0)
        } else {
            (text, 255.0)
        };
    let mut parts = inner
        .split_whitespace()
        .map(|part| part.parse::<f32>().ok());
    let mut channel = || -> Option<f32> {
        let srgb = (parts.next()?? / scale).clamp(0.0, 1.0);
        Some(if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        })
    };
    Some([channel()?, channel()?, channel()?])
}

/// Parses VMT text with its fallback blocks applied.
fn read(text: &str) -> Result<Material, VmtError> {
    Ok(vmt_parser::from_str(&vmt_fallback::apply(text))?)
}

/// Returns the `include` path of a `patch` material, or `None` for any other
/// shader. Lets a VFS-aware caller fetch the base file's bytes before calling
/// [`parse_resolved`], since this crate does no I/O of its own.
///
/// # Errors
///
/// Returns [`VmtError::Parse`] on malformed input.
pub fn patch_include(text: &str) -> Result<Option<String>, VmtError> {
    match read(text)? {
        Material::Patch(patch) => Ok(Some(patch.include)),
        _ => Ok(None),
    }
}

/// Parses VMT `text`, resolving a `patch` by loading the text of the material
/// it includes through `load_base`.
///
/// `load_base` receives the `include` path from the patch (e.g.
/// `materials/brick/brickwall.vmt`) and must return that file's contents.
///
/// # Errors
///
/// Returns [`VmtError::Parse`] if either the patch or the base fails to parse,
/// or if `load_base` reports an error.
pub fn parse_resolved<F>(text: &str, load_base: F) -> Result<MaterialDesc, VmtError>
where
    F: FnOnce(&str) -> Result<String, VmtError>,
{
    let material = read(text)?;
    let mut base_text = String::new();
    let resolved = material.resolve(|include| {
        load_base(include).map(|base| {
            let applied = vmt_fallback::apply(&base).into_owned();
            base_text.clone_from(&applied);
            applied
        })
    })?;
    let mut desc = describe(&resolved);
    if desc.kind == MaterialKind::Water {
        desc.water_fog = Some(water_fog(&[text, &base_text]));
    }
    Ok(desc)
}

/// Reduces a parsed [`Material`] to a [`MaterialDesc`].
fn describe(material: &Material) -> MaterialDesc {
    MaterialDesc {
        kind: kind_of(material),
        base_texture: base_texture(material).map(str::to_owned),
        base_texture2: match material {
            Material::WorldVertexTransition(blend) => Some(blend.base_texture2.clone()),
            _ => None,
        },
        bump_map: material.bump_map().map(str::to_owned),
        surface_prop: material.surface_prop().map(str::to_owned),
        translucent: material.translucent(),
        alpha_test: material.alpha_test(),
        no_cull: material.no_cull(),
        water_fog: None,
    }
}

/// `$basetexture` for any shader. `vmt-parser`'s own `base_texture()` returns
/// `None` for several shaders that do have one — notably `sky`, which would
/// otherwise make a skybox fall back to its compressed-HDR texture and render
/// as garbage — so those are filled in here.
fn base_texture(material: &Material) -> Option<&str> {
    material.base_texture().or(match material {
        Material::Sky(sky) => Some(&sky.base_texture),
        Material::Modulate(modulate) | Material::DecalModulate(modulate) => {
            Some(&modulate.base_texture)
        }
        Material::Cable(cable) => Some(&cable.base_texture),
        Material::Refract(refract) => refract.base_texture.as_deref(),
        Material::SpriteCard(sprite) => sprite.base_texture.as_deref(),
        _ => None,
    })
}

/// Maps a `vmt-parser` shader variant to our coarser [`MaterialKind`].
fn kind_of(material: &Material) -> MaterialKind {
    match material {
        Material::LightMappedGeneric(_) => MaterialKind::LightmappedGeneric,
        Material::WorldVertexTransition(_) => MaterialKind::WorldVertexTransition,
        Material::VertexLitGeneric(_) | Material::VertexLitGenericDx6(_) => {
            MaterialKind::VertexLitGeneric
        }
        Material::UnlitGeneric(_) | Material::UnlitTwoTexture(_) => MaterialKind::UnlitGeneric,
        Material::Water(_) => MaterialKind::Water,
        Material::Sky(_) => MaterialKind::Sky,
        Material::Refract(_) => MaterialKind::Refract,
        Material::Patch(_) => MaterialKind::Patch,
        _ => MaterialKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_reads_its_fog_and_patches_override_it() {
        let water = r#"
            "Water"
            {
                "$normalmap" "nature/water_dx70_normal"
                "$fogenable" "1"
                "$fogcolor" "{255 128 0}"
                "$fogstart" "10"
                "$fogend" "400"
            }
        "#;
        let desc = parse(water).expect("parse");
        assert_eq!(desc.kind, MaterialKind::Water);
        let fog = desc.water_fog.expect("fog");
        assert_eq!((fog.start, fog.end), (10.0, 400.0));
        assert!((fog.color[0] - 1.0).abs() < 1e-6 && fog.color[2] == 0.0);
        assert!(
            (fog.color[1] - 0.2158).abs() < 1e-3,
            "sRGB decoded: {:?}",
            fog.color
        );

        let patch = r#"
            "patch"
            {
                "include" "materials/nature/water.vmt"
                "replace" { "$fogcolor" "[0 0 1]" }
            }
        "#;
        let desc = parse_resolved(patch, |_| Ok(water.to_owned())).expect("patched");
        let fog = desc.water_fog.expect("fog");
        assert_eq!(fog.color, [0.0, 0.0, 1.0], "the patch's colour");
        assert_eq!(fog.end, 400.0, "the base's distance");
        assert!(
            parse(r#""LightmappedGeneric" { "$basetexture" "a" }"#)
                .expect("parse")
                .water_fog
                .is_none()
        );
    }

    #[test]
    fn reads_a_lightmapped_world_material() {
        let desc = parse(
            r#"
            "LightmappedGeneric"
            {
                "$basetexture" "concrete/concretefloor001a"
                "$surfaceprop" "concrete"
                "$translucent" "1"
            }
            "#,
        )
        .expect("parse");
        assert_eq!(desc.kind, MaterialKind::LightmappedGeneric);
        assert_eq!(
            desc.base_texture.as_deref(),
            Some("concrete/concretefloor001a")
        );
        assert_eq!(desc.surface_prop.as_deref(), Some("concrete"));
        assert!(desc.translucent);
        assert_eq!(desc.alpha_test, None);
    }

    #[test]
    fn reads_a_model_material_with_bumpmap_and_alphatest() {
        let desc = parse(
            r#"
            "VertexLitGeneric"
            {
                "$basetexture" "models/props/crate"
                "$bumpmap" "models/props/crate_normal"
                "$alphatest" "1"
                "$alphatestreference" "0.5"
            }
            "#,
        )
        .expect("parse");
        assert_eq!(desc.kind, MaterialKind::VertexLitGeneric);
        assert_eq!(desc.bump_map.as_deref(), Some("models/props/crate_normal"));
        assert_eq!(desc.alpha_test, Some(0.5));
    }

    #[test]
    fn reads_both_textures_of_a_blend_material() {
        let desc = parse(
            r#"
            "WorldVertexTransition"
            {
                "$basetexture" "nature/grass01"
                "$basetexture2" "nature/dirt01"
            }
            "#,
        )
        .expect("parse");
        assert_eq!(desc.kind, MaterialKind::WorldVertexTransition);
        assert_eq!(desc.base_texture.as_deref(), Some("nature/grass01"));
        assert_eq!(desc.base_texture2.as_deref(), Some("nature/dirt01"));
    }

    #[test]
    fn sky_materials_report_their_ldr_base_texture() {
        // HL2's HDR skies carry both an encoded HDR texture and a plain LDR
        // one; the base texture must be the LDR one.
        let desc = parse(
            r#"
            "sky"
            {
                "$hdrcompressedTexture" "skybox/sky_day03_04_hdrrt"
                "$basetexture" "skybox/sky_day03_04rt"
            }
            "#,
        )
        .expect("parse");
        assert_eq!(desc.kind, MaterialKind::Sky);
        assert_eq!(desc.base_texture.as_deref(), Some("skybox/sky_day03_04rt"));
    }

    #[test]
    fn modulate_and_cable_materials_report_their_base_texture() {
        for shader in ["Modulate", "DecalModulate", "Cable"] {
            let desc =
                parse(&format!(r#""{shader}" {{ "$basetexture" "decals/x" }}"#)).expect("parse");
            assert_eq!(desc.base_texture.as_deref(), Some("decals/x"), "{shader}");
        }
    }

    #[test]
    fn an_unresolved_patch_is_reported_as_such() {
        let desc = parse(
            r#"
            "patch"
            {
                "include" "materials/brick/brickwall.vmt"
                "replace" { "$basetexture" "brick/override" }
            }
            "#,
        )
        .expect("parse");
        assert_eq!(desc.kind, MaterialKind::Patch);
        assert_eq!(desc.base_texture, None);
    }

    #[test]
    fn patch_include_reports_the_base_path() {
        let patch = r#"
            "patch"
            {
                "include" "materials/brick/brickwall.vmt"
                "replace" { "$basetexture" "brick/override" }
            }
        "#;
        assert_eq!(
            patch_include(patch).unwrap().as_deref(),
            Some("materials/brick/brickwall.vmt")
        );
        // A non-patch material has no include.
        assert_eq!(
            patch_include(r#""UnlitGeneric" { "$basetexture" "x" }"#).unwrap(),
            None
        );
    }

    #[test]
    fn a_patch_is_resolved_and_overrides_the_base() {
        let base = r#"
            "LightmappedGeneric"
            {
                "$basetexture" "brick/brickwall001a"
                "$surfaceprop" "brick"
            }
        "#;
        let desc = parse_resolved(
            r#"
            "patch"
            {
                "include" "materials/brick/brickwall.vmt"
                "replace" { "$basetexture" "brick/override" }
            }
            "#,
            |include| {
                assert_eq!(include, "materials/brick/brickwall.vmt");
                Ok(base.to_owned())
            },
        )
        .expect("resolve");
        assert_eq!(desc.kind, MaterialKind::LightmappedGeneric);
        // The patch replaced the base texture but kept the base's surfaceprop.
        assert_eq!(desc.base_texture.as_deref(), Some("brick/override"));
        assert_eq!(desc.surface_prop.as_deref(), Some("brick"));
    }

    #[test]
    fn base_textures_set_in_fallback_blocks_are_found() {
        let text = r#""VertexLitGeneric"
{
	"$bumpmap" "models/x_normal"
	">=dx90_20b" { "$baseTexture" "Models/X/x_noalpha" }
	"<dx90_20b" { "$baseTexture" "Models/X/x_sheet" }
}"#;
        let material = parse(text).expect("parse");
        assert_eq!(material.kind, MaterialKind::VertexLitGeneric);
        assert_eq!(material.base_texture.as_deref(), Some("models/x/x_noalpha"));
    }

    #[test]
    fn malformed_vmt_errors_without_panicking() {
        assert!(parse("this is not a material").is_err());
        assert!(parse("").is_err());
    }
}
