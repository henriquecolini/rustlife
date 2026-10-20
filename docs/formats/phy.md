# PHY collision models

Implemented in `crates/source_formats/src/phy.rs`. A `.phy` sits beside a
model's `.mdl` and holds the collision shape the physics engine (IVP, the
Havok predecessor inside Source's `vphysics`) simulates, plus the physical
properties the modeller compiled in. All integers are little-endian.

## File layout

| Offset | Field | Type |
|--------|-------|------|
| 0 | header size (16) | `i32` |
| 4 | id (0) | `i32` |
| 8 | solid count | `i32` |
| 12 | model checksum (matches the `.mdl`) | `u32` |

Then, per solid: an `i32` byte size followed by that many bytes of shape
data. After the last solid comes a KeyValues text section, usually
NUL-terminated.

A prop has one solid. A ragdoll has one per physics bone, each in that
bone's frame; rustlife currently simulates multi-solid models as their
bounding box.

## Solid shape data

Files from the 2006+ tools start each solid with a 28-byte `VPHY` header:

| Offset | Field | Type |
|--------|-------|------|
| 0 | `"VPHY"` | `[u8; 4]` |
| 4 | version (`0x100`) | `i16` |
| 6 | model type (0 = compact surface, 1 = MOPP) | `i16` |
| 8 | surface size | `i32` |
| 12 | drag axis areas | `[f32; 3]` |
| 24 | axis map size | `i32` |

Older files omit it and start straight with the compact surface. MOPP
surfaces are never written by the Source tools and are rejected.

### Compact surface (`IVP_Compact_Surface`, 48 bytes)

| Offset | Field | Type |
|--------|-------|------|
| 0 | mass centre | `[f32; 3]` |
| 12 | rotational inertia | `[f32; 3]` |
| 24 | upper limit radius | `f32` |
| 28 | max deviation (8 bits) · byte size (24 bits) | `u32` |
| 32 | ledge tree root, relative to the surface | `i32` |
| 36 | reserved; `"IVPS"` at 44 | `[i32; 3]` |

### Ledge tree (`IVP_Compact_Ledgetree_Node`, 28 bytes)

| Offset | Field | Type |
|--------|-------|------|
| 0 | right child, relative to this node (0 = leaf) | `i32` |
| 4 | ledge, relative to this node | `i32` |
| 8 | bounding sphere centre, radius | `[f32; 4]` |
| 24 | box sizes, padding | `[u8; 4]` |

The left child immediately follows its parent. Only **leaf** ledges are the
shape: an inner node's ledge is a coarse hull around everything below it. A
surface with root offset 0 is a single ledge right after the header.

### Ledge (`IVP_Compact_Ledge`, 16 bytes) and triangles

| Offset | Field | Type |
|--------|-------|------|
| 0 | point array, relative to this ledge | `i32` |
| 4 | tree node / client data | `i32` |
| 8 | flags (8 bits) · size / 16 (24 bits) | `u32` |
| 12 | triangle count | `i16` |
| 14 | reserved | `i16` |

`triangle count` 16-byte triangles follow. Each is a `u32` of indices and
flags, then three `u32` edges whose low 16 bits are the index of the edge's
start point. The three start points are the triangle's corners. Points are
16 bytes (`x, y, z`, padding) and shared by every ledge of the surface. Each
ledge is convex.

## Coordinates

IVP works in metres with Y pointing down. A point `(x, y, z)` is Source
`(x, z, −y) / 0.0254`. The points are in the model's frame, and the same
frame as its meshes once the root bone's transform is applied (for 97.6% of
stock props; the rest are animated models posed differently at rest).

## Text section

```text
solid {
"index" "0"
"name" "oildrum001_physbox"
"mass" "30.000000"
"surfaceprop" "metal_barrel"
"damping" "0.000000"
"rotdamping" "0.000000"
"inertia" "1.000000"
"volume" "27002.146484"
}
editparams {
"rootname" ""
"totalmass" "30.000000"
"concave" "1"
}
```

rustlife reads `mass`, `surfaceprop`, `damping`, `rotdamping` and `inertia`
per solid and `totalmass` from `editparams`. Ragdoll constraints and
collision rules are ignored. The section is best-effort: a model with an
unreadable one still collides, with default properties.

## Validation

`cargo test -p source_formats --test real_models -- --ignored` (with
`HL2DM_DIR` set) decodes all 1893 `.phy` files of HL2DM (9188 convex pieces,
no failures) and checks hull/mesh alignment.
