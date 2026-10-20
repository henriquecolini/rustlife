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
