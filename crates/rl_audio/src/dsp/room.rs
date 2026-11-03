//! The automatic room effect (soundscape `dsp` 1): HL2DM's "DSP automatic
//! system", which listens to the shape of the place.
//!
//! Every so often the engine traces [`PROBES`] lines out from the listener
//! — eight level ones, ten rising ones and one straight up and one down —
//! and notes how far each runs before it hits something, whether that was
//! the sky and how reflective the surface is (its `audioreflectivity`).
//! From those it measures the place ([`RoomShape::measure`]): the width and
//! length across the level traces (the pair of axes that encloses the most
//! area), the height from the rising ones, the average reflectivity of the
//! walls, ceiling and floor, and whether it is outside (a rising trace saw
//! the sky). The shape is sorted into one of eleven kinds
//! ([`RoomKind::classify`]: a duct, a hall, a tunnel, a street...), and
//! each kind has two template presets in `dsp_presets.txt`, the smallest
//! brightest room of the kind and the hugest dullest. The effect played is
//! a blend of the two, by how big the place is, how reflective, and how
//! cluttered ([`automatic_preset`]).
//!
//! The thresholds, template numbers and blend rules are HL2DM's
//! (`engine.so`: `DAS_CheckNewRoomDSP` and the functions it calls). How
//! cluttered a place is (its "diffusion") rustlife estimates from the props
//! the level traces hit.

use glam::Vec3;
use source_formats::dsp_presets::{DspPresets, PresetDef, ProcessorKind};

/// Directions of the probe traces, in Source's axes (x forward, y left, z
/// up) and units: their lengths are how far they reach (4800 units level
/// and straight up and down).
pub const PROBES: [Vec3; 20] = [
    Vec3::new(4800.0, 0.0, 0.0),
    Vec3::new(-4800.0, 0.0, 0.0),
    Vec3::new(0.0, 4800.0, 0.0),
    Vec3::new(0.0, -4800.0, 0.0),
    Vec3::new(-2496.0, 2304.0, 0.0),
    Vec3::new(2496.0, -2304.0, 0.0),
    Vec3::new(2496.0, 2304.0, 0.0),
    Vec3::new(-2496.0, -2304.0, 0.0),
    Vec3::new(2496.0, 2304.0, 1248.0),
    Vec3::new(2496.0, 2304.0, 2496.0),
    Vec3::new(1248.0, 1152.0, 2496.0),
    Vec3::new(-2496.0, -2304.0, 1248.0),
    Vec3::new(-2496.0, -2304.0, 2496.0),
    Vec3::new(-1248.0, -1152.0, 2496.0),
    Vec3::new(-2496.0, 0.0, 2496.0),
    Vec3::new(0.0, 1248.0, 2496.0),
    Vec3::new(0.0, -2496.0, 2496.0),
    Vec3::new(1248.0, 0.0, 2496.0),
    Vec3::new(0.0, 0.0, 4800.0),
    Vec3::new(0.0, 0.0, -4800.0),
];
/// How far a probe that hits nothing is taken to have gone.
pub const PROBE_REACH: f32 = 4800.0;
/// Probes 0-7 are level; 8-18 rise (18 straight up); 19 goes down.
const LEVEL: std::ops::Range<usize> = 0..8;
const RISING: std::ops::Range<usize> = 8..18;
const UP: usize = 18;
const DOWN: usize = 19;

/// What one probe found.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Probe {
    /// How far it went before hitting something ([`PROBE_REACH`] if it hit
    /// nothing).
    pub distance: f32,
    /// The `audioreflectivity` of what it hit (0 for nothing or the sky).
    pub reflectivity: f32,
    /// Whether it hit the sky.
    pub sky: bool,
    /// Whether a prop or player stood in the way before the wall.
    pub cluttered: bool,
}

impl Probe {
    /// A probe that hit nothing.
    pub const OPEN: Self = Self {
        distance: PROBE_REACH,
        reflectivity: 0.0,
        sky: false,
        cluttered: false,
    };
}

/// The shape of the place around the listener.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomShape {
    /// Whether it is outside (the sky is above).
    pub outside: bool,
    /// The shorter level extent, units.
    pub width: f32,
    /// The longer level extent, units.
    pub length: f32,
    /// How high it goes, units.
    pub height: f32,
    /// How cluttered it is, `0..=1`.
    pub diffusion: f32,
    /// Average reflectivity of its walls, ceiling and floor.
    pub reflectivity: f32,
    /// Of the four level walls measured, how many are open (nothing or
    /// sky).
    pub open_sides: u8,
}

