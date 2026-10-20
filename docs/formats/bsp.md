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

`vrad` also bakes per-vertex lighting for each static prop into the pakfile as
`sp_<index>.vhv` (`sp_hdr_<index>.vhv` for HDR). rustlife does not read these
yet; props are lit from the ambient cubes below.

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
