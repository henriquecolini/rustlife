# BSP maps

Implemented in `crates/source_formats/src/bsp.rs` (with
`lightmap.rs` and `skybox.rs`), on top of the `vbsp` crate, which parses most
lumps. This page covers what rustlife reads and the parts `vbsp` does not
handle for us.

HL2DM maps are BSP version 20.

## Header and lump directory

| Offset | Field | Notes |
|--------|-------|-------|
| 0 | magic | `"VBSP"` |
| 4 | version | `i32`, 20 for HL2DM |
| 8 | lump directory | 64 entries of 16 bytes |

Each directory entry is `fileofs: i32`, `filelen: i32`, `version: i32`,
`fourCC: [u8; 4]`. rustlife reads lumps itself (`lump()`) where `vbsp` does not
expose them:

| Lump | Index | Used for |
|------|-------|----------|
| `LIGHTING` | 8 | Low-dynamic-range lightmaps |
| `PAKFILE` | 40 | Embedded zip of map-specific content |

A compressed lump starts with `"LZMA"`; HL2DM does not compress the lumps
rustlife reads, so such lumps are treated as absent.

## Faces

Faces flagged `SKY`, `SKY2D`, `NODRAW`, `TRIGGER`, `HINT` or `SKIP` are not
drawn. Each face's polygon is triangulated as a fan; displacement faces are
tessellated into a `(2^power + 1)²` grid of displaced vertices. Texture UVs are
`(dot(pos, s.xyz) + s.w) / texture_width` (and likewise for `t`).

## Lightmaps

Each lit face has `light_offset` (bytes into the lighting lump),
`light_map_texture_size` (luxels − 1 per axis), `light_map_texture_min`, and
four light `styles`. Style `255` means no lightmap in that slot; rustlife uses
style 0 only.

A luxel is `ColorRGBExp32`: `r, g, b: u8` and a signed exponent `e: i8`,
giving linear RGB `c × 2^e / 255`, where 1.0 is nominal full brightness.
Values above 1 are common near lights, hence the half-float atlas.

Bump-mapped faces store four sample sets per style (flat, then three bump
basis directions); the flat set comes first, at `light_offset`.

The luxel coordinate of a world position is its projection onto the face's
lightmap axes (`TextureInfo::light_map_scale` for *s*,
`light_map_transform` for *t*, each `[x, y, z, offset]`), minus
`light_map_texture_min`. Sampling at `+0.5` hits luxel centres. Displaced
vertices are projected the same way; their offsets lie mostly along the face
normal, which the projection drops.

All faces are packed into one atlas with a shelf packer. Each block gets a
one-luxel border repeating its edges, so bilinear filtering never mixes
neighbouring faces' light. Faces without a lightmap (unlit materials) point at a
reserved full-bright luxel.

## Displacement blending

Each displacement vertex has an `alpha` (0–255) that a `WorldVertexTransition`
material uses to blend `$basetexture` towards `$basetexture2`. rustlife reads
alphas in the exact order `vbsp` triangulates displaced vertices
(`triangulated_grid`), so the per-vertex blend matches the positions.

## Entities

The entity lump is KeyValues text. rustlife reads:

* `worldspawn` → `skyname` (the 2D skybox).
* `info_player_deathmatch`, `info_player_combine`, `info_player_rebel`,
  `info_player_start` → spawn points (`origin`, and yaw from `angles`, which is
  `"pitch yaw roll"`).

## Pakfile

Lump 40 is a plain zip (stored or deflated). Stock maps keep their per-map
cubemap *patch* materials there (`materials/maps/<map>/..._x_y_z.vmt`) as well
as custom textures, so the pakfile must be mounted above the base game while
the map is loaded.

## 2D skybox

`worldspawn`'s `skyname` names six textures,
`materials/skybox/<skyname><suffix>`, resolved through their VMTs. Source
follows Quake's naming in its Z-up frame: `rt` +X, `lf` −X, `bk` +Y, `ft` −Y,
`up` +Z, `dn` −Z. After the Source→Bevy frame change and Bevy's cubemap lookup
(which flips Z), the faces fill cubemap layers +X…−Z in the order
`rt lf up dn bk ft`. Side faces are used as-is; `up` is rotated three quarter
turns clockwise and `dn` one. Those rotations were found by scoring all
4⁶ combinations on seam continuity over three stock skies, and an ignored test
re-checks them.