impl RoomShape {
    /// Measures the place from [`PROBES`]' results; `None` when they
    /// disagree about the sky (it shows on a slant but not straight up),
    /// where HL2DM keeps the room it had.
    #[must_use]
    pub fn measure(probes: &[Probe; 20]) -> Option<Self> {
        let sky_slanted = probes[RISING].iter().any(|probe| probe.sky);
        if sky_slanted && !probes[UP].sky {
            return None;
        }
        let extent = |a: usize, b: usize| probes[a].distance + probes[b].distance;
        let spans = [extent(0, 1), extent(2, 3), extent(4, 5), extent(6, 7)];
        let (square, diagonal) = (spans[0] * spans[1], spans[2] * spans[3]);
        // Use the pair of axes enclosing the most area; when the two are
        // alike, the pair with the single longest span.
        let use_diagonals = if square.min(diagonal) / square.max(diagonal).max(1.0) < 0.75 {
            diagonal > square
        } else {
            let longest = (0..4)
                .max_by(|&a, &b| spans[a].total_cmp(&spans[b]))
                .unwrap_or(0);
            longest >= 2
        };
        let (walls, a, b) = if use_diagonals {
            (4..8, spans[2], spans[3])
        } else {
            (0..4, spans[0], spans[1])
        };
        let height = probes[RISING.start..=UP]
            .iter()
            .map(|probe| probe.distance)
            .fold(0.0, f32::max);
        let ceiling = probes[RISING.start..=UP]
            .iter()
            .find(|probe| probe.reflectivity > 0.0)
            .map_or(0.0, |probe| probe.reflectivity);
        let wall_reflectivity: f32 = probes[walls.clone()].iter().map(|p| p.reflectivity).sum();
        let cluttered = probes[LEVEL].iter().filter(|probe| probe.cluttered).count();
        Some(Self {
            outside: sky_slanted || probes[UP].sky,
            width: a.min(b),
            length: a.max(b),
            height,
            diffusion: cluttered as f32 / LEVEL.len() as f32 * 0.5,
            reflectivity: (wall_reflectivity + ceiling + probes[DOWN].reflectivity) / 6.0,
            open_sides: probes[walls]
                .iter()
                .filter(|p| p.reflectivity <= 0.0)
                .count() as u8,
        })
    }
}

/// The kinds of place the automatic effect tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomKind {
    /// An ordinary room.
    Room,
    /// Narrow and long.
    Duct,
    /// Wide-ish and long.
    Hall,
    /// Wide and very long.
    Tunnel,
    /// Outside between walls, wide and long.
    Street,
    /// Outside between walls, narrow and long.
    Alley,
    /// Outside, walled on every side.
    Courtyard,
    /// Outside with nothing around.
    OpenSpace,
    /// Outside by one wall.
    OpenWall,
    /// Outside between two walls.
    OpenStreet,
    /// Outside walled on three sides.
    OpenCourtyard,
}

impl RoomKind {
    /// The kind of place `shape` is.
    #[must_use]
    pub fn classify(shape: &RoomShape) -> Self {
        let (width, mut length) = (shape.width, shape.length);
        if shape.outside {
            return match shape.open_sides {
                0 if length > width * 2.5 && width > 144.0 => Self::Street,
                0 if length > width * 2.5 => Self::Alley,
                0 => Self::Courtyard,
                1 => Self::OpenCourtyard,
                2 => Self::OpenStreet,
                3 => Self::OpenWall,
                _ => Self::OpenSpace,
            };
        }
        // A vertical shaft is judged by its height.
        if length * 3.0 < shape.height {
            length = shape.height;
        }
        let long = length > width * 4.0;
        if width <= 48.0 && long {
            Self::Duct
        } else if length > width * 2.5 && (49.0..=96.0).contains(&width) {
            Self::Hall
        } else if width >= 97.0 && long {
            Self::Tunnel
        } else {
            Self::Room
        }
    }

    /// The first of its templates (`adsp_*_min`): the small bright empty
    /// room, then the huge dull one; two on, the same cluttered.
    #[must_use]
    pub fn first_template(self) -> usize {
        match self {
            Self::Room => 102,
            Self::Duct => 106,
            Self::Hall => 110,
            Self::Tunnel => 114,
            Self::Street | Self::OpenStreet => 118,
            Self::Alley => 122,
            Self::Courtyard | Self::OpenCourtyard => 126,
            Self::OpenSpace | Self::OpenWall => 130,
        }
    }
}

