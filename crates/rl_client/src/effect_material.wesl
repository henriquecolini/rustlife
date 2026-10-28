// The forward pass of `EffectMaterial`: Bevy's own `StandardMaterial`
// fragment shader (`bevy_pbr::render::pbr`), except for how distance fog
// meets the colour of a surface that is not alpha-blended.
//
// Bevy's fog mixes every surface's colour towards the fog's, whatever its
// alpha mode. For an alpha-blended surface that is right: what it covers is
// replaced by fog. But an additive surface then *adds* fog-coloured light,
// shining through the fog that should hide it, and a multiplying one (a
// decal) darkens what is behind it by the fog's colour. Source fogs those
// passes to the colour that leaves the framebuffer alone instead: black for
// additive ones, white for multiplying ones. Fogging towards that colour,
// with Bevy's own falloff, does the same here.
//
// Only the main forward pass is replaced: an effect is transparent, so has no
// prepass, and Bevy's defaults serve the passes it has.

import bevy_pbr::render::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    pbr_types,
};
@if(DISTANCE_FOG)
import bevy_pbr::render::{
    mesh_view_bindings::{fog, view},
    pbr_functions::apply_fog,
};

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    pbr_input.material.base_color = alpha_discard(
        pbr_input.material.flags,
        pbr_input.material.alpha_cutoff,
        pbr_input.material.base_color,
    );

    var out: FragmentOutput;
    if (pbr_input.material.flags & pbr_types::STANDARD_MATERIAL_FLAGS_UNLIT_BIT) == 0u {
        out.color = apply_pbr_lighting(pbr_input);
    } else {
        out.color = pbr_input.material.base_color;
    }

@if(DISTANCE_FOG) {
    let fog_enabled = pbr_types::STANDARD_MATERIAL_FLAGS_FOG_ENABLED_BIT;
    if (pbr_input.material.flags & fog_enabled) != 0u {
        var effect_fog = fog;
        let alpha_mode = pbr_input.material.flags
            & pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_RESERVED_BITS;
        if alpha_mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_ADD {
            // Adds nothing where fully fogged.
            effect_fog.base_color = vec4<f32>(vec3<f32>(0.0), effect_fog.base_color.a);
            effect_fog.directional_light_color.a = 0.0;
        } else if alpha_mode == pbr_types::STANDARD_MATERIAL_FLAGS_ALPHA_MODE_MULTIPLY {
            // Multiplies by one where fully fogged.
            effect_fog.base_color = vec4<f32>(vec3<f32>(1.0), effect_fog.base_color.a);
            effect_fog.directional_light_color.a = 0.0;
        }
        out.color = apply_fog(
            effect_fog,
            out.color,
            pbr_input.world_position.xyz,
            view.world_position.xyz,
            pbr_input.frag_coord.xy,
        );
        // Fogged once, here: not again by Bevy's post-processing below.
        pbr_input.material.flags &= ~fog_enabled;
    }
}

    // Tone mapping, debanding and alpha premultiplication, as Bevy does them.
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