## Static props

The `sprp` game lump holds a model-name dictionary, a leaf list, and one
record per prop: origin, angles (`pitch yaw roll`, degrees), dictionary index,
solidity, skin, fade distances, a lighting origin and flags. `vbsp` reads
versions 4–7, 9 and 10 (stock HL2DM maps use 6 and 7). Props flagged
`NO_DRAW` (0x4) are skipped; `USE_LIGHTING_ORIGIN` (0x2) means the stored
lighting origin replaces the model's centre as the point it is lit from.
`vbsp`'s `model()` accessor indexes the dictionary unchecked, so rustlife
indexes it itself.

Model entities (`prop_dynamic*`, `prop_physics*`, `prop_ragdoll`,
`prop_door_rotating`) come from the entity lump with `model`, `origin`,
`angles`, `skin` and `modelscale`; `rendermode 10` hides one.

Source angles compose as `R = Rz(yaw) · Ry(pitch) · Rx(roll)` (`AngleMatrix`),
with positive pitch looking down.

`vrad` also bakes per-vertex lighting for each static prop (unless flagged
`NO_PER_VERTEX_LIGHTING`, 0x40) into the pakfile as `sp_<index>.vhv`
(`sp_hdr_<index>.vhv` for HDR). The file is a 40-byte header (version 2, the
model's checksum — the same as the `.vtx` header's — vertex flags, vertex
size 4, vertex count, mesh count), 28-byte mesh headers (`lod`, vertex
count, byte offset) and BGRA colours storing `(linear / 2)^(1/2.2)`. There is
one mesh header per LOD per strip group; within it, colours follow the strip
group's vertices, each naming a model vertex as
`model.vertex_offset + mesh.vertex_offset + origMeshVertID`. Validated
against the ambient cubes: the median ratio of baked to ambient light is
about 1.1 across stock maps.

## Brush entities

Each brush entity's `model` key names a BSP model `*N`; model 0 is the
world. Model `N`'s faces are `first_face .. first_face + face_count` and are
stored relative to the entity's `origin` (and turned by its `angles`), so
they must be placed by them. Hammer writes an origin for most brush
entities, not only those with an origin brush.

## World lights

Lump 15 (HDR: 54) lists the lights `vrad` baked, which the engine reuses to
light dynamic models. Version 0 records are 88 bytes: origin, intensity,
normal (each three floats), then `cluster`, `type`, `style` (ints),
`stopdot`, `stopdot2`, `exponent`, `radius`, constant / linear / quadratic
attenuation (floats) and `flags`, `texinfo`, `owner` (ints). Version 1 adds
a 12-byte vector after the normal (100 bytes). Types: 0 surface, 1 point,
2 spot, 3 sun (`light_environment`; the normal is the light's direction),
4 Quake light, 5 sky ambient. A point or spot light arrives with
`intensity / (c + l·d + q·d² + 0.0001)`, zero beyond a non-zero `radius`; a
spot light is zero outside `stopdot2` (cosine), ramps linearly up to
`stopdot`, and is multiplied by `cos^exponent` when the exponent is neither
0 nor 1. Intensities are in the same units as the decoded ambient cubes.

## Ambient lighting

Models are lit from *ambient cubes*: six colours, one per axis direction in
the order `+X −X +Y −Y +Z −Z`, combined by `Σ nᵢ² · cube[side]` for a normal
`n`. In version 20 maps each leaf has an entry in lump 52 (HDR: 51) of
`u16 sample_count, u16 first_sample` pointing into lump 56 (HDR: 55), whose
28-byte samples are six `ColorRGBExp32` colours followed by the sample's
position as three `u8` fractions of the leaf's bounding box. Version 19 maps
instead embed one cube in each 56-byte leaf at offset 30.

Ambient colours are decoded like luxels **times 255** (the engine's
`ColorRGBExp32ToVector` carries that factor), which puts them on the same
scale as lightmaps. At a point, the engine blends the containing leaf's
samples with weights `1 / (distance² + 1)`.

`vbsp` sorts leaves by cluster after reading them, so its leaf indices no
longer match the node tree's children or the ambient index; rustlife reads the
leaf lump and walks the tree itself.
