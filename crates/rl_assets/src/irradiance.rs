//! A map's ambient cubes as a Bevy irradiance volume.
//!
//! Bevy's irradiance volumes use the same ambient-cube representation as
//! Source (six colours per voxel, blended by the squared normal), so a map's
//! baked ambient light ([`source_formats::ambient`]) can light every model in
//! it through Bevy's standard PBR pipeline, interpolated in hardware.
//!
//! Bevy expects a voxel grid of resolution `(Rx, Ry, Rz)` packed into a 3D
//! texture of `(Rx, 2·Ry, 3·Rz)` texels: the `-X/+X` sides stacked in the
//! first `Rz` slices (negative sides in the first `Ry` rows), then `-Y/+Y`,
//! then `-Z/+Z`. The volume spans a unit cube that the light-probe entity's
//! transform scales over the map; voxel `i` of `R` sits at
//! `(i + 0.5) / R - 0.5` in that cube.

use bevy_asset::RenderAssetUsages;
use bevy_image::{Image, ImageSampler};
use bevy_math::Vec3;
use half::f16;
use source_formats::ambient::{AmbientGrid, AmbientLighting};
use wgpu_types::{Extent3d, TextureDimension, TextureFormat};

use crate::coords;

/// Smallest voxel spacing, in Source units: about a player's width, finer
/// than which the leaf samples carry no extra detail.
const MIN_SPACING: f32 = 48.0;
/// Most voxels a volume may have. At 48 bytes a voxel (six `Rgba16Float`
/// colours) this caps the texture at 24 MiB.
const MAX_VOXELS: usize = 1 << 19;
/// Growth factor applied to the spacing until the grid fits [`MAX_VOXELS`].
const SPACING_STEP: f32 = 1.25;

/// For each Bevy axis, the Source ambient-cube sides `[negative, positive]`:
/// Bevy ±X is Source ±X (sides 1, 0), Bevy ±Y is Source ±Z (5, 4), and Bevy
/// ±Z is Source ∓Y (2, 3).
const SIDES: [[usize; 2]; 3] = [[1, 0], [5, 4], [2, 3]];

/// Where an irradiance volume sits in the world: the light-probe transform's
/// translation and scale, Bevy space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumePlacement {
    /// Centre of the volume.
    pub center: Vec3,
    /// Full extent along each Bevy axis.
    pub size: Vec3,
}

/// Bakes `ambient` into an irradiance-volume image, or `None` if the map has
/// no usable ambient light.
#[must_use]
pub fn bake(ambient: &AmbientLighting) -> Option<(Image, VolumePlacement)> {
    let grid = ambient.grid(spacing_for(ambient.bounds()))?;
    Some((volume_image(&grid), placement(&grid)))
}

/// The smallest spacing (from [`MIN_SPACING`], growing by [`SPACING_STEP`])
/// whose grid over `bounds` fits in [`MAX_VOXELS`].
fn spacing_for((min, max): ([f32; 3], [f32; 3])) -> f32 {
    let voxels = |spacing: f32| -> usize {
        (0..3)
            .map(|axis| ((max[axis] - min[axis]).max(0.0) / spacing).ceil() as usize + 1)
            .product()
    };
    let mut spacing = MIN_SPACING;
    while voxels(spacing) > MAX_VOXELS {
        spacing *= SPACING_STEP;
    }
    spacing
}

/// The light-probe placement covering `grid`: each voxel is a cell of
/// `spacing` centred on its sample point.
fn placement(grid: &AmbientGrid) -> VolumePlacement {
    let [sx, sy, sz] = grid.size.map(|n| n as f32 * grid.spacing);
    let center =
        [0, 1, 2].map(|axis| grid.origin[axis] + (grid.size[axis] - 1) as f32 * grid.spacing / 2.0);
    VolumePlacement {
        center: coords::position(center),
        // Bevy X, Y, Z run along Source X, Z and (−)Y.
        size: Vec3::new(sx, sz, sy) * coords::METERS_PER_UNIT,
    }
}

