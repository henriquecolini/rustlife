//! Map overlays: the `info_overlay` decals `vbsp` compiles into lump 45.
//!
//! An overlay is a quad of texture laid over a list of faces. The compiler
//! removes the entity and keeps a 352-byte `doverlay_t`:
//!
//! | Offset | Field | Notes |
//! |--------|-------|-------|
//! | 0 | `id` | `i32` |
//! | 4 | `texinfo` | `i16`; its texture data names the material |
//! | 6 | face count and render order | `u16`: low 14 bits, high 2 bits |
//! | 8 | faces | 64 `i32` face indices |
//! | 264 | `u` | two `f32`: the texture's *u* at the quad's two sides |
//! | 272 | `v` | two `f32` |
//! | 280 | corners | four `Vector`s |
//! | 328 | origin | `Vector` |
//! | 340 | basis normal | `Vector` |
//!
//! Each corner's `x` and `y` are its place in the plane through the origin,
//! along the overlay's tangent and bitangent. The tangent itself is packed
//! into the corners' `z`: corner 0, 1 and 2 hold its components, and corner 3
//! holds 1 if the bitangent (normal × tangent) is flipped.

use crate::bsp::lump;

/// BSP lump index of the overlays.
const LUMP_OVERLAYS: usize = 45;
/// Size of one `doverlay_t`.
const RECORD_SIZE: usize = 352;
/// The most faces one overlay can list.
const MAX_FACES: usize = 64;
/// Bits of the face-count field that hold the count.
const FACE_COUNT_MASK: u16 = 0x3fff;

/// One overlay, Source space.
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    /// Index of its texinfo, which names the material.
    pub texinfo: usize,
    /// Which pass it is drawn in, 0 to 3. Every shipped overlay uses 0.
    pub render_order: u8,
    /// The faces it lies on.
    pub faces: Vec<usize>,
    /// The quad's corners, in winding order, on the plane through its origin.
    pub corners: [[f32; 3]; 4],
    /// Texture coordinates of the corners.
    pub uvs: [[f32; 2]; 4],
    /// The basis normal: the direction it is projected along.
    pub normal: [f32; 3],
    /// Basis tangent.
    tangent: [f32; 3],
    /// Basis bitangent.
    bitangent: [f32; 3],
}

/// A vertex being clipped: position and lightmap coordinate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayVertex {
    /// Position, Source space.
    pub position: [f32; 3],
    /// Coordinate into the lightmap atlas.
    pub lightmap_uv: [f32; 2],
    /// Coordinate into the overlay's texture; filled in once clipped.
    pub uv: [f32; 2],
}

/// Every overlay in a map's BSP `bytes`.
pub(crate) fn load(bytes: &[u8]) -> Vec<Overlay> {
    lump(bytes, LUMP_OVERLAYS).map_or_else(Vec::new, parse)
}

/// Parses lump 45. A record whose face list is too long, or a truncated one,
/// is dropped.
#[must_use]
pub fn parse(lump: &[u8]) -> Vec<Overlay> {
    lump.as_chunks::<RECORD_SIZE>()
        .0
        .iter()
        .filter_map(parse_record)
        .collect()
}

