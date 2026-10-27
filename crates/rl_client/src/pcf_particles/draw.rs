//! Turning particles into quads: the geometry of sprites, trails and ropes.
//!
//! Everything here is plain maths on Bevy-space vectors (metres, Y up), so
//! it is tested without a renderer. [`render`](super::render) feeds the
//! quads into meshes.

use bevy::math::Vec3;
use rl_assets::SizeLimits;
use source_formats::sheet::{Rect, Sheet};

/// One camera-facing (or otherwise oriented) rectangle to draw.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Quad {
    /// Top left, top right, bottom right, bottom left.
    pub corners: [Vec3; 4],
    /// The texture coordinates of each corner.
    pub uvs: [[f32; 2]; 4],
    /// Linear RGBA, multiplied into the texture.
    pub color: [f32; 4],
}

impl Quad {
    pub(super) fn center(&self) -> Vec3 {
        (self.corners[0] + self.corners[2]) * 0.5
    }
}

/// Where the viewer is and how it is turned.
#[derive(Debug, Clone, Copy)]
pub(super) struct View {
    pub position: Vec3,
    pub right: Vec3,
    pub up: Vec3,
}

/// How a sprite quad is turned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Facing {
    /// Toward the viewer.
    Camera,
    /// Upright, turning only about the vertical to face the viewer.
    Upright,
    /// Lying in the plane spanned by these axes.
    Plane(Vec3, Vec3),
}

/// The rectangle of the texture to show for a particle.
///
/// With a sprite sheet the particle's `sequence` picks an animation and time
/// picks the frame in it; without one the texture is `strip_frames` frames
/// side by side. `fit` stretches the animation over the particle's life
/// (`fraction`, `0..=1`); otherwise it plays at `rate` times its own speed
/// (frames per second for a strip).
pub(super) fn frame_rect(
    sheet: Option<&Sheet>,
    strip_frames: u32,
    sequence: i32,
    age: f32,
    fraction: f32,
    rate: f32,
    fit: bool,
) -> Rect {
    let sequence = sheet
        .and_then(|sheet| sheet.sequence(u32::try_from(sequence).unwrap_or(0)))
        .filter(|sequence| !sequence.frames.is_empty());
    if let Some(sequence) = sequence {
        let total: f32 = sequence.frames.iter().map(|frame| frame.duration).sum();
        let count = sequence.frames.len();
        if total <= 0.0 {
            let t = if fit {
                fraction
            } else {
                age * rate / count as f32
            };
            let index = ((t * count as f32) as usize).min(count - 1);
            return sequence.frames[index].rect;
        }
        let mut t = if fit { fraction * total } else { age * rate };
        t = if sequence.clamp || fit {
            t.min(total)
        } else {
            t.rem_euclid(total)
        };
        let mut elapsed = 0.0;
        for frame in &sequence.frames {
            elapsed += frame.duration;
            if t < elapsed {
                return frame.rect;
            }
        }
        return sequence.frames[count - 1].rect;
    }
    let frames = strip_frames.max(1);
    let t = if fit {
        fraction * frames as f32
    } else {
        age * rate
    };
    let index = (t.max(0.0) as u32).min(frames - 1);
    Rect {
        u0: index as f32 / frames as f32,
        v0: 0.0,
        u1: (index + 1) as f32 / frames as f32,
        v1: 1.0,
    }
}

/// A card's radius and opacity scale as the engine's `SpriteCard` shader
/// gives them, for a card of `radius` seen from `distance` away (same
/// units): the radius is kept between `limits.min` and `limits.max` times
/// the distance, and the card fades out, to nothing, as the radius grows
/// from `limits.start_fade` to `limits.end_fade` times the distance.
pub(super) fn limit_size(radius: f32, distance: f32, limits: SizeLimits) -> (f32, f32) {
    let distance = distance.max(f32::EPSILON);
    let size = radius.max(limits.min * distance);
    let fade = if size > limits.end_fade * distance {
        0.0
    } else if size > limits.start_fade * distance {
        1.0 - (size - limits.start_fade * distance)
            / ((limits.end_fade - limits.start_fade) * distance)
    } else {
        1.0
    };
    (size.min(limits.max * distance), fade.clamp(0.0, 1.0))
}

