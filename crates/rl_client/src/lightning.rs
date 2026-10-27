//! Source's noise beams (`TE_BEAMPOINTS` with an amplitude): a ribbon from one
//! point to another that crackles, as the Gravity Gun's arcs do.
//!
//! This is `DrawSegs` of the engine's `beamdraw.cpp`: the beam is cut into
//! `segments` points (fewer if it is short for its width, so a thick beam
//! over a short distance stays straight); each is moved sideways by a
//! fractal noise (new every frame, which is what makes it crackle) times the
//! amplitude and a hundredth of the beam's length in units; the ribbon is as
//! wide as the beam is there, widening from one end to the other, and its
//! texture slides along it so fast (`speed`) that it looks different every
//! frame.

use bevy::prelude::*;

/// The size of the noise table, `NOISE_DIVISIONS`.
pub(crate) const NOISE_DIVISIONS: usize = 128;

/// How a beam looks, as `BeamInfo_t` gives it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BeamShape {
    /// Width at the start and at the end, units.
    pub(crate) widths: (f32, f32),
    /// How far the noise throws it (`m_flAmplitude`).
    pub(crate) amplitude: f32,
    /// How many points it is cut into at most.
    pub(crate) segments: usize,
    /// How fast the texture slides along it (`m_flSpeed`).
    pub(crate) speed: f32,
}

/// One point of the ribbon.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    /// Where it is, in the units the beam was given in.
    pub(crate) position: Vec3,
    /// How wide the ribbon is there.
    pub(crate) width: f32,
    /// How far along the texture it is.
    pub(crate) v: f32,
}

/// A small, fast random number generator: a splitmix over a counter, enough
/// for an effect that is new every frame.
#[derive(Debug, Clone)]
pub(crate) struct Random(u64);

impl Random {
    /// A generator that starts from `seed`.
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// The next number in `[0, 1)`.
    pub(crate) fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 40) as f32 / (1u64 << 24) as f32
    }

    /// The next number in `[-1, 1)`.
    pub(crate) fn signed(&mut self) -> f32 {
        self.next() * 2.0 - 1.0
    }
}

/// A fractal noise over `NOISE_DIVISIONS + 1` points: nothing at the two ends,
/// the middle thrown by up to half, and each half's middle by half as much
/// again, so it wanders at every scale like lightning.
pub(crate) fn fractal_noise(random: &mut Random) -> Vec<f32> {
    let mut noise = vec![0.0; NOISE_DIVISIONS + 1];
    displace(&mut noise, 0, NOISE_DIVISIONS, 0.5, random);
    noise
}

/// Sets the middle of `noise[low..=high]` to the middle of its ends moved by
/// up to `scale`, then does the same to each half.
fn displace(noise: &mut [f32], low: usize, high: usize, scale: f32, random: &mut Random) {
    if high - low < 2 {
        return;
    }
    let middle = usize::midpoint(low, high);
    noise[middle] = f32::midpoint(noise[low], noise[high]) + random.signed() * scale;
    displace(noise, low, middle, scale * 0.5, random);
    displace(noise, middle, high, scale * 0.5, random);
}

/// How many points a beam is cut into: as many as it is allowed, unless it
/// is so short for how wide it is that they would overlap.
pub(crate) fn point_count(length: f32, shape: &BeamShape) -> usize {
    let widest = shape.widths.0.max(shape.widths.1) * 0.5;
    let wanted = shape.segments.max(2);
    if widest <= 0.0 {
        return wanted;
    }
    let fits = (length / (widest * std::f32::consts::SQRT_2)) as usize + 1;
    wanted.min(fits).max(2)
}

