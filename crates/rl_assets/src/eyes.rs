//! A character's eyes: the iris laid over the eyeball.
//!
//! The game draws an `Eyes` material with a shader that projects the iris
//! texture onto the sphere where it looks, over the eyeball texture the
//! sphere's own coordinates sample. Bevy's materials sample one texture by
//! one set of coordinates, so the same picture is made ahead of time: for
//! every texel of the eyeball's texture, which is a point on a triangle of
//! the sphere, where the iris is laid there. See
//! [`source_formats::eyes::Eyeball::iris_uv`].

use source_formats::vtf::DecodedTexture;

/// A triangle of an eye's mesh.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeTriangle {
    /// Where its corners are on the eyeball texture.
    pub uv: [[f32; 2]; 3],
    /// Where its corners are on the iris texture, for a triangle that is on
    /// the front of the eye; `None` for any corner on its back.
    pub iris: [Option<[f32; 2]>; 3],
}

/// The eyeball texture with the iris laid over it on `triangles`, as a
/// square of `pixels` on a side.
#[must_use]
pub fn bake(
    eyeball: &DecodedTexture,
    iris: &DecodedTexture,
    triangles: &[EyeTriangle],
    pixels: u32,
) -> DecodedTexture {
    let side = pixels.max(1);
    let step = 1.0 / side as f32;
    let mut rgba8 = Vec::with_capacity(side as usize * side as usize * 4);
    for y in 0..side {
        for x in 0..side {
            let uv = [(x as f32 + 0.5) * step, (y as f32 + 0.5) * step];
            rgba8.extend(sample(eyeball, uv).map(to_byte));
        }
    }
    // The texels whose centres lie within `low..=high`, and one more either
    // side for the margin.
    let texels = |low: f32, high: f32| {
        let last = side as usize - 1;
        let first = ((low * side as f32 - 0.5).floor().max(1.0) as usize - 1).min(last);
        let end = ((high * side as f32 + 0.5).ceil().max(0.0) as usize).min(last);
        first..=end
    };
    // Each texel is the iris of the triangle that holds it best: where two
    // triangles share an edge, the iris is laid once, not twice.
    let mut held: Vec<(f32, Option<[f32; 4]>)> = vec![(f32::NEG_INFINITY, None); rgba8.len() / 4];
    for triangle in triangles {
        let corners = triangle.uv;
        let low = |axis: usize| {
            corners
                .iter()
                .map(|c| c[axis])
                .fold(f32::INFINITY, f32::min)
        };
        let high = |axis: usize| {
            corners
                .iter()
                .map(|c| c[axis])
                .fold(f32::NEG_INFINITY, f32::max)
        };
        for y in texels(low(1), high(1)) {
            for x in texels(low(0), high(0)) {
                let uv = [(x as f32 + 0.5) * step, (y as f32 + 0.5) * step];
                let Some(weights) = barycentric(corners, uv) else {
                    continue;
                };
                let depth = weights.into_iter().fold(f32::INFINITY, f32::min);
                let texel = &mut held[y * side as usize + x];
                if depth > texel.0 {
                    *texel = (depth, iris_at(triangle, weights, iris));
                }
            }
        }
    }
    for (pixel, (_, laid)) in rgba8.as_chunks_mut::<4>().0.iter_mut().zip(held) {
        if let Some(laid) = laid {
            blend(&mut pixel[..], laid);
        }
    }
    DecodedTexture {
        width: side,
        height: side,
        rgba8,
        is_normal_map: false,
    }
}

/// How far outside a triangle a texel may be and still be taken as in it,
/// so the seams between triangles do not show the eyeball through.
const MARGIN: f32 = 0.02;

