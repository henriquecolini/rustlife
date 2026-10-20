//! Assembling a Source 2D skybox into a cubemap.
//!
//! A map's `skyname` (e.g. `sky_day02_09`) names six textures,
//! `materials/skybox/<skyname><suffix>.vtf`, one per cube face. Source names
//! the faces after Quake's convention in its own Z-up frame: `rt` +X, `lf` -X,
//! `bk` +Y, `ft` -Y, `up` +Z, `dn` -Z.
//!
//! GPU cubemaps instead store layers in the order +X, -X, +Y, -Y, +Z, -Z of
//! *texture* space, each with fixed pixel axes. Going from Source to Bevy's
//! world (`(x, y, z) -> (x, z, -y)`) and then to the texture space Bevy samples
//! (it flips z) maps the faces to layers `rt lf up dn bk ft`. Each face may
//! additionally need quarter turns to line its pixels up with the layer's axes;
//! [`QUARTER_TURNS`] was determined by matching the real HL2 skies' seams (see
//! [`seam_error`] and the `#[ignore]`d test that checks it).

use crate::vtf::DecodedTexture;

/// Face suffixes in cubemap layer order (+X, -X, +Y, -Y, +Z, -Z).
pub const SUFFIXES: [&str; 6] = ["rt", "lf", "up", "dn", "bk", "ft"];

/// Clockwise quarter turns applied to each face (in [`SUFFIXES`] order) so its
/// pixels follow the cubemap layer's axes.
/// Sides need none (their images are upright); the `up` face turns three
/// times and `dn` once. Determined by exhaustive seam matching over HL2 skies.
pub const QUARTER_TURNS: [u8; 6] = [0, 0, 3, 1, 0, 0];

/// Six square RGBA8 faces of equal size, concatenated in layer order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cubemap {
    /// Edge length of each face in pixels.
    pub size: usize,
    /// `6 * size * size * 4` bytes of RGBA8.
    pub rgba8: Vec<u8>,
}

/// Builds a [`Cubemap`] from the six faces given in [`SUFFIXES`] order.
///
/// Faces are resampled (nearest neighbour) to the largest face's width so a
/// sky with a smaller or non-square face still assembles. Returns `None` if
/// any face is empty.
#[must_use]
pub fn assemble(faces: [&DecodedTexture; 6]) -> Option<Cubemap> {
    assemble_with(faces, QUARTER_TURNS)
}

fn assemble_with(faces: [&DecodedTexture; 6], turns: [u8; 6]) -> Option<Cubemap> {
    let size = faces.iter().map(|face| face.width as usize).max()?;
    if size == 0 || faces.iter().any(|face| face.width == 0 || face.height == 0) {
        return None;
    }
    let mut rgba8 = Vec::with_capacity(6 * size * size * 4);
    for (face, &turn) in faces.iter().zip(&turns) {
        let square = resample(face, size);
        rgba8.extend(rotate_clockwise(&square, size, turn));
    }
    Some(Cubemap { size, rgba8 })
}

/// Nearest-neighbour resample of a face to `size` × `size`.
fn resample(face: &DecodedTexture, size: usize) -> Vec<u8> {
    let (width, height) = (face.width as usize, face.height as usize);
    if width == size && height == size {
        return face.rgba8.clone();
    }
    let mut out = vec![0; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let (sx, sy) = (x * width / size, y * height / size);
            let from = (sy * width + sx) * 4;
            out[(y * size + x) * 4..][..4].copy_from_slice(&face.rgba8[from..from + 4]);
        }
    }
    out
}

/// Rotates a square RGBA8 image clockwise by `turns` quarter turns.
fn rotate_clockwise(rgba8: &[u8], size: usize, turns: u8) -> Vec<u8> {
    let mut image = rgba8.to_vec();
    for _ in 0..turns % 4 {
        let mut rotated = vec![0; image.len()];
        // Clockwise: the source pixel (x, y) lands at (size - 1 - y, x).
        for y in 0..size {
            for x in 0..size {
                let to = (x * size + (size - 1 - y)) * 4;
                rotated[to..to + 4].copy_from_slice(&image[(y * size + x) * 4..][..4]);
            }
        }
        image = rotated;
    }
    image
}

/// Samples the cubemap (nearest) along a texture-space direction, using the
/// standard cube-face selection and per-face axes that wgpu (and so Bevy) use.
fn sample(cube: &Cubemap, direction: [f32; 3]) -> [f32; 3] {
    let [x, y, z] = direction;
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    let (layer, sc, tc, major) = if ax >= ay && ax >= az {
        if x > 0.0 {
            (0, -z, -y, ax)
        } else {
            (1, z, -y, ax)
        }
    } else if ay >= az {
        if y > 0.0 {
            (2, x, z, ay)
        } else {
            (3, x, -z, ay)
        }
    } else if z > 0.0 {
        (4, x, -y, az)
    } else {
        (5, -x, -y, az)
    };
    let to_pixel = |c: f32| {
        // Map the face coordinate from [-1, 1] to [0, 1].
        let unit = (c / major).midpoint(1.0);
        ((unit * cube.size as f32) as usize).min(cube.size - 1)
    };
    let (px, py) = (to_pixel(sc), to_pixel(tc));
    let at = ((layer * cube.size + py) * cube.size + px) * 4;
    [0, 1, 2].map(|c| f32::from(cube.rgba8[at + c]))
}