/// Steps of the size, width, length, height scales (`0..=4`); a blend
/// goes `step / 5` of the way from the small template to the huge.
const STEPS: f32 = 5.0;
/// Steps of the reflectivity scale (`0..=3`), over 4.
const REFLECTIVITY_STEPS: f32 = 4.0;

/// Where a shape falls on each of the blend's scales.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scales {
    size: u8,
    width: u8,
    length: u8,
    height: u8,
    reflectivity: u8,
    diffusion: u8,
}

impl Scales {
    fn of(shape: &RoomShape, kind: RoomKind) -> Self {
        let step =
            |value: f32, bounds: [f32; 4]| bounds.iter().filter(|&&b| value > b).count() as u8;
        let long = step(shape.length, [120.0, 240.0, 480.0, 1200.0]);
        let length = step(shape.length, [144.0, 288.0, 576.0, 1152.0]);
        let width = step(shape.width, [72.0, 144.0, 288.0, 576.0]);
        let size = match kind {
            RoomKind::Hall | RoomKind::Courtyard => long,
            RoomKind::Street | RoomKind::Alley => width,
            _ => u8::midpoint(length, width),
        };
        let step3 =
            |value: f32, bounds: [f32; 3]| bounds.iter().filter(|&&b| value > b).count() as u8;
        Self {
            size,
            width,
            length,
            height: step(shape.height, [48.0, 128.0, 216.0, 384.0]),
            reflectivity: step3(shape.reflectivity, [0.04, 0.5, 0.8]),
            diffusion: step3(shape.diffusion, [0.01, 0.1, 0.3]),
        }
    }
}

/// The preset the automatic effect plays for a place of `shape`, blended
/// from the templates of its kind; `None` if the file lacks them.
#[must_use]
pub fn automatic_preset(presets: &DspPresets, shape: &RoomShape) -> Option<PresetDef> {
    let kind = RoomKind::classify(shape);
    let scales = Scales::of(shape, kind);
    let first = kind.first_template() + if scales.diffusion > 1 { 2 } else { 0 };
    let small = presets.get(first)?;
    let huge = presets.get(first + 1)?;
    let mut blend = if scales.size < 2 {
        small.clone()
    } else {
        huge.clone()
    };
    let sizes = Sizes::of(shape);
    for nth in 0..2 {
        let mut set = |kind, param, step: u8, squared, value: Option<f32>| {
            blend_param(
                &mut blend,
                small,
                huge,
                kind,
                nth,
                param,
                f32::from(step) / STEPS,
                squared,
                value,
            );
        };
        for param in 0..4 {
            set(ProcessorKind::Diffusor, param, scales.size, false, None);
        }
        reverb_params(&mut set, scales, &sizes);
        delay_params(&mut set, scales, &sizes);
        let other: [(ProcessorKind, usize); 8] = [
            (ProcessorKind::ModDelay, 13),
            (ProcessorKind::Amplifier, 8),
            (ProcessorKind::Filter, 5),
            (ProcessorKind::Pitch, 3),
            (ProcessorKind::Chorus, 4),
            (ProcessorKind::Envelope, 9),
            (ProcessorKind::EnvelopeFollower, 4),
            (ProcessorKind::Lfo, 4),
        ];
        for (kind, count) in other {
            let step = if kind == ProcessorKind::ModDelay {
                scales.length
            } else {
                scales.size
            };
            for param in 0..count {
                set(kind, param, step, false, None);
            }
        }
        // Brighter (more reflective) places keep more treble: the cutoff
        // runs from the huge dull template back to the small bright one.
        blend_param(
            &mut blend,
            huge,
            small,
            ProcessorKind::Reverb,
            nth,
            5,
            f32::from(scales.reflectivity) / REFLECTIVITY_STEPS,
            false,
            None,
        );
        for param in [5, 6] {
            blend_param(
                &mut blend,
                huge,
                small,
                ProcessorKind::Delay,
                nth,
                param,
                f32::from(scales.length) / STEPS,
                false,
                None,
            );
        }
    }
    Some(blend)
}

/// A setter of one blended parameter: processor kind, parameter, scale
/// step, whether the blend is squared, and the value to use where the
/// templates ask for one from the room's size.
type Set<'a> = dyn FnMut(ProcessorKind, usize, u8, bool, Option<f32>) + 'a;

