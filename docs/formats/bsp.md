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
| `OVERLAYS` | 45 | `info_overlay` decals |

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
four light `styles`. Style `255` ends the list.

A luxel is `ColorRGBExp32`: `r, g, b: u8` and a signed exponent `e: i8`,
giving linear RGB `c × 2^e / 255`, where 1.0 is nominal full brightness.
Values above 1 are common near lights, hence the half-float atlas.

A face holds one set of samples per style, and four sets per style if its
texinfo has `SURF_BUMPLIGHT` (0x800): flat, then three bump basis directions.
So style slot `k`'s flat set starts at
`light_offset + k × (bumped ? 4 : 1) × width × height × 4`. Every face's data
is preceded by one `ColorRGBExp32` average per style, which is what the
remaining gaps between faces in the lump are.

The engine adds a face's slots every frame, each scaled by its style's
brightness. rustlife bakes the sum once (`LightStyles`), as of the map's start:

| Style | Brightness |
|-------|------------|
| 0 | 1 |
| 1–12 | mean of the stock pattern (`a` = 0, `m` = 1; `CWorld::Precache`) |
| 32 and up, owned by a `light`, `light_spot`, `light_glspot` or `light_environment` | 0 if spawn flag 1 ("initially dark"), else the mean of its `pattern`, else 1. The last entity to spawn wins |
| any other | 1 |

`vrad` gives each named, switchable light a style of 32 or more, so its pool
of light is a slot of its own. dm_steamlab has 898 such faces and dm_runoff
542. A `logic_auto`'s `OnMapSpawn` → `TurnOn`/`TurnOff` is taken as the initial
state (dm_runoff's `red_light` has a light without the flag, and relies on
that output). Later switching, such as `item_suitcharger` turning off
dm_lockdown's `megacharger_light_`, is not followed.

The luxel coordinate of a world position on a flat face is its projection onto
the face's lightmap axes (`TextureInfo::light_map_scale` for *s*,
`light_map_transform` for *t*, each `[x, y, z, offset]`), minus
`light_map_texture_min`. Sampling at `+0.5` hits luxel centres. The centroid
of a point light's hot spot on a large floor lands where this says, within
about 0.3 luxels.

### Displacements' lightmaps are not projected

A displacement's block of luxels is spread evenly over its vertex grid, and
ignores the lightmap axes, which decide only how many luxels it gets. The
stored grid is row by row: the row number counts along the base polygon's edge
from the start corner to the next corner, the column number along its edge from
the start corner to the previous corner. A vertex at `column / steps`, `row / steps` samples

    s = (column / steps) × width,  t = (row / steps) × height

in texels from the block's outer corner, so luxel (0, 0) is centred half a luxel
in from the start corner. Projecting instead (even from the undisplaced
position) lands on a rotated or mirrored copy of the light, by however the
axes happen to lie.

Two pieces of evidence support this:

- `LUMP_DISP_LIGHTMAP_SAMPLE_POSITIONS` (34), where `vrad` records each luxel's
  triangle and barycentric weights, starts every displacement at the same
  triangle for luxel (0, 0), (w−1, 0), (0, h−1) and (w−1, h−1), however its face
  is turned. A record is the triangle index (a byte; 255 means add the next
  byte too, and so on), then three weights as bytes.
- Against a native capture of dm_underpass (`mat_fullbright 2`, `mat_bumpmap
  0`), the correlation of log brightness on its two largest displacements is
  0.88 for this mapping, and at most 0.07 and −0.08 for projection. The whole
  frame's correlation with rustlife's went from 0.51 to 0.89.

A displacement's base *texture* does come from projection, of the undisplaced
position (displaced position − `vector × distance`), so steep terrain
stretches its texture.

All faces are packed into one atlas with a shelf packer. Each block gets a
one-luxel border repeating its edges, so bilinear filtering never mixes
neighbouring faces' light. Faces without a lightmap (unlit materials) point at a
reserved full-bright luxel.

## Map decals

A map's graffiti, stains and posters come in two kinds. `presentation.md` says
how they are drawn.

**`infodecal`** entities stay in the entity lump: `origin` and `texture` (a
material path). One with a `targetname` waits to be triggered, and one with
spawn flag 2048 is barred from deathmatch; no shipped map has either. It goes
on the closest surface: the engine traces ±5 units along all axes
(`CDecal::StaticDecal`), and rustlife takes the closest point within 8 units.
Its size is the texture's in texels times the material's `$decalscale`. Its
axes come from the surface's normal alone (`R_DecalComputeBasis`): on a floor or
ceiling (|normal.z| > sin 45°) *s* keeps to +X with *t* = *s* × normal; on a wall
*t* is −Z, so that it stands upright, with *s* = normal × *t*.

**Overlays** are compiled out of the entity lump into lump 45 as 352-byte
`doverlay_t` records. Lump 50 holds water overlays in the same form; no shipped
map has any.

| Offset | Field | Notes |
|--------|-------|-------|
| 0 | `id` | `i32` |
| 4 | `texinfo` | `i16`, naming the material through its texture data |
| 6 | face count, render order | `u16`: low 14 bits, high 2 bits (every shipped overlay uses order 0) |
| 8 | `faces` | 64 `i32`, face indices |
| 264 | `u` | `f32` × 2 |
| 272 | `v` | `f32` × 2 |
| 280 | `points` | 4 vectors |
| 328 | `origin` | vector |
| 340 | `normal` | vector, the basis normal |

Each point's `x` and `y` place a corner in the plane through `origin`, along
the basis tangent and bitangent. The tangent is packed in the points' `z`:
points 0 to 2 hold its components, and point 3 holds 1 if the bitangent, normal
× tangent, is flipped. The corners take texture coordinates `(u0, v0)`,
`(u0, v1)`, `(u1, v1)` and `(u1, v0)`, in that order. rustlife clips the listed
faces' triangles to the quad, seen along the normal, and reads each new vertex's
texture coordinate off the quad by inverse bilinear interpolation. Each keeps
its lightmap coordinate, so the overlay can be lit like the face under it.

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

## Collision

What players collide with is not the faces but the brushes
(`source_formats::collision`):

- **Brushes** (lump 18): `contents` flags plus a range of **brush sides**
  (lump 19). Each side is a plane index, texinfo, a `dispinfo` field and a
  `bevel` byte. The compiler adds bevel sides (axis-aligned and edge planes)
  so box sweeps clip exactly; rays must skip them. Newer compilers put a
  `thin` flag in the high byte of `bevel`, so only the low byte means bevel.
- **Leaves** list the brushes touching them (lump 17, `u16` brush indices).
  As for lighting, the leaf lump is read directly, because `vbsp` reorders
  leaves by cluster.
- **Models** (lump 14): each has a `head_node`. Model 0 is the world;
  brush entities (`model "*N"`) have their own sub-trees in the same node
  and leaf arrays. Their planes are in the entity's frame, placed by its
  `origin` and `angles`.
- **Displacements** collide as their tessellated triangles. The brush a
  displacement was built on is still in the file as an ordinary solid
  brush. Its sides' `dispinfo` is not filled in, so it is recognised
  geometrically: it has a side on the displacement's base-face plane, and
  the face's corners are the brush's corners. Source collides displacements
  only through their surface, so that brush must be made non-solid.
  Otherwise its flat face is a lid over terrain that dips below it.
- **Ladders** in HL2-era maps are `func_useableladder` entities (`point0` and
  `point1` are the feet positions at the ends), with `info_ladder_dismount`
  points nearby. They are not `CONTENTS_LADDER` brushes.
