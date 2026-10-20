//! Source lightmaps: luxel decoding and atlas packing.
//!
//! Every lit BSP face owns a small grid of *luxels* (lightmap texels) in the
//! map's lighting lump, stored as `ColorRGBExp32`: three 8-bit channels sharing
//! a signed power-of-two exponent. This module decodes those grids to linear
//! RGB and packs all of a map's faces into one atlas texture, so the renderer
//! can light the whole world with a single lightmap image.
//!
//! Each packed block is surrounded by a one-luxel border that repeats its edge
//! luxels. Bilinear filtering samples up to half a luxel outside a face's grid
//! at its edges; the border makes those samples read the face's own lighting
//! instead of a neighbour's.

/// Linear-RGB value of one luxel.
pub type Luxel = [f32; 3];

/// Width of the padding ring around each packed block, in luxels.
const BORDER: usize = 1;

/// Decodes one `ColorRGBExp32` luxel (`r, g, b, exponent`) to linear RGB,
/// where `1.0` is the engine's nominal full brightness.
#[must_use]
pub fn decode_luxel(bytes: [u8; 4]) -> Luxel {
    let exponent = i32::from(bytes[3] as i8);
    let scale = 2f32.powi(exponent) / 255.0;
    [
        f32::from(bytes[0]) * scale,
        f32::from(bytes[1]) * scale,
        f32::from(bytes[2]) * scale,
    ]
}

/// A packed lightmap atlas in linear RGB, row-major from the top-left luxel.
#[derive(Debug, Clone, PartialEq)]
pub struct LightmapAtlas {
    /// Width in luxels.
    pub width: usize,
    /// Height in luxels.
    pub height: usize,
    /// `width * height` luxels.
    pub luxels: Vec<Luxel>,
}

/// Where a block landed in the atlas: the top-left of its *inner* luxels (the
/// border sits outside this origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Column of the block's first luxel.
    pub x: usize,
    /// Row of the block's first luxel.
    pub y: usize,
}

impl Placement {
    /// Converts a luxel-space coordinate within this block (`s`, `t`, where
    /// `0.0` is the first luxel's edge) to normalised atlas UVs, sampling at
    /// luxel centres the way Source does.
    #[must_use]
    pub fn uv(self, atlas: &LightmapAtlas, s: f32, t: f32) -> [f32; 2] {
        [
            (self.x as f32 + s + 0.5) / atlas.width as f32,
            (self.y as f32 + t + 0.5) / atlas.height as f32,
        ]
    }
}

/// A block of luxels waiting to be packed.
#[derive(Debug, Clone)]
struct Block {
    width: usize,
    height: usize,
    luxels: Vec<Luxel>,
}

/// Collects luxel blocks, then packs them into a [`LightmapAtlas`] with a
/// shelf (row) packer: blocks are sorted tallest first and laid left to right,
/// starting a new shelf when a row fills up. Simple, fast, and tight enough for
/// Source maps, whose lightmap blocks are small and similar in size.
#[derive(Debug, Default)]
pub struct AtlasBuilder {
    blocks: Vec<Block>,
}

impl AtlasBuilder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a `width` × `height` block and returns its id for
    /// [`AtlasBuilder::pack`]'s placement list.
    ///
    /// `luxels.len()` must equal `width * height`. A mismatch is a programming
    /// error: debug builds assert, release builds pad or truncate the block.
    pub fn add(&mut self, width: usize, height: usize, mut luxels: Vec<Luxel>) -> usize {
        debug_assert_eq!(luxels.len(), width * height, "block size mismatch");
        luxels.resize(width * height, [0.0; 3]);
        self.blocks.push(Block {
            width,
            height,
            luxels,
        });
        self.blocks.len() - 1
    }

    /// Packs every queued block, returning the atlas and each block's placement
    /// (indexed by the id [`AtlasBuilder::add`] returned).
    #[must_use]
    pub fn pack(self) -> (LightmapAtlas, Vec<Placement>) {
        let padded = |block: &Block| (block.width + 2 * BORDER, block.height + 2 * BORDER);
        let area: usize = self
            .blocks
            .iter()
            .map(|b| {
                let (w, h) = padded(b);
                w * h
            })
            .sum();
        let widest = self.blocks.iter().map(|b| padded(b).0).max().unwrap_or(1);
        // Aim for a roughly square atlas, as a power of two for friendliness to
        // GPUs, but always wide enough for the widest block.
        let width = ((area as f64).sqrt().ceil() as usize)
            .next_power_of_two()
            .max(widest.next_power_of_two())
            .max(4);

        let mut order: Vec<usize> = (0..self.blocks.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.blocks[i].height));

        let mut placements = vec![Placement { x: 0, y: 0 }; self.blocks.len()];
        let (mut x, mut y, mut shelf_height) = (0, 0, 0);
        for &i in &order {
            let (w, h) = padded(&self.blocks[i]);
            if x + w > width {
                x = 0;
                y += shelf_height;
                shelf_height = 0;
            }
            placements[i] = Placement {
                x: x + BORDER,
                y: y + BORDER,
            };
            x += w;
            shelf_height = shelf_height.max(h);
        }
        let height = (y + shelf_height).max(1).next_power_of_two();

        let mut atlas = LightmapAtlas {
            width,
            height,
            luxels: vec![[0.0; 3]; width * height],
        };
        for (block, placement) in self.blocks.iter().zip(&placements) {
            blit_with_border(&mut atlas, block, *placement);
        }
        (atlas, placements)
    }
}

