//! The map's light sources (`worldlights` lump), as the engine uses them to
//! light models.
//!
//! `vrad` writes every light entity it baked into lump 15 (HDR: 54) as a
//! `dworldlight_t`: origin, linear intensity, direction, type, spot cone,
//! cutoff radius and attenuation coefficients. Lightmaps and static-prop
//! lighting already contain these lights; the engine reads them again at run
//! time to light *dynamic* models, adding each light that can see the model to
//! the model's ambient cube. Intensities are in the same units as the ambient
//! cubes, so the two simply add.

use crate::ambient::AmbientCube;
use crate::bsp::{lump, lump_version};

/// World lights lumps, LDR then HDR.
const LUMP_WORLD_LIGHTS: [usize; 2] = [15, 54];
/// Record size of lump version 0 (Source 2004–2007 maps).
const RECORD_SIZE_V0: usize = 88;
/// Record size of lump version 1, which inserts a 12-byte shadow-cast offset
/// after the normal.
const RECORD_SIZE_V1: usize = 100;
/// Guards the attenuation division, as the engine does.
const ATTENUATION_EPSILON: f32 = 0.0001;

/// The ambient-cube axis directions, in side order `+X −X +Y −Y +Z −Z`.
const CUBE_AXES: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
];

/// The shape of a light's emission.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LightKind {
    /// Emits equally in all directions (`light`).
    Point,
    /// A cone (`light_spot`): full intensity inside `inner_cos`, fading to
    /// nothing at `outer_cos` (cosines of the half-angles), shaped by
    /// `exponent`.
    Spot {
        /// Direction the light points in, unit length.
        direction: [f32; 3],
        /// Cosine of the inner (full-intensity) half-angle.
        inner_cos: f32,
        /// Cosine of the outer (zero-intensity) half-angle.
        outer_cos: f32,
        /// Falloff exponent inside the cone.
        exponent: f32,
    },
    /// The sun (`light_environment`): parallel light along `direction`,
    /// reaching only points that can see the sky.
    Sun {
        /// Direction the light travels in, unit length.
        direction: [f32; 3],
    },
}

/// One light source, Source space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldLight {
    /// Emission shape.
    pub kind: LightKind,
    /// Position of point and spot lights.
    pub origin: [f32; 3],
    /// Linear colour times brightness, in ambient-cube units at unit falloff.
    pub intensity: [f32; 3],
    /// Distance beyond which the light has no effect; `0` for unlimited.
    pub radius: f32,
    /// Constant, linear and quadratic attenuation:
    /// `1 / (c + l·d + q·d²)`.
    pub attenuation: [f32; 3],
}

impl WorldLight {
    /// The light this source sends to `point`, ignoring occlusion: its colour
    /// there and the unit direction from the point towards the light, or
    /// `None` if it does not reach the point.
    #[must_use]
    pub fn arriving_at(&self, point: [f32; 3]) -> Option<([f32; 3], [f32; 3])> {
        let (scale, towards) = match self.kind {
            LightKind::Sun { direction } => (1.0, direction.map(|c| -c)),
            LightKind::Point | LightKind::Spot { .. } => {
                let delta = sub(self.origin, point);
                let distance_sq = dot(delta, delta);
                if self.radius > 0.0 && distance_sq > self.radius * self.radius {
                    return None;
                }
                let distance = distance_sq.sqrt();
                if distance <= f32::EPSILON {
                    return None;
                }
                let [c, l, q] = self.attenuation;
                let falloff = 1.0 / (c + l * distance + q * distance_sq + ATTENUATION_EPSILON);
                let towards = delta.map(|v| v / distance);
                (falloff * self.spot_factor(towards), towards)
            }
        };
        (scale > 0.0).then(|| (self.intensity.map(|c| c * scale), towards))
    }