fn corners_uv(rect: Rect) -> [[f32; 2]; 4] {
    [
        [rect.u0, rect.v0],
        [rect.u1, rect.v0],
        [rect.u1, rect.v1],
        [rect.u0, rect.v1],
    ]
}

/// A square sprite of half-size `radius` at `center`, rolled by `roll`
/// radians.
pub(super) fn sprite(
    center: Vec3,
    radius: f32,
    roll: f32,
    facing: Facing,
    view: View,
    rect: Rect,
    color: [f32; 4],
) -> Quad {
    let (right, up) = match facing {
        Facing::Camera => (view.right, view.up),
        Facing::Upright => {
            let up = Vec3::Y;
            let to_viewer = view.position - center;
            let right = up.cross(to_viewer).try_normalize().unwrap_or(view.right);
            // Looking along the right axis of a camera facing -Z flips it.
            (-right, up)
        }
        Facing::Plane(right, up) => (right, up),
    };
    let (sin, cos) = roll.sin_cos();
    let rolled_right = right * cos + up * sin;
    let rolled_up = up * cos - right * sin;
    let (x, y) = (rolled_right * radius, rolled_up * radius);
    Quad {
        corners: [
            center - x + y,
            center + x + y,
            center + x - y,
            center - x - y,
        ],
        uvs: corners_uv(rect),
        color,
    }
}

/// A streak from `head` back along `velocity`: `length` long (clamped to
/// `min..=max`) and `radius` either side, turned about its axis to show its
/// face to the viewer. `None` for a particle that is standing still.
pub(super) fn trail(
    head: Vec3,
    velocity: Vec3,
    length: f32,
    (min, max): (f32, f32),
    radius: f32,
    view_position: Vec3,
    rect: Rect,
    color: [f32; 4],
) -> Option<Quad> {
    let direction = velocity.try_normalize()?;
    let length = length.clamp(min.min(max), max);
    if length <= f32::EPSILON {
        return None;
    }
    let tail = head - direction * length;
    let side = direction
        .cross(view_position - head)
        .try_normalize()
        .unwrap_or(Vec3::Y)
        * radius;
    // Texture runs tail (u0) to head (u1) along the streak.
    Some(Quad {
        corners: [tail + side, head + side, head - side, tail - side],
        uvs: corners_uv(rect),
        color,
    })
}

/// One point of a rope.
#[derive(Debug, Clone, Copy)]
pub(super) struct RopePoint {
    pub position: Vec3,
    pub radius: f32,
    pub color: [f32; 4],
}

