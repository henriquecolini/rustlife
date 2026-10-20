# VTF textures

Implemented in `crates/source_formats/src/vtf.rs` (decoding, via the `vtf`
crate) and `crates/rl_assets/src/texture.rs` (GPU upload).

## Header (7.1–7.5)

| Offset | Field | Type |
|--------|-------|------|
| 0 | signature | `"VTF\0"` |
| 4 | version | `[u32; 2]` |
| 12 | header size | `u32` |
| 16 | width, height | `u16`, `u16` |
| 20 | flags | `u32` (`0x0080` = normal map) |
| 24 | frames, first frame | `u16`, `u16` |
| 52 | high-res format | `i32` (13 = DXT1, 14 = DXT3, 15 = DXT5, …) |
| 56 | mip count | `u8` |
| 57 | low-res format | `i32` (usually DXT1) |
| 61 | low-res width, height | `u8`, `u8` |

After the header comes a small low-res thumbnail, then the high-res data. Mips
are stored **smallest first**, so the full-size image sits at the end of the
file.

## Decoding

rustlife decodes frame 0 of the full-size mip to straight RGBA8. Two gaps in
the `vtf` crate are filled here:

* It indexes the input without bounds checks and panics on truncated files.
  `decode` contains that panic and returns an error (covered by proptests).
* It only exposes the base mip, so the uploader rebuilds the chain with a 2×2
  box filter.

Formats the crate cannot decode (e.g. `UV88`, used for water refraction
maps) fail to load and leave the material untextured.

## Sampling

Source texture coordinates run far outside 0–1 (walls tile their textures),
so images use a **repeating** sampler, trilinear filtering and 8×
anisotropy. Bevy's default sampler clamps to the edge, which smears each
texture's border pixels over whole surfaces.

Colour textures are sRGB. Normal maps (the `0x0080` flag) are linear.