/// Copies `block` into the atlas at `placement`, then fills the surrounding
/// border by clamping to the block's own edge luxels.
fn blit_with_border(atlas: &mut LightmapAtlas, block: &Block, placement: Placement) {
    let border = BORDER as isize;
    for row in -border..block.height as isize + border {
        for column in -border..block.width as isize + border {
            let source_row = row.clamp(0, block.height as isize - 1) as usize;
            let source_column = column.clamp(0, block.width as isize - 1) as usize;
            let value = block.luxels[source_row * block.width + source_column];
            let atlas_x = (placement.x as isize + column) as usize;
            let atlas_y = (placement.y as isize + row) as usize;
            atlas.luxels[atlas_y * atlas.width + atlas_x] = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_shared_exponent_luxels() {
        assert_eq!(decode_luxel([255, 0, 0, 0]), [1.0, 0.0, 0.0]);
        // Exponent +1 doubles, -2 quarters.
        assert_eq!(decode_luxel([255, 255, 255, 1]), [2.0, 2.0, 2.0]);
        let quarter = decode_luxel([255, 0, 0, (-2i8) as u8]);
        assert!((quarter[0] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn packed_blocks_do_not_overlap_and_keep_their_luxels() {
        let mut builder = AtlasBuilder::new();
        let ids: Vec<usize> = (0..20)
            .map(|i| {
                let (w, h) = (1 + i % 5, 1 + i % 3);
                builder.add(w, h, vec![[i as f32, 0.0, 0.0]; w * h])
            })
            .collect();
        let sizes: Vec<(usize, usize)> = (0..20).map(|i| (1 + i % 5, 1 + i % 3)).collect();
        let (atlas, placements) = builder.pack();

        // Every block's inner luxels hold its own value...
        for (&id, &(w, h)) in ids.iter().zip(&sizes) {
            let p = placements[id];
            for y in 0..h {
                for x in 0..w {
                    let luxel = atlas.luxels[(p.y + y) * atlas.width + p.x + x];
                    assert_eq!(luxel[0], id as f32, "block {id} overwritten");
                }
            }
        }
        // ...and padded rectangles, as half-open [start, end) ranges that
        // include the border, are pairwise disjoint.
        let padded = |i: usize| {
            let p = placements[i];
            let (x, y) = (p.x - BORDER, p.y - BORDER);
            (
                x,
                y,
                x + sizes[i].0 + 2 * BORDER,
                y + sizes[i].1 + 2 * BORDER,
            )
        };
        for a in 0..ids.len() {
            for b in a + 1..ids.len() {
                let (ax0, ay0, ax1, ay1) = padded(a);
                let (bx0, by0, bx1, by1) = padded(b);
                let disjoint = ax1 <= bx0 || bx1 <= ax0 || ay1 <= by0 || by1 <= ay0;
                assert!(disjoint, "blocks {a} and {b} overlap");
            }
        }
    }

    #[test]
    fn border_repeats_edge_luxels() {
        let mut builder = AtlasBuilder::new();
        let id = builder.add(2, 1, vec![[1.0, 0.0, 0.0], [2.0, 0.0, 0.0]]);
        let (atlas, placements) = builder.pack();
        let p = placements[id];
        let at = |x: usize, y: usize| atlas.luxels[y * atlas.width + x][0];
        assert_eq!(at(p.x - 1, p.y), 1.0, "left border copies first luxel");
        assert_eq!(at(p.x + 2, p.y), 2.0, "right border copies last luxel");
        assert_eq!(at(p.x, p.y - 1), 1.0, "top border copies the row");
        assert_eq!(at(p.x + 1, p.y + 1), 2.0, "bottom border copies the row");
    }

    #[test]
    fn uvs_sample_luxel_centres() {
        let mut builder = AtlasBuilder::new();
        let id = builder.add(2, 2, vec![[0.0; 3]; 4]);
        let (atlas, placements) = builder.pack();
        let [u, v] = placements[id].uv(&atlas, 0.0, 0.0);
        assert!((u * atlas.width as f32 - (placements[id].x as f32 + 0.5)).abs() < 1e-5);
        assert!((v * atlas.height as f32 - (placements[id].y as f32 + 0.5)).abs() < 1e-5);
    }
}