fn reverb_params(set: &mut Set<'_>, scales: Scales, sizes: &Sizes) {
    let reverb = ProcessorKind::Reverb;
    for (param, squared) in [
        (3, false),
        (1, true),
        (0, true),
        (4, false),
        (2, false),
        (7, false),
        (8, false),
    ] {
        set(reverb, param, scales.size, squared, None);
    }
    set(reverb, 9, scales.width, true, Some(sizes.width_feet));
    set(reverb, 10, scales.length, true, Some(sizes.length_feet));
    set(reverb, 11, scales.height, true, Some(sizes.height_feet));
    set(reverb, 12, scales.width, false, None);
    set(reverb, 13, scales.length, false, None);
    set(reverb, 14, scales.height, false, None);
}

fn delay_params(set: &mut Set<'_>, scales: Scales, sizes: &Sizes) {
    let delay = ProcessorKind::Delay;
    set(delay, 1, scales.length, true, Some(sizes.length_delay));
    set(delay, 2, scales.length, false, None);
    set(delay, 3, scales.length, false, None);
    set(delay, 8, scales.length, true, Some(sizes.width_delay));
    set(delay, 9, scales.length, true, Some(sizes.length_delay));
    set(delay, 10, scales.length, true, Some(sizes.length_delay));
}

/// The room's size in the units template parameters of -1 take it in.
struct Sizes {
    width_feet: f32,
    length_feet: f32,
    height_feet: f32,
    /// Delay of an echo across the room, ms (feet × 2).
    length_delay: f32,
    width_delay: f32,
}

impl Sizes {
    fn of(shape: &RoomShape) -> Self {
        let feet = |units: f32| {
            if units < 72.0 {
                6.0
            } else if units > 6000.0 {
                500.0
            } else {
                units / 12.0
            }
        };
        let delay = |units: f32| {
            if units <= 83.0 {
                14.0
            } else if units > 3000.0 {
                500.0
            } else {
                units / 6.0
            }
        };
        Self {
            width_feet: feet(shape.width),
            length_feet: feet(shape.length),
            height_feet: feet(shape.height),
            length_delay: delay(shape.length),
            width_delay: delay(shape.width),
        }
    }
}