fn parse_record(record: &[u8; RECORD_SIZE]) -> Option<Overlay> {
    let field = |at: usize| -> Option<[u8; 4]> { record.get(at..at + 4)?.try_into().ok() };
    let float = |at: usize| Some(f32::from_le_bytes(field(at)?));
    let vector = |at: usize| Some([float(at)?, float(at + 4)?, float(at + 8)?]);
    let short = |at: usize| -> Option<[u8; 2]> { record.get(at..at + 2)?.try_into().ok() };

    let texinfo = usize::try_from(i16::from_le_bytes(short(4)?)).ok()?;
    let packed = u16::from_le_bytes(short(6)?);
    let count = usize::from(packed & FACE_COUNT_MASK);
    if count > MAX_FACES {
        return None;
    }
    let faces = (0..count)
        .map(|slot| usize::try_from(i32::from_le_bytes(field(8 + slot * 4)?)).ok())
        .collect::<Option<Vec<usize>>>()?;
    let (u, v) = ([float(264)?, float(268)?], [float(272)?, float(276)?]);
    let points = [vector(280)?, vector(292)?, vector(304)?, vector(316)?];
    let origin = vector(328)?;
    let normal = vector(340)?;

    let tangent = [points[0][2], points[1][2], points[2][2]];
    let flip = if points[3][2] > 0.5 { -1.0 } else { 1.0 };
    let bitangent = cross(normal, tangent).map(|component| component * flip);
    let corners = points.map(|point| {
        std::array::from_fn(|axis| {
            origin[axis] + tangent[axis] * point[0] + bitangent[axis] * point[1]
        })
    });
    Some(Overlay {
        texinfo,
        render_order: (packed >> 14) as u8,
        faces,
        corners,
        // As `COverlayMgr` lays them out.
        uvs: [[u[0], v[0]], [u[0], v[1]], [u[1], v[1]], [u[1], v[0]]],
        normal,
        tangent,
        bitangent,
    })
}

impl Overlay {
    /// The part of the triangle `vertices` under the quad, seen along the
    /// overlay's normal, as triangles with texture coordinates.
    #[must_use]
    pub fn clip(&self, vertices: [OverlayVertex; 3]) -> Vec<[OverlayVertex; 3]> {
        let quad = self.corners.map(|corner| self.in_plane(corner));
        let doubled_area: f32 = (0..4)
            .map(|i| {
                let (a, b) = (quad[i], quad[(i + 1) % 4]);
                a[0] * b[1] - b[0] * a[1]
            })
            .sum();
        let winding = doubled_area.signum();
        let mut polygon = vertices.to_vec();
        for i in 0..4 {
            let (a, b) = (quad[i], quad[(i + 1) % 4]);
            // Positive inside the edge a → b.
            let inside = |vertex: &OverlayVertex| {
                let p = self.in_plane(vertex.position);
                winding * ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]))
            };
            polygon = clip_polygon(&polygon, inside);
            if polygon.len() < 3 {
                return Vec::new();
            }
        }
        for vertex in &mut polygon {
            let [across, along] = inverse_bilinear(quad, self.in_plane(vertex.position));
            let [c0, c1, c2, c3] = self.uvs;
            vertex.uv = std::array::from_fn(|axis| {
                let near = c0[axis] + (c1[axis] - c0[axis]) * along;
                let far = c3[axis] + (c2[axis] - c3[axis]) * along;
                near + (far - near) * across
            });
        }
        (1..polygon.len() - 1)
            .map(|i| [polygon[0], polygon[i], polygon[i + 1]])
            .collect()
    }

    /// A point's coordinates along the overlay's tangent and bitangent.
    fn in_plane(&self, point: [f32; 3]) -> [f32; 2] {
        [dot(point, self.tangent), dot(point, self.bitangent)]
    }
}

