//! The material effects are drawn with: Bevy's `StandardMaterial`, fogged
//! the way Source fogs effects.
//!
//! Bevy's distance fog mixes a surface's colour towards the fog's whatever
//! its alpha mode. That suits alpha-blended surfaces, but an additive one
//! (a muzzle flash, a glow sprite, an explosion's fireball) then adds
//! fog-coloured light and glows through the fog that should hide it, and a
//! multiplying one (a bullet hole) darkens what is behind it by the fog's
//! colour. Source fogs those passes to the colour that changes nothing
//! instead: black for additive ones, white for multiplying ones.
//! [`EffectMaterial`] is a `StandardMaterial` drawn by a fragment shader
//! that does that (`effect_material.wesl`); everything else about it is
//! Bevy's.
//!
//! | alpha mode | fogged colour, at fog fraction `f` |
//! |------------|------------------------------------|
//! | `Blend` and the rest | `mix(colour, fog, f)` (Bevy's own) |
//! | `Add` | `mix(colour, black, f)` = `colour × (1 − f)` |
//! | `Multiply` | `mix(colour, white, f)` |

use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;

/// A `StandardMaterial` whose additive and multiplying passes fade out in
/// fog rather than taking on its colour (see the [module docs](self)).
pub type EffectMaterial = ExtendedMaterial<StandardMaterial, FogBlend>;

/// The extension behind [`EffectMaterial`]: no data of its own, only its
/// fragment shader, which picks the fog's target colour by alpha mode.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone, Default)]
pub struct FogBlend {}

/// Where the fragment shader is embedded.
const SHADER: &str = "embedded://rl_client/effect_material.wesl";

impl MaterialExtension for FogBlend {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
}

/// `base`, drawn as an effect: fogged as Source fogs its blend mode.
#[must_use]
pub fn effect_material(base: StandardMaterial) -> EffectMaterial {
    ExtendedMaterial {
        base,
        extension: FogBlend {},
    }
}

/// Registers [`EffectMaterial`] and embeds its shader.
pub struct EffectMaterialPlugin;

impl Plugin for EffectMaterialPlugin {
    fn build(&self, app: &mut App) {
        bevy::asset::embedded_asset!(app, "effect_material.wesl");
        app.add_plugins(MaterialPlugin::<EffectMaterial>::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colour the framebuffer ends up with when a surface of `color`
    /// and opacity `alpha`, fogged by fraction `f` towards `fog`, is drawn
    /// over `behind`: what the shader and Bevy's blend states do, on the
    /// CPU.
    fn drawn(mode: AlphaMode, color: f32, alpha: f32, fog: f32, f: f32, behind: f32) -> f32 {
        let target = match mode {
            AlphaMode::Add => 0.0,
            AlphaMode::Multiply => 1.0,
            _ => fog,
        };
        let fogged = color + (target - color) * f;
        match mode {
            AlphaMode::Add => behind + fogged * alpha,
            AlphaMode::Multiply => behind * (fogged * alpha) + (1.0 - alpha) * behind,
            _ => fogged * alpha + behind * (1.0 - alpha),
        }
    }

    #[test]
    fn fully_fogged_effects_leave_what_is_behind_them_alone() {
        let behind = 0.3;
        for mode in [AlphaMode::Add, AlphaMode::Multiply] {
            let seen = drawn(mode, 0.8, 0.7, 0.9, 1.0, behind);
            assert!((seen - behind).abs() < 1e-6, "{mode:?}: {seen}");
        }
    }

    #[test]
    fn an_unfogged_effect_is_drawn_as_it_is() {
        assert!((drawn(AlphaMode::Add, 0.5, 1.0, 0.9, 0.0, 0.25) - 0.75).abs() < 1e-6);
        assert!((drawn(AlphaMode::Multiply, 0.5, 1.0, 0.9, 0.0, 0.5) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn fog_fades_an_additive_effect_in_proportion() {
        // A quarter of the way into the fog, three quarters of its light.
        let added = drawn(AlphaMode::Add, 0.8, 1.0, 0.9, 0.25, 0.0);
        assert!((added - 0.6).abs() < 1e-6);
    }

    #[test]
    fn blended_surfaces_fog_as_bevy_fogs_them() {
        // Fully fogged and opaque: the fog's colour.
        assert!((drawn(AlphaMode::Blend, 0.2, 1.0, 0.9, 1.0, 0.0) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn effect_materials_keep_their_standard_material() {
        let material = effect_material(StandardMaterial {
            alpha_mode: AlphaMode::Add,
            unlit: true,
            ..default()
        });
        assert_eq!(material.base.alpha_mode, AlphaMode::Add);
        assert!(material.base.unlit);
        assert!(matches!(
            <FogBlend as MaterialExtension>::fragment_shader(),
            ShaderRef::Path(path) if path.to_string() == SHADER
        ));
    }

    #[test]
    fn the_shader_parses_and_fogs_both_passes() {
        let source = include_str!("effect_material.wesl");
        if let Err(error) = source.parse::<wesl::syntax::TranslationUnit>() {
            panic!("effect_material.wesl: {error}");
        }
        for mode in ["ALPHA_MODE_ADD", "ALPHA_MODE_MULTIPLY"] {
            assert!(source.contains(mode), "{mode}");
        }
    }
}