    /// The cone factor of a spot light for a point in direction `towards`
    /// (from the point to the light); `1` for other kinds. Mirrors the
    /// engine: zero outside the outer cone, a linear ramp across the
    /// penumbra, times `cos^exponent` unless the exponent is 0 or 1.
    fn spot_factor(&self, towards: [f32; 3]) -> f32 {
        let LightKind::Spot {
            direction,
            inner_cos,
            outer_cos,
            exponent,
        } = self.kind
        else {
            return 1.0;
        };
        let cos = -dot(direction, towards);
        if cos <= outer_cos {
            return 0.0;
        }
        let mut factor = 1.0;
        if cos < inner_cos {
            factor = (cos - outer_cos) / (inner_cos - outer_cos);
        }
        if exponent > 0.0 && (exponent - 1.0).abs() > f32::EPSILON {
            factor *= cos.powf(exponent);
        }
        factor
    }
}

/// Adds light of `color` arriving from unit direction `towards` to `cube`,
/// as the engine folds lights into a model's ambient cube: each side
/// receives the light in proportion to how directly it faces it.
pub fn add_to_cube(cube: &mut AmbientCube, color: [f32; 3], towards: [f32; 3]) {
    for (side, axis) in cube.iter_mut().zip(CUBE_AXES) {
        let facing = dot(axis, towards);
        if facing > 0.0 {
            for (channel, light) in side.iter_mut().zip(color) {
                *channel += light * facing;
            }
        }
    }
}

/// Reads the map's world lights (LDR lump, falling back to HDR). Unknown
/// lump versions and light types the engine does not use for models
/// (surface lights, sky ambient, Quake lights) are skipped.
pub(crate) fn load(bytes: &[u8]) -> Vec<WorldLight> {
    LUMP_WORLD_LIGHTS
        .into_iter()
        .find_map(|index| {
            let data = lump(bytes, index).filter(|data| !data.is_empty())?;
            let size = match lump_version(bytes, index)? {
                0 => RECORD_SIZE_V0,
                1 => RECORD_SIZE_V1,
                _ => return None,
            };
            Some(data.chunks_exact(size).filter_map(parse).collect())
        })
        .unwrap_or_default()
}