/// Sets parameter `param` of the `nth` processor of `kind` in `blend`
/// `t` (squared if asked) of the way from `from`'s to `to`'s — or to
/// `sized` when either template gives it as negative ("from the room").
fn blend_param(
    blend: &mut PresetDef,
    from: &PresetDef,
    to: &PresetDef,
    kind: ProcessorKind,
    nth: usize,
    param: usize,
    t: f32,
    squared: bool,
    sized: Option<f32>,
) {
    let (Some(a), Some(b)) = (from.nth(kind, nth), to.nth(kind, nth)) else {
        return;
    };
    let (a, b) = (a.params[param], b.params[param]);
    let Some(target) = blend.nth_mut(kind, nth) else {
        return;
    };
    let t = if squared { t * t } else { t };
    target.params[param] = match sized {
        Some(value) if a < 0.0 || b < 0.0 => value,
        _ => a + (b - a) * t,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walls(level: [f32; 8], height: f32, reflectivity: f32) -> [Probe; 20] {
        let mut probes = [Probe::OPEN; 20];
        for (probe, distance) in probes.iter_mut().zip(level) {
            *probe = Probe {
                distance,
                reflectivity,
                sky: false,
                cluttered: false,
            };
        }
        for probe in &mut probes[8..20] {
            *probe = Probe {
                distance: height,
                reflectivity,
                sky: false,
                cluttered: false,
            };
        }
        probes
    }

    fn shape(width: f32, length: f32, height: f32, outside: bool, open_sides: u8) -> RoomShape {
        RoomShape {
            outside,
            width,
            length,
            height,
            diffusion: 0.0,
            reflectivity: 0.66,
            open_sides,
        }
    }

    #[test]
    fn a_box_measures_its_width_length_and_height() {
        let room = RoomShape::measure(&walls(
            [100.0, 100.0, 50.0, 50.0, 60.0, 60.0, 60.0, 60.0],
            120.0,
            0.66,
        ))
        .expect("measured");
        assert_eq!(
            (room.width, room.length, room.height),
            (100.0, 200.0, 120.0)
        );
        assert!(!room.outside);
        assert_eq!(room.open_sides, 0);
        assert!((room.reflectivity - 0.66).abs() < 1e-6);
    }

    #[test]
    fn the_sky_makes_it_outside_unless_only_slanted() {
        let mut probes = walls([500.0; 8], PROBE_REACH, 0.66);
        probes[10].sky = true;
        assert_eq!(RoomShape::measure(&probes), None, "sky on a slant only");
        probes[18].sky = true;
        probes[0] = Probe::OPEN;
        let open = RoomShape::measure(&probes).expect("measured");
        assert!(open.outside);
        assert_eq!(open.open_sides, 1);
    }

    #[test]
    fn places_sort_into_kinds_as_in_hl2dm() {
        assert_eq!(
            RoomKind::classify(&shape(40.0, 400.0, 100.0, false, 0)),
            RoomKind::Duct
        );
        assert_eq!(
            RoomKind::classify(&shape(80.0, 400.0, 100.0, false, 0)),
            RoomKind::Hall
        );
        assert_eq!(
            RoomKind::classify(&shape(200.0, 1000.0, 200.0, false, 0)),
            RoomKind::Tunnel
        );
        assert_eq!(
            RoomKind::classify(&shape(300.0, 400.0, 150.0, false, 0)),
            RoomKind::Room
        );
        assert_eq!(
            RoomKind::classify(&shape(40.0, 60.0, 900.0, false, 0)),
            RoomKind::Duct,
            "a shaft"
        );
        assert_eq!(
            RoomKind::classify(&shape(200.0, 900.0, 500.0, true, 0)),
            RoomKind::Street
        );
        assert_eq!(
            RoomKind::classify(&shape(100.0, 900.0, 500.0, true, 0)),
            RoomKind::Alley
        );
        assert_eq!(
            RoomKind::classify(&shape(500.0, 600.0, 500.0, true, 0)),
            RoomKind::Courtyard
        );
        assert_eq!(
            RoomKind::classify(&shape(500.0, 600.0, 500.0, true, 4)),
            RoomKind::OpenSpace
        );
        assert_eq!(
            RoomKind::classify(&shape(500.0, 600.0, 500.0, true, 2)),
            RoomKind::OpenStreet
        );
    }

    const TEMPLATES: &str = "
        { 102 LINEAR 0.3 0.8 0 0 80 0.5
            { RVA 80.0 30.0 2 0.78 1.1 6000 1 0 0 0 0 0 0 0 0 0 } }
        { 103 LINEAR 0.32 0.8 0 0 80 0.5
            { DFR 1.0 3 0.15 }
            { RVA 240.0 50.0 10 0.97 2.4 1800 1 0 0 0 0 0 0 0 0 0 } }
        { 126 LINEAR 0.21 0.8 0 0 95 0.1
            { DLY LOWPASS -1.0 0.5 1.2 LP 4000 0 LO -1 -1 -1 } }
        { 127 LINEAR 0.21 0.8 0 0 95 0.1
            { DLY LOWPASS -1.0 0.4 1.2 LP 900 0 LO -1 -1 -1 } }
    ";

    #[test]
    fn bigger_rooms_blend_towards_the_huge_template() {
        let presets = DspPresets::parse(TEMPLATES);
        let small =
            automatic_preset(&presets, &shape(80.0, 100.0, 100.0, false, 0)).expect("small");
        let big = automatic_preset(&presets, &shape(600.0, 1300.0, 400.0, false, 0)).expect("big");
        let size = |p: &PresetDef| p.nth(ProcessorKind::Reverb, 0).expect("RVA").params[0];
        assert_eq!(
            size(&small),
            80.0,
            "the smallest room is the small template"
        );
        assert!(size(&big) > 150.0, "{}", size(&big));
        assert_eq!(small.processors.len(), 1, "built on the small template");
        assert_eq!(big.processors.len(), 2, "built on the huge one");
    }

    #[test]
    fn courtyard_echoes_come_back_from_its_walls() {
        let presets = DspPresets::parse(TEMPLATES);
        let yard = automatic_preset(&presets, &shape(600.0, 900.0, 600.0, true, 0)).expect("yard");
        let delay = yard.nth(ProcessorKind::Delay, 0).expect("DLY");
        assert_eq!(delay.params[1], 150.0, "length / 6 ms");
        assert_eq!(delay.params[8], 100.0, "width / 6 ms");
        assert!(
            automatic_preset(&DspPresets::default(), &shape(1.0, 1.0, 1.0, false, 0)).is_none()
        );
    }
}
