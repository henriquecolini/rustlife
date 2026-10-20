# VMT materials

Implemented in `crates/source_formats/src/vmt.rs` (parsing, via the
`vmt-parser` crate) and `crates/rl_assets/src/material.rs` (loading).

A VMT is KeyValues text: a shader name and a block of `$parameters`, e.g.

```text
"LightmappedGeneric"
{
	"$basetexture" "concrete/concretefloor001a"
	"$surfaceprop" "concrete"
}
```

Texture paths are relative to `materials/`, without extension, and may use
backslashes. Keys are case-insensitive.

## What rustlife reads

rustlife does not reimplement Source's shaders. Each material is reduced to a
`MaterialDesc`: the shader family plus the parameters the renderer acts on.

| Parameter | Effect |
|-----------|--------|
| `$basetexture` | Base colour |
| `$basetexture2` | Second texture of `WorldVertexTransition`, blended by vertex alpha |
| `$bumpmap` | Normal map |
| `$translucent` | Alpha blending |
| `$alphatest`, `$alphatestreference` | Alpha mask with cutoff |
| `$nocull` | Recorded; all world surfaces currently draw double-sided |
| `$surfaceprop` | Recorded for physics and impact sounds (M4) |

## Patch materials

A `patch` material includes another and overrides some of its parameters:

```text
"patch"
{
	"include" "materials/brick/brickwall001a.vmt"
	"replace" { "$envmapcubemap" "maps/dm_lockdown/c-3612_4604_137" }
}
```

The `include` path is rooted at the game directory. Stock maps use patches
for nearly every world material, to attach per-map cubemaps, and keep them in
the map's pakfile. rustlife resolves one level of patching: the loader reads the
included file through the asset system (so it comes from the same mounted
content) and applies the overrides.