/// Parses one `dworldlight_t`. Version 1 records carry an extra vector after
/// the normal; the fields after it are read from the record's end so both
/// layouts share this code.
fn parse(record: &[u8]) -> Option<WorldLight> {
    let word =
        |at: usize| -> [u8; 4] { [record[at], record[at + 1], record[at + 2], record[at + 3]] };
    let float = |at: usize| f32::from_le_bytes(word(at));
    let vector = |at: usize| [float(at), float(at + 4), float(at + 8)];
    // The 13 words from `cluster` to `owner` end every record.
    let tail = record.len() - 13 * 4;
    let kind_code = i32::from_le_bytes(word(tail + 4));
    let normal = vector(24);
    let kind = match kind_code {
        1 => LightKind::Point,
        2 => LightKind::Spot {
            direction: normal,
            inner_cos: float(tail + 12),
            outer_cos: float(tail + 16),
            exponent: float(tail + 20),
        },
        3 => LightKind::Sun { direction: normal },
        _ => return None,
    };
    Some(WorldLight {
        kind,
        origin: vector(0),
        intensity: vector(12),
        radius: float(tail + 24),
        attenuation: [float(tail + 28), float(tail + 32), float(tail + 36)],
    })
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(origin: [f32; 3]) -> WorldLight {
        WorldLight {
            kind: LightKind::Point,
            origin,
            intensity: [100.0; 3],
            radius: 0.0,
            attenuation: [0.0, 0.0, 1.0],
        }
    }

    /// A version-0 record of `kind_code` with the given fields.
    fn record(kind_code: i32, normal: [f32; 3], cone: [f32; 3], attenuation: [f32; 3]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [1.0, 2.0, 3.0, 10.0, 20.0, 30.0] {
            bytes.extend(f32::to_le_bytes(value));
        }
        for value in normal {
            bytes.extend(value.to_le_bytes());
        }
        for value in [0, kind_code, 0] {
            bytes.extend(i32::to_le_bytes(value));
        }
        for value in cone.into_iter().chain([64.0]).chain(attenuation) {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend([0; 12]);
        assert_eq!(bytes.len(), RECORD_SIZE_V0);
        bytes
    }

    #[test]
    fn parses_records_of_each_kind() {
        let light = parse(&record(1, [1.0, 0.0, 0.0], [0.0; 3], [0.0, 0.0, 1.0])).expect("point");
        assert_eq!(light.kind, LightKind::Point);
        assert_eq!(light.origin, [1.0, 2.0, 3.0]);
        assert_eq!(light.intensity, [10.0, 20.0, 30.0]);
        assert_eq!(light.radius, 64.0);
        assert_eq!(light.attenuation, [0.0, 0.0, 1.0]);

        let spot = parse(&record(2, [0.0, 0.0, -1.0], [0.9, 0.5, 1.0], [0.0; 3])).expect("spot");
        assert_eq!(
            spot.kind,
            LightKind::Spot {
                direction: [0.0, 0.0, -1.0],
                inner_cos: 0.9,
                outer_cos: 0.5,
                exponent: 1.0
            }
        );
        assert!(matches!(
            parse(&record(3, [0.0, 0.0, -1.0], [0.0; 3], [0.0; 3])).map(|l| l.kind),
            Some(LightKind::Sun { .. })
        ));
        assert_eq!(
            parse(&record(5, [0.0; 3], [0.0; 3], [0.0; 3])),
            None,
            "sky ambient"
        );
    }

    #[test]
    fn point_lights_fall_off_with_distance_and_radius() {
        let light = point([0.0, 0.0, 100.0]);
        let (color, towards) = light.arriving_at([0.0; 3]).expect("in range");
        assert!((color[0] - 100.0 / 10_000.0).abs() < 1e-6);
        assert_eq!(towards, [0.0, 0.0, 1.0]);

        let limited = WorldLight {
            radius: 50.0,
            ..light
        };
        assert_eq!(limited.arriving_at([0.0; 3]), None);
    }

    #[test]
    fn spot_lights_only_reach_their_cone() {
        let spot = WorldLight {
            kind: LightKind::Spot {
                direction: [0.0, 0.0, -1.0],
                inner_cos: 0.9,
                outer_cos: 0.5,
                exponent: 1.0,
            },
            ..point([0.0, 0.0, 100.0])
        };
        assert!(spot.arriving_at([0.0; 3]).is_some(), "straight below");
        assert_eq!(
            spot.arriving_at([0.0, 0.0, 200.0]),
            None,
            "behind the light"
        );
        assert!(
            spot.arriving_at([100.0, 0.0, 0.0]).is_some(),
            "45° is inside a 60° outer cone"
        );
        assert_eq!(
            spot.arriving_at([200.0, 0.0, 0.0]),
            None,
            "63° is outside it"
        );
    }

    #[test]
    fn the_sun_comes_from_against_its_direction() {
        let sun = WorldLight {
            kind: LightKind::Sun {
                direction: [0.0, 0.0, -1.0],
            },
            ..point([0.0; 3])
        };
        let (color, towards) = sun.arriving_at([5.0, 5.0, 5.0]).expect("sun");
        assert_eq!(color, [100.0; 3], "no falloff");
        assert_eq!(towards, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn folded_light_reaches_only_the_sides_facing_it() {
        let mut cube = AmbientCube::default();
        let diagonal = 0.5f32.sqrt();
        add_to_cube(&mut cube, [1.0; 3], [diagonal, 0.0, diagonal]);
        assert!((cube[0][0] - diagonal).abs() < 1e-6, "+X");
        assert_eq!(cube[1], [0.0; 3], "-X faces away");
        assert_eq!(cube[2], [0.0; 3], "+Y is perpendicular");
        assert!((cube[4][0] - diagonal).abs() < 1e-6, "+Z");
    }
}