/// Keeps the part of `polygon` where `inside` is not negative, interpolating
/// new vertices along the edges it cuts.
fn clip_polygon(
    polygon: &[OverlayVertex],
    inside: impl Fn(&OverlayVertex) -> f32,
) -> Vec<OverlayVertex> {
    let mut out = Vec::with_capacity(polygon.len() + 1);
    for (i, &a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        let (da, db) = (inside(&a), inside(&b));
        if da >= 0.0 {
            out.push(a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            let t = da / (da - db);
            let lerp = |x: f32, y: f32| x + (y - x) * t;
            out.push(OverlayVertex {
                position: std::array::from_fn(|axis| lerp(a.position[axis], b.position[axis])),
                lightmap_uv: std::array::from_fn(|axis| {
                    lerp(a.lightmap_uv[axis], b.lightmap_uv[axis])
                }),
                uv: [0.0; 2],
            });
        }
    }
    out
}

/// Where `point` lies in the quad, as fractions `[across, along]`: `across`
/// runs from the edge 0–1 to the edge 3–2, and `along` from the edge 0–3 to 1–2.
///
/// The quad is `point = q0 + across·side + along·(up + across·skew)`. Crossing
/// that with `up + across·skew` leaves a quadratic in `across`.
fn inverse_bilinear(quad: [[f32; 2]; 4], point: [f32; 2]) -> [f32; 2] {
    let cross2 = |a: [f32; 2], b: [f32; 2]| a[0] * b[1] - a[1] * b[0];
    let sub = |a: [f32; 2], b: [f32; 2]| [a[0] - b[0], a[1] - b[1]];
    let [start, top_start, top_end, end] = quad;
    let side = sub(end, start);
    let up = sub(top_start, start);
    let skew = sub(sub(top_end, end), up);
    let offset = sub(point, start);
    let squared = cross2(side, skew);
    let linear = cross2(side, up) - cross2(offset, skew);
    let constant = -cross2(offset, up);
    let across = if squared.abs() < 1e-5 {
        if linear.abs() < 1e-9 {
            0.0
        } else {
            -constant / linear
        }
    } else {
        let root = (linear * linear - 4.0 * squared * constant).max(0.0).sqrt();
        let first = (-linear + root) / (2.0 * squared);
        if (-1e-3..=1.0 + 1e-3).contains(&first) {
            first
        } else {
            (-linear - root) / (2.0 * squared)
        }
    };
    let direction = [up[0] + skew[0] * across, up[1] + skew[1] * across];
    let axis = usize::from(direction[0].abs() <= direction[1].abs());
    let along = (offset[axis] - side[axis] * across) / direction[axis];
    [across.clamp(0.0, 1.0), along.clamp(0.0, 1.0)]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `doverlay_t` as `vbsp` writes one.
    fn record(faces: &[i32], packed_order: u16, points: [[f32; 3]; 4]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RECORD_SIZE);
        bytes.extend_from_slice(&7i32.to_le_bytes());
        bytes.extend_from_slice(&3i16.to_le_bytes());
        let count = u16::try_from(faces.len()).expect("few faces");
        bytes.extend_from_slice(&(count | (packed_order << 14)).to_le_bytes());
        for slot in 0..MAX_FACES {
            let face = faces.get(slot).copied().unwrap_or(0);
            bytes.extend_from_slice(&face.to_le_bytes());
        }
        for value in [0.0f32, 1.0, 0.25, 0.75] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        for point in points {
            for value in point {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
        for value in [100.0f32, 200.0, 300.0, 0.0, 0.0, 1.0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(bytes.len(), RECORD_SIZE);
        bytes
    }

    /// An axis-aligned quad over `[-10, 10] × [-5, 5]`, tangent +X, normal +Z.
    const SQUARE: [[f32; 3]; 4] = [
        [-10.0, -5.0, 1.0],
        [-10.0, 5.0, 0.0],
        [10.0, 5.0, 0.0],
        [10.0, -5.0, 0.0],
    ];

    fn vertex(x: f32, y: f32) -> OverlayVertex {
        OverlayVertex {
            position: [x, y, 300.0],
            lightmap_uv: [x, y],
            uv: [0.0; 2],
        }
    }

    #[test]
    fn a_record_parses_into_a_placed_quad() {
        let overlays = parse(&record(&[12, 34], 2, SQUARE));
        let [overlay] = overlays.as_slice() else {
            panic!("one overlay: {overlays:?}");
        };
        assert_eq!(overlay.texinfo, 3);
        assert_eq!(overlay.render_order, 2);
        assert_eq!(overlay.faces, [12, 34]);
        assert_eq!(overlay.normal, [0.0, 0.0, 1.0]);
        // The bitangent is normal × tangent: +Y here.
        assert_eq!(overlay.corners[0], [90.0, 195.0, 300.0]);
        assert_eq!(overlay.corners[2], [110.0, 205.0, 300.0]);
        assert_eq!(overlay.uvs[0], [0.0, 0.25]);
        assert_eq!(overlay.uvs[1], [0.0, 0.75]);
        assert_eq!(overlay.uvs[2], [1.0, 0.75]);
        assert_eq!(overlay.uvs[3], [1.0, 0.25]);
    }

    #[test]
    fn the_last_corners_z_flips_the_bitangent() {
        let mut points = SQUARE;
        points[3][2] = 1.0;
        let overlays = parse(&record(&[1], 0, points));
        assert_eq!(overlays[0].corners[0], [90.0, 205.0, 300.0]);
    }

    #[test]
    fn a_short_lump_or_an_impossible_face_list_is_dropped() {
        assert!(parse(&[0; RECORD_SIZE - 1]).is_empty());
        let mut bad = record(&[1], 0, SQUARE);
        bad[6..8].copy_from_slice(&65u16.to_le_bytes());
        assert!(parse(&bad).is_empty());
        let mut negative = record(&[1], 0, SQUARE);
        negative[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        assert!(parse(&negative).is_empty());
    }

    #[test]
    fn a_triangle_under_the_quad_is_kept_with_its_texture_coordinates() {
        let overlay = &parse(&record(&[1], 0, SQUARE))[0];
        let triangles = overlay.clip([
            vertex(90.0, 195.0),
            vertex(110.0, 195.0),
            vertex(90.0, 205.0),
        ]);
        assert_eq!(triangles.len(), 1);
        let [a, b, c] = triangles[0];
        assert_eq!(a.uv, [0.0, 0.25], "corner 0");
        assert_eq!(b.uv, [1.0, 0.25], "corner 3");
        assert_eq!(c.uv, [0.0, 0.75], "corner 1");
    }

    #[test]
    fn a_triangle_past_the_quads_edge_is_cut_there_and_keeps_its_lightmap() {
        let overlay = &parse(&record(&[1], 0, SQUARE))[0];
        // Half of the triangle's base sticks out past x = 110.
        let triangles = overlay.clip([
            vertex(100.0, 196.0),
            vertex(120.0, 196.0),
            vertex(100.0, 204.0),
        ]);
        assert!(!triangles.is_empty());
        for vertex in triangles.iter().flatten() {
            assert!(vertex.position[0] <= 110.0 + 1e-3, "{vertex:?}");
            assert_eq!(vertex.lightmap_uv, [vertex.position[0], vertex.position[1]]);
            assert!((0.5..=1.0).contains(&vertex.uv[0]), "{vertex:?}");
        }
    }

    #[test]
    fn a_triangle_beyond_the_quad_leaves_nothing() {
        let overlay = &parse(&record(&[1], 0, SQUARE))[0];
        let far = overlay.clip([vertex(0.0, 0.0), vertex(5.0, 0.0), vertex(0.0, 5.0)]);
        assert!(far.is_empty());
    }

    #[test]
    fn inverse_bilinear_finds_places_in_a_skewed_quad() {
        let quad = [[0.0, 0.0], [1.0, 3.0], [5.0, 4.0], [4.0, 0.5]];
        let at = |a: f32, b: f32| -> [f32; 2] {
            std::array::from_fn(|axis| {
                let near = quad[0][axis] + (quad[1][axis] - quad[0][axis]) * b;
                let far = quad[3][axis] + (quad[2][axis] - quad[3][axis]) * b;
                near + (far - near) * a
            })
        };
        for (a, b) in [(0.0, 0.0), (1.0, 1.0), (0.25, 0.75), (0.6, 0.1)] {
            let [across, along] = inverse_bilinear(quad, at(a, b));
            assert!(
                (across - a).abs() < 1e-3 && (along - b).abs() < 1e-3,
                "{a},{b}"
            );
        }
    }
}