/// The points of the beam from `start` to `end`, in units, seen from
/// `toward_camera` (a direction from the beam to the camera), at `time`
/// seconds. `noise` is the table [`fractal_noise`] made for this frame.
pub(crate) fn points(
    start: Vec3,
    end: Vec3,
    toward_camera: Vec3,
    shape: &BeamShape,
    noise: &[f32],
    time: f32,
) -> Vec<Point> {
    let delta = end - start;
    let length = delta.length();
    let count = point_count(length, shape);
    let step = 1.0 / (count - 1) as f32;
    // The texture is stretched a hundredth of its height per unit, and slid
    // along by `speed`.
    let hundredths = length * 0.01;
    let v_step = hundredths * step;
    let v_start = (time * shape.speed).fract();
    // Noise is sideways on the screen.
    let sideways = delta.cross(toward_camera).normalize_or_zero();
    let throw = shape.amplitude * hundredths;
    (0..count)
        .map(|i| {
            let fraction = i as f32 * step;
            let index = (fraction * NOISE_DIVISIONS as f32).round() as usize;
            let noise = noise.get(index).copied().unwrap_or(0.0);
            Point {
                position: start + delta * fraction + sideways * (noise * throw),
                width: shape.widths.0 + (shape.widths.1 - shape.widths.0) * fraction,
                v: v_start + v_step * i as f32,
            }
        })
        .collect()
}