/// A ribbon through `points` in order, one quad per gap, each widened
/// sideways to face the viewer. The texture's `v` runs along the rope and
/// `u` across it.
pub(super) fn rope(points: &[RopePoint], view_position: Vec3, rect: Rect) -> Vec<Quad> {
    if points.len() < 2 {
        return Vec::new();
    }
    // A side vector per point, from the direction of the rope through it.
    let sides: Vec<Vec3> = (0..points.len())
        .map(|i| {
            let before = points[i.saturating_sub(1)].position;
            let after = points[(i + 1).min(points.len() - 1)].position;
            (after - before)
                .cross(view_position - points[i].position)
                .try_normalize()
                .unwrap_or(Vec3::Y)
                * points[i].radius
        })
        .collect();
    let along = |i: usize| {
        let t = i as f32 / (points.len() - 1) as f32;
        rect.v0 + (rect.v1 - rect.v0) * t
    };
    (0..points.len() - 1)
        .map(|i| {
            let (a, b) = (&points[i], &points[i + 1]);
            Quad {
                corners: [
                    a.position - sides[i],
                    a.position + sides[i],
                    b.position + sides[i + 1],
                    b.position - sides[i + 1],
                ],
                uvs: [
                    [rect.u0, along(i)],
                    [rect.u1, along(i)],
                    [rect.u1, along(i + 1)],
                    [rect.u0, along(i + 1)],
                ],
                color: [
                    a.color[0].midpoint(b.color[0]),
                    a.color[1].midpoint(b.color[1]),
                    a.color[2].midpoint(b.color[2]),
                    a.color[3].midpoint(b.color[3]),
                ],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use source_formats::sheet::{Frame, Sequence};

    use super::*;

    const WHOLE: Rect = Rect {
        u0: 0.0,
        v0: 0.0,
        u1: 1.0,
        v1: 1.0,
    };

    fn view() -> View {
        View {
            position: Vec3::new(0.0, 0.0, 10.0),
            right: Vec3::X,
            up: Vec3::Y,
        }
    }

    fn sheet() -> Sheet {
        let frame = |duration, u: f32| Frame {
            duration,
            rect: Rect {
                u0: u,
                v0: 0.0,
                u1: u + 0.25,
                v1: 0.5,
            },
        };
        Sheet {
            sequences: vec![
                Sequence {
                    id: 0,
                    clamp: false,
                    total_time: 1.0,
                    frames: vec![frame(0.5, 0.0), frame(0.5, 0.25)],
                },
                Sequence {
                    id: 2,
                    clamp: true,
                    total_time: 1.0,
                    frames: vec![frame(0.5, 0.5), frame(0.5, 0.75)],
                },
            ],
        }
    }

    #[test]
    fn cards_keep_their_size_and_fade_when_they_fill_the_view() {
        let limits = SizeLimits {
            min: 0.01,
            max: 0.5,
            start_fade: 0.25,
            end_fade: 0.45,
        };
        // Small against its distance: as it is.
        assert_eq!(limit_size(1.0, 100.0, limits), (1.0, 1.0));
        // Too small to see: raised to the minimum.
        assert_eq!(limit_size(0.1, 100.0, limits).0, 1.0);
        // Halfway through the fade.
        let (size, fade) = limit_size(35.0, 100.0, limits);
        assert_eq!(size, 35.0);
        assert!((fade - 0.5).abs() < 1e-5, "{fade}");
        // Past the end of the fade: invisible, and never past the maximum.
        let (size, fade) = limit_size(80.0, 100.0, limits);
        assert_eq!((size, fade), (50.0, 0.0));
        // The defaults limit nothing a scene draws.
        assert_eq!(
            limit_size(100.0, 100.0, SizeLimits::default()),
            (100.0, 1.0)
        );
    }

    #[test]
    fn a_sprite_is_a_square_facing_the_camera() {
        let q = sprite(
            Vec3::ZERO,
            2.0,
            0.0,
            Facing::Camera,
            view(),
            WHOLE,
            [1.0; 4],
        );
        assert_eq!(q.corners[0], Vec3::new(-2.0, 2.0, 0.0));
        assert_eq!(q.corners[2], Vec3::new(2.0, -2.0, 0.0));
        assert_eq!(q.uvs[0], [0.0, 0.0]);
        assert_eq!(q.uvs[2], [1.0, 1.0]);
        assert_eq!(q.center(), Vec3::ZERO);
    }

    #[test]
    fn rolling_turns_the_corners_without_resizing() {
        let q = sprite(
            Vec3::ZERO,
            1.0,
            std::f32::consts::FRAC_PI_2,
            Facing::Camera,
            view(),
            WHOLE,
            [1.0; 4],
        );
        for corner in q.corners {
            assert!((corner.length() - 2.0_f32.sqrt()).abs() < 1e-5);
        }
        assert!((q.corners[0] - Vec3::new(-1.0, -1.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn upright_sprites_stay_vertical() {
        let q = sprite(
            Vec3::new(5.0, 0.0, 0.0),
            1.0,
            0.0,
            Facing::Upright,
            view(),
            WHOLE,
            [1.0; 4],
        );
        assert_eq!(q.corners[0].y, 1.0);
        assert_eq!(q.corners[3].y, -1.0);
    }

    #[test]
    fn a_plane_sprite_lies_in_its_plane() {
        let q = sprite(
            Vec3::ZERO,
            1.0,
            0.0,
            Facing::Plane(Vec3::X, Vec3::Z),
            view(),
            WHOLE,
            [1.0; 4],
        );
        assert!(q.corners.iter().all(|c| c.y == 0.0));
    }

    #[test]
    fn sheet_frames_follow_time_and_loop_or_clamp() {
        let sheet = sheet();
        let at = |sequence, age| frame_rect(Some(&sheet), 1, sequence, age, 0.0, 1.0, false).u0;
        assert_eq!(at(0, 0.1), 0.0);
        assert_eq!(at(0, 0.6), 0.25);
        assert_eq!(at(0, 1.1), 0.0, "loops");
        assert_eq!(at(2, 0.1), 0.5);
        assert_eq!(at(2, 5.0), 0.75, "clamped sequences hold their last frame");
        assert_eq!(at(9, 0.6), 0.25, "unknown sequences use the first");
    }

    #[test]
    fn fitting_the_lifetime_spans_the_whole_sequence() {
        let sheet = sheet();
        let at = |fraction| frame_rect(Some(&sheet), 1, 0, 99.0, fraction, 1.0, true).u0;
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(0.49), 0.0);
        assert_eq!(at(0.51), 0.25);
        assert_eq!(at(1.0), 0.25);
    }

    #[test]
    fn strips_step_through_equal_columns() {
        let at = |age, fit, fraction| frame_rect(None, 4, 0, age, fraction, 2.0, fit);
        assert_eq!(at(0.0, false, 0.0).u0, 0.0);
        assert_eq!(at(1.0, false, 0.0).u0, 0.5, "two frames a second");
        assert_eq!(at(100.0, false, 0.0).u1, 1.0, "holds the last");
        assert_eq!(at(0.0, true, 0.6).u0, 0.5);
        assert_eq!(at(0.0, true, 1.0).u1, 1.0);
    }

    #[test]
    fn a_trail_stretches_behind_the_head() {
        let q = trail(
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(5.0, 0.0, 0.0),
            2.0,
            (0.0, 100.0),
            0.5,
            Vec3::new(10.0, 0.0, 10.0),
            WHOLE,
            [1.0; 4],
        )
        .unwrap();
        assert!((q.corners[1].x - 10.0).abs() < 1e-5);
        assert!((q.corners[0].x - 8.0).abs() < 1e-5);
        assert!((q.corners[0] - q.corners[3]).length() - 1.0 < 1e-5);
        assert!(
            trail(
                Vec3::ZERO,
                Vec3::ZERO,
                1.0,
                (0.0, 5.0),
                1.0,
                Vec3::Z,
                WHOLE,
                [1.0; 4]
            )
            .is_none()
        );
    }

    #[test]
    fn trail_length_is_clamped() {
        let long = trail(
            Vec3::ZERO,
            Vec3::X,
            50.0,
            (0.0, 3.0),
            1.0,
            Vec3::Z,
            WHOLE,
            [1.0; 4],
        )
        .unwrap();
        assert!((long.corners[1].x - long.corners[0].x - 3.0).abs() < 1e-5);
    }

    #[test]
    fn a_rope_has_a_quad_per_gap_with_continuous_texture() {
        let point = |x: f32| RopePoint {
            position: Vec3::new(x, 0.0, 0.0),
            radius: 0.5,
            color: [1.0; 4],
        };
        let quads = rope(&[point(0.0), point(1.0), point(2.0)], Vec3::Z * 5.0, WHOLE);
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0].uvs[2][1], quads[1].uvs[0][1]);
        assert_eq!(quads[1].uvs[2][1], 1.0);
        assert_eq!(quads[0].corners[2], quads[1].corners[1], "shared edge");
        assert_eq!(quads[0].corners[3], quads[1].corners[0], "shared edge");
        assert!(rope(&[point(0.0)], Vec3::Z, WHOLE).is_empty());
    }
}