/// The weights of `corners` that make `point`, if it is in (or on the edge
/// of) the triangle they make.
fn barycentric(corners: [[f32; 2]; 3], point: [f32; 2]) -> Option<[f32; 3]> {
    let [a, b, c] = corners;
    let area = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
    if area.abs() < f32::EPSILON {
        return None;
    }
    let wb = ((point[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (point[1] - a[1])) / area;
    let wc = ((b[0] - a[0]) * (point[1] - a[1]) - (point[0] - a[0]) * (b[1] - a[1])) / area;
    let weights = [1.0 - wb - wc, wb, wc];
    weights.iter().all(|&w| w >= -MARGIN).then_some(weights)
}

/// The iris's colour (non-premultiplied, `0..=1`) and coverage at the point
/// of `triangle` with `weights`, if the iris is there.
fn iris_at(triangle: &EyeTriangle, weights: [f32; 3], iris: &DecodedTexture) -> Option<[f32; 4]> {
    let [a, b, c] = triangle.iris;
    let (a, b, c) = (a?, b?, c?);
    let at = [
        a[0] * weights[0] + b[0] * weights[1] + c[0] * weights[2],
        a[1] * weights[0] + b[1] * weights[1] + c[1] * weights[2],
    ];
    if !(0.0..=1.0).contains(&at[0]) || !(0.0..=1.0).contains(&at[1]) {
        return None;
    }
    let colour = sample(iris, at);
    (colour[3] > 0.0).then_some(colour)
}

/// Lays `colour` (non-premultiplied, `0..=1`) over the byte pixel `under`.
fn blend(under: &mut [u8], colour: [f32; 4]) {
    for (channel, &over) in under.iter_mut().zip(&colour).take(3) {
        let below = f32::from(*channel) / 255.0;
        *channel = to_byte(below + (over - below) * colour[3]);
    }
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The texture's colour at `uv`, bilinearly filtered and clamped at its
/// edges, as non-premultiplied `0..=1` RGBA. Colour is weighed by coverage,
/// so a transparent texel's colour does not bleed into its neighbours'.
fn sample(texture: &DecodedTexture, uv: [f32; 2]) -> [f32; 4] {
    let (width, height) = (texture.width as usize, texture.height as usize);
    let x = (uv[0] * width as f32 - 0.5).clamp(0.0, (width - 1) as f32);
    let y = (uv[1] * height as f32 - 0.5).clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let texel = |x: usize, y: usize| {
        let at = (y * width + x) * 4;
        let pixel = &texture.rgba8[at..at + 4];
        let alpha = f32::from(pixel[3]) / 255.0;
        [
            f32::from(pixel[0]) / 255.0 * alpha,
            f32::from(pixel[1]) / 255.0 * alpha,
            f32::from(pixel[2]) / 255.0 * alpha,
            alpha,
        ]
    };
    let corners = [
        (texel(x0, y0), (1.0 - fx) * (1.0 - fy)),
        (texel(x1, y0), fx * (1.0 - fy)),
        (texel(x0, y1), (1.0 - fx) * fy),
        (texel(x1, y1), fx * fy),
    ];
    let mut sum = [0.0f32; 4];
    for (texel, weight) in corners {
        for (total, value) in sum.iter_mut().zip(texel) {
            *total += value * weight;
        }
    }
    let alpha = sum[3];
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    [sum[0] / alpha, sum[1] / alpha, sum[2] / alpha, alpha]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, pixel: [u8; 4]) -> DecodedTexture {
        DecodedTexture {
            width,
            height,
            rgba8: pixel.repeat(width as usize * height as usize),
            is_normal_map: false,
        }
    }

    fn pixel(texture: &DecodedTexture, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * texture.width + x) * 4) as usize;
        texture.rgba8[at..at + 4].try_into().unwrap()
    }

    /// Two triangles that cover the whole texture, the iris laid across the
    /// whole of it too.
    fn square() -> [EyeTriangle; 2] {
        let corner = |u: f32, v: f32| (Some([u, v]), [u, v]);
        let [a, b, c, d] = [
            corner(0.0, 0.0),
            corner(1.0, 0.0),
            corner(1.0, 1.0),
            corner(0.0, 1.0),
        ];
        [
            EyeTriangle {
                uv: [a.1, b.1, c.1],
                iris: [a.0, b.0, c.0],
            },
            EyeTriangle {
                uv: [a.1, c.1, d.1],
                iris: [a.0, c.0, d.0],
            },
        ]
    }

    #[test]
    fn the_eyeball_shows_where_there_is_no_iris() {
        let eyeball = solid(4, 4, [200, 190, 180, 255]);
        let iris = solid(4, 4, [10, 20, 30, 255]);
        let triangles = [EyeTriangle {
            iris: [None; 3],
            ..square()[0]
        }];
        let baked = bake(&eyeball, &iris, &triangles, 8);
        assert_eq!((baked.width, baked.height), (8, 8));
        assert!(baked.rgba8.chunks(4).all(|p| p == [200, 190, 180, 255]));
    }

    #[test]
    fn the_iris_covers_the_eyeball_where_it_is_laid() {
        let eyeball = solid(4, 4, [200, 190, 180, 255]);
        let iris = solid(4, 4, [10, 20, 30, 255]);
        let baked = bake(&eyeball, &iris, &square(), 8);
        for (x, y) in [(0, 0), (7, 0), (3, 4), (7, 7), (0, 7)] {
            assert_eq!(pixel(&baked, x, y), [10, 20, 30, 255], "at {x} {y}");
        }
    }

    #[test]
    fn a_clear_iris_lets_the_eyeball_through_and_a_half_one_mixes() {
        let eyeball = solid(4, 4, [200, 100, 0, 255]);
        let clear = solid(4, 4, [0, 0, 0, 0]);
        let baked = bake(&eyeball, &clear, &square(), 4);
        assert!(baked.rgba8.chunks(4).all(|p| p == [200, 100, 0, 255]));
        let half = solid(4, 4, [0, 100, 200, 128]);
        let baked = bake(&eyeball, &half, &square(), 4);
        let [r, g, b, a] = pixel(&baked, 1, 1);
        assert!((99..=101).contains(&r) && (99..=101).contains(&g) && (99..=101).contains(&b));
        assert_eq!(a, 255);
    }

    #[test]
    fn the_iris_is_only_where_its_own_texture_reaches() {
        // The iris texture spans only the middle of the eye: a quarter of
        // its width on either side is outside it.
        let eyeball = solid(4, 4, [200, 190, 180, 255]);
        let iris = solid(4, 4, [10, 20, 30, 255]);
        let wide = |triangle: EyeTriangle| EyeTriangle {
            iris: triangle
                .iris
                .map(|at| at.map(|[u, v]| [u * 2.0 - 0.5, v * 2.0 - 0.5])),
            ..triangle
        };
        let triangles = square().map(wide);
        let baked = bake(&eyeball, &iris, &triangles, 16);
        assert_eq!(pixel(&baked, 8, 8), [10, 20, 30, 255], "in the middle");
        assert_eq!(pixel(&baked, 0, 0), [200, 190, 180, 255], "in the corner");
        assert_eq!(pixel(&baked, 15, 8), [200, 190, 180, 255], "at the side");
    }

    #[test]
    fn a_transparent_texel_does_not_bleed_its_colour() {
        let mut iris = solid(2, 1, [255, 0, 0, 0]);
        iris.rgba8[4..8].copy_from_slice(&[0, 0, 255, 255]);
        let [r, g, b, a] = sample(&iris, [0.5, 0.5]);
        assert!(
            r < 1e-6 && g < 1e-6 && (b - 1.0).abs() < 1e-6,
            "{r} {g} {b}"
        );
        assert!((a - 0.5).abs() < 1e-6);
    }

    #[test]
    fn points_are_found_inside_triangles_only() {
        let corners = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let inside = barycentric(corners, [0.25, 0.25]).unwrap();
        assert!(inside.iter().all(|w| (*w - 0.5).abs() <= 0.25 + 1e-6));
        assert!((inside.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(barycentric(corners, [0.9, 0.9]).is_none());
        assert!(
            barycentric([[0.0, 0.0]; 3], [0.0, 0.0]).is_none(),
            "no area"
        );
    }
}