/// The triangles of the ribbon through `points`, turned to face the camera
/// (`toward_camera` from each point): vertex positions (a pair per point,
/// either side of it), texture coordinates and indices.
pub(crate) fn ribbon(
    points: &[Point],
    toward_camera: impl Fn(Vec3) -> Vec3,
) -> (Vec<[f32; 3]>, Vec<[f32; 2]>, Vec<u32>) {
    let mut positions = Vec::with_capacity(points.len() * 2);
    let mut uvs = Vec::with_capacity(points.len() * 2);
    for (i, point) in points.iter().enumerate() {
        let before = points[i.saturating_sub(1)].position;
        let after = points[(i + 1).min(points.len() - 1)].position;
        let along = (after - before).normalize_or_zero();
        let across = along
            .cross(toward_camera(point.position))
            .normalize_or_zero();
        let half = across * (point.width * 0.5);
        positions.push((point.position - half).to_array());
        positions.push((point.position + half).to_array());
        uvs.push([0.0, point.v]);
        uvs.push([1.0, point.v]);
    }
    let mut indices = Vec::with_capacity(points.len().saturating_sub(1) * 6);
    for i in 0..points.len().saturating_sub(1) {
        let a = (i * 2) as u32;
        indices.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    (positions, uvs, indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPE: BeamShape = BeamShape {
        widths: (0.0, 4.0),
        amplitude: 16.0,
        segments: 8,
        speed: 150.0,
    };

    #[test]
    fn the_random_numbers_stay_in_range_and_differ() {
        let mut random = Random::new(7);
        let numbers: Vec<f32> = (0..1000).map(|_| random.next()).collect();
        assert!(numbers.iter().all(|n| (0.0..1.0).contains(n)));
        let mean = numbers.iter().sum::<f32>() / numbers.len() as f32;
        assert!((mean - 0.5).abs() < 0.05, "mean {mean}");
        assert_ne!(
            Random::new(1).next().to_bits(),
            Random::new(2).next().to_bits()
        );
    }

    #[test]
    fn the_noise_starts_and_ends_at_nothing_and_wanders_between() {
        let noise = fractal_noise(&mut Random::new(3));
        assert_eq!(noise.len(), NOISE_DIVISIONS + 1);
        assert_eq!((noise[0], noise[NOISE_DIVISIONS]), (0.0, 0.0));
        assert!(noise[NOISE_DIVISIONS / 2].abs() <= 0.5);
        assert!(noise.iter().any(|n| n.abs() > 0.05), "it moves");
        assert!(noise.iter().all(|n| n.abs() <= 1.0), "within a unit");
    }

    #[test]
    fn it_is_new_every_time() {
        let mut random = Random::new(3);
        assert_ne!(fractal_noise(&mut random), fractal_noise(&mut random));
    }

    #[test]
    fn a_short_thick_beam_gets_fewer_points() {
        // Width 4 needs 2.83 units between points.
        assert_eq!(point_count(1.0, &SHAPE), 2);
        assert_eq!(point_count(6.0, &SHAPE), 3);
        assert_eq!(point_count(100.0, &SHAPE), 8);
        let thin = BeamShape {
            widths: (0.5, 2.0),
            ..SHAPE
        };
        assert_eq!(point_count(6.0, &thin), 5);
    }

    #[test]
    fn the_beam_runs_from_end_to_end_and_widens() {
        let noise = fractal_noise(&mut Random::new(5));
        let (start, end) = (Vec3::new(1.0, 2.0, 3.0), Vec3::new(1.0, 2.0, 33.0));
        let points = points(start, end, Vec3::X, &SHAPE, &noise, 0.0);
        assert_eq!(points.len(), 8);
        assert!(points[0].position.abs_diff_eq(start, 1e-5));
        assert!(points[7].position.abs_diff_eq(end, 1e-5));
        assert_eq!(points[0].width, 0.0);
        assert!((points[7].width - 4.0).abs() < 1e-5);
        assert!(points.windows(2).all(|pair| pair[0].width < pair[1].width));
    }

    #[test]
    fn the_noise_throws_it_sideways_by_the_amplitude_and_a_hundredth_of_its_length() {
        // A 50-unit beam: 16 × 0.5 = 8 units at most per unit of noise.
        let mut noise = vec![0.0; NOISE_DIVISIONS + 1];
        noise[NOISE_DIVISIONS / 2] = 1.0;
        let shape = BeamShape {
            segments: 3,
            widths: (0.0, 0.0),
            ..SHAPE
        };
        let points = points(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, 50.0),
            Vec3::X,
            &shape,
            &noise,
            0.0,
        );
        assert_eq!(points.len(), 3);
        let thrown = points[1].position - Vec3::new(0.0, 0.0, 25.0);
        assert!((thrown.length() - 8.0).abs() < 1e-4, "{thrown}");
        assert!(thrown.dot(Vec3::X).abs() < 1e-4, "not toward the camera");
    }

    #[test]
    fn the_texture_slides_along_it_and_is_stretched_over_a_hundredth_of_the_length() {
        let noise = vec![0.0; NOISE_DIVISIONS + 1];
        let (start, end) = (Vec3::ZERO, Vec3::new(0.0, 0.0, 50.0));
        let still = points(start, end, Vec3::X, &SHAPE, &noise, 0.0);
        assert_eq!(still[0].v, 0.0);
        assert!(
            (still[7].v - 0.5).abs() < 1e-5,
            "50 units is half a texture"
        );
        let moved = points(start, end, Vec3::X, &SHAPE, &noise, 0.01);
        assert!(
            (moved[0].v - 0.5).abs() < 1e-5,
            "150 per second, a hundredth"
        );
    }

    #[test]
    fn the_ribbon_is_a_strip_of_quads_facing_the_camera() {
        let noise = vec![0.0; NOISE_DIVISIONS + 1];
        let shape = BeamShape {
            widths: (2.0, 2.0),
            amplitude: 0.0,
            ..SHAPE
        };
        let points = points(
            Vec3::ZERO,
            Vec3::new(0.0, 0.0, 40.0),
            Vec3::X,
            &shape,
            &noise,
            0.0,
        );
        let (positions, uvs, indices) = ribbon(&points, |_| Vec3::X);
        assert_eq!(positions.len(), points.len() * 2);
        assert_eq!(uvs.len(), positions.len());
        assert_eq!(indices.len(), (points.len() - 1) * 6);
        assert!(indices.iter().all(|&i| (i as usize) < positions.len()));
        // Two units wide, in the screen plane (here, along Y).
        let (left, right) = (Vec3::from(positions[0]), Vec3::from(positions[1]));
        assert!(((right - left).length() - 2.0).abs() < 1e-5);
        assert!((right - left).dot(Vec3::X).abs() < 1e-5);
        assert_eq!((uvs[0][0], uvs[1][0]), (0.0, 1.0));
    }
}