/// Packs `grid` into Bevy's irradiance-volume texture layout.
fn volume_image(grid: &AmbientGrid) -> Image {
    // Bevy grid resolution: X along Source X, Y along Source Z (up), Z along
    // Source −Y.
    let [rx, ry, rz] = [grid.size[0], grid.size[2], grid.size[1]];
    let one = f16::from_f32(1.0).to_le_bytes();
    let mut data = vec![0u8; rx * 2 * ry * 3 * rz * 8];
    for (axis, sides) in SIDES.iter().enumerate() {
        for (positive, &side) in sides.iter().enumerate() {
            for bz in 0..rz {
                for by in 0..ry {
                    for bx in 0..rx {
                        let cube = &grid.cubes[grid.index(bx, rz - 1 - bz, by)];
                        let row = by + positive * ry;
                        let slice = bz + axis * rz;
                        let texel = (slice * 2 * ry + row) * rx + bx;
                        let [red, green, blue] =
                            cube[side].map(|channel| f16::from_f32(channel).to_le_bytes());
                        data[texel * 8..texel * 8 + 8].copy_from_slice(&[
                            red[0], red[1], green[0], green[1], blue[0], blue[1], one[0], one[1],
                        ]);
                    }
                }
            }
        }
    }

    let mut image = Image::new(
        Extent3d {
            width: rx as u32,
            height: (2 * ry) as u32,
            depth_or_array_layers: (3 * rz) as u32,
        },
        TextureDimension::D3,
        data,
        TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::linear();
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_formats::ambient::AmbientCube;

    /// A 2×3×4 grid whose voxel `(x, y, z)` has side `s` brightness
    /// `100·s + 10·x + y + z / 10` in its red channel, so every texel's origin
    /// is recoverable.
    fn labelled_grid() -> AmbientGrid {
        let size = [2, 3, 4];
        let mut cubes = Vec::new();
        for z in 0..size[2] {
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let mut cube = AmbientCube::default();
                    for (side, color) in cube.iter_mut().enumerate() {
                        color[0] =
                            100.0 * side as f32 + 10.0 * x as f32 + y as f32 + z as f32 / 10.0;
                    }
                    cubes.push(cube);
                }
            }
        }
        AmbientGrid {
            origin: [0.0; 3],
            spacing: 10.0,
            size,
            cubes,
        }
    }

    /// Red channel of the texel at `column`, `row`, `slice`.
    fn red(image: &Image, column: usize, row: usize, slice: usize) -> f32 {
        let size = image.texture_descriptor.size;
        let (width, height) = (size.width as usize, size.height as usize);
        let at = ((slice * height + row) * width + column) * 8;
        let data = image.data.as_ref().expect("CPU data");
        f16::from_le_bytes([data[at], data[at + 1]]).to_f32()
    }

    #[test]
    fn packs_sides_and_axes_in_bevy_layout() {
        let grid = labelled_grid();
        let image = volume_image(&grid);
        // Bevy resolution is (Source X, Source Z, Source Y) = (2, 4, 3).
        let size = image.texture_descriptor.size;
        assert_eq!(
            (size.width, size.height, size.depth_or_array_layers),
            (2, 8, 9)
        );

        // Bevy voxel (1, 2, 0): Source x = 1, z = 2, y = 3 - 1 - 0 = 2.
        let label = |side: f32| 100.0 * side + 10.0 + 2.0 + 0.2;
        let close = |a: f32, b: f32| (a - f16::from_f32(b).to_f32()).abs() < 1e-6;
        // -X (Source side 1) in the negative rows of the first slab, +X (0) in
        // the positive rows.
        assert!(close(red(&image, 1, 2, 0), label(1.0)));
        assert!(close(red(&image, 1, 2 + 4, 0), label(0.0)));
        // -Y / +Y: Source -Z (5) / +Z (4), second slab.
        assert!(close(red(&image, 1, 2, 3), label(5.0)));
        assert!(close(red(&image, 1, 2 + 4, 3), label(4.0)));
        // -Z / +Z: Source +Y (2) / -Y (3), third slab.
        assert!(close(red(&image, 1, 2, 6), label(2.0)));
        assert!(close(red(&image, 1, 2 + 4, 6), label(3.0)));
    }

    #[test]
    fn placement_covers_every_voxel_cell() {
        let grid = labelled_grid();
        let placed = placement(&grid);
        let m = coords::METERS_PER_UNIT;
        // Source extents 20 × 30 × 40 → Bevy (X, Y, Z) = (20, 40, 30).
        assert!((placed.size - Vec3::new(20.0, 40.0, 30.0) * m).length() < 1e-5);
        // Sample centres span 0..10 in X, 0..20 in Y and 0..30 in Z.
        assert!((placed.center - coords::position([5.0, 10.0, 15.0])).length() < 1e-5);
    }

    #[test]
    fn spacing_grows_until_the_grid_fits() {
        assert_eq!(spacing_for(([0.0; 3], [100.0; 3])), MIN_SPACING);
        let huge = ([-16384.0; 3], [16384.0; 3]);
        let spacing = spacing_for(huge);
        let per_axis = (32768.0 / spacing).ceil() as usize + 1;
        assert!(per_axis.pow(3) <= MAX_VOXELS);
        assert!(spacing > MIN_SPACING);
    }
}