/// Mean absolute RGB difference (0–255) across all twelve cube edges, sampling
/// just inside the two faces that meet at each edge. A correctly oriented sky
/// is continuous at every seam, so this is small; a wrongly rotated face makes
/// it jump.
#[must_use]
pub fn seam_error(cube: &Cubemap) -> f32 {
    const AXES: [[f32; 3]; 6] = [
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
    ];
    const SAMPLES: usize = 64;
    // Step inside each face by about one texel.
    let inset = 2.0 / cube.size as f32;

    let (mut total, mut count) = (0.0, 0usize);
    for (a, na) in AXES.iter().enumerate() {
        for nb in &AXES[a + 1..] {
            let dot: f32 = (0..3).map(|i| na[i] * nb[i]).sum();
            if dot != 0.0 {
                continue; // same or opposite face: no shared edge
            }
            let edge = [
                na[1] * nb[2] - na[2] * nb[1],
                na[2] * nb[0] - na[0] * nb[2],
                na[0] * nb[1] - na[1] * nb[0],
            ];
            for i in 0..SAMPLES {
                let t = (i as f32 + 0.5) / SAMPLES as f32 * 1.8 - 0.9;
                let point = |out_of: &[f32; 3]| {
                    [0, 1, 2].map(|k| na[k] + nb[k] + t * edge[k] - inset * out_of[k])
                };
                let in_a = sample(cube, point(nb));
                let in_b = sample(cube, point(na));
                total += (0..3).map(|c| (in_a[c] - in_b[c]).abs()).sum::<f32>() / 3.0;
                count += 1;
            }
        }
    }
    total / count as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(size: u32, fill: impl Fn(u32, u32) -> [u8; 4]) -> DecodedTexture {
        let mut rgba8 = Vec::new();
        for y in 0..size {
            for x in 0..size {
                rgba8.extend(fill(x, y));
            }
        }
        DecodedTexture {
            width: size,
            height: size,
            rgba8,
            is_normal_map: false,
        }
    }

    #[test]
    fn rotation_moves_the_top_row_to_the_right_column() {
        // 2x2: a b / c d -> c a / d b
        let image = [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0];
        let rotated = rotate_clockwise(&image, 2, 1);
        let reds: Vec<u8> = rotated.iter().step_by(4).copied().collect();
        assert_eq!(reds, [3, 1, 4, 2]);
        assert_eq!(
            rotate_clockwise(&image, 2, 4),
            image,
            "four turns is identity"
        );
    }

    #[test]
    fn a_uniform_sky_has_no_seams() {
        let faces: Vec<DecodedTexture> = (0..6)
            .map(|_| face(8, |_, _| [90, 120, 200, 255]))
            .collect();
        let cube = assemble([
            &faces[0], &faces[1], &faces[2], &faces[3], &faces[4], &faces[5],
        ])
        .expect("assemble");
        assert_eq!(cube.rgba8.len(), 6 * 8 * 8 * 4);
        assert!(seam_error(&cube) < 1e-3);
    }

    #[test]
    fn a_vertical_gradient_is_seamless_only_when_upright() {
        // Sides darken towards the bottom; up is bright, down is dark. With the
        // sides upright the horizon seams match, rotated they do not.
        let sides = face(16, |_, y| [(255 - y * 16) as u8; 4]);
        let up = face(16, |_, _| [255; 4]);
        let down = face(16, |_, _| [15; 4]);
        let faces = [&sides, &sides, &up, &down, &sides, &sides];
        let upright = assemble_with(faces, [0; 6]).expect("assemble");
        let tipped = assemble_with(faces, [1, 0, 0, 0, 0, 0]).expect("assemble");
        assert!(seam_error(&upright) < seam_error(&tipped));
    }

    #[test]
    fn mismatched_faces_are_resampled() {
        let big = face(8, |_, _| [1, 2, 3, 4]);
        let small = face(4, |_, _| [1, 2, 3, 4]);
        let cube = assemble([&big, &small, &big, &big, &big, &big]).expect("assemble");
        assert_eq!(cube.size, 8);
        assert!(cube.rgba8.chunks(4).all(|p| p == [1, 2, 3, 4]));
    }

    /// Checks [`QUARTER_TURNS`] against a real sky. Set `RUSTLIFE_TEST_SKY` to a
    /// directory holding `<suffix>.vtf` for all six faces; prints the best
    /// rotation set found by exhaustive search for comparison.
    #[test]
    #[ignore = "requires sky VTFs; set RUSTLIFE_TEST_SKY"]
    fn real_sky_is_seamless_with_the_chosen_rotations() {
        let Ok(dir) = std::env::var("RUSTLIFE_TEST_SKY") else {
            return;
        };
        let faces: Vec<DecodedTexture> = SUFFIXES
            .iter()
            .map(|suffix| {
                let bytes = std::fs::read(format!("{dir}/{suffix}.vtf")).expect("read face");
                crate::vtf::decode(&bytes).expect("decode face")
            })
            .collect();
        let faces = [
            &faces[0], &faces[1], &faces[2], &faces[3], &faces[4], &faces[5],
        ];

        let mut results: Vec<(f32, [u8; 6])> = (0..4096u32)
            .map(|code| {
                let turns: [u8; 6] = std::array::from_fn(|i| ((code >> (2 * i)) & 3) as u8);
                (
                    seam_error(&assemble_with(faces, turns).expect("assemble")),
                    turns,
                )
            })
            .collect();
        results.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (error, turns) in &results[..4] {
            eprintln!("seam error {error:6.2} with turns {turns:?}");
        }
        let chosen = seam_error(&assemble(faces).expect("assemble"));
        eprintln!("chosen {QUARTER_TURNS:?}: seam error {chosen:.2}");
        assert!(
            chosen <= results[0].0 + 0.01,
            "chosen rotations are not seam-optimal"
        );
    }
}
