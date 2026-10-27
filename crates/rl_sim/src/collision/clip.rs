//! Sweeping a box against one convex volume.
//!
//! A convex volume is the intersection of half-spaces `n · p <= d`. Sweeping
//! an axis-aligned box of half-size `e` through it is equivalent to sweeping
//! the box's *centre point* through the volume grown by the box: each plane
//! moves out by the box's support distance along its normal,
//! `d' = d + |n.x|·e.x + |n.y|·e.y + |n.z|·e.z`. That is exact for every plane
//! the volume actually has; the corners and edges of the grown volume are
//! rounded off by the missing planes, which is why compiled brushes carry
//! *bevel* planes, and why displacement triangles are given theirs (see
//! [`super::triangle`]).
//!
//! The segment enters the grown volume at the largest "entering" crossing and
//! leaves at the smallest "leaving" crossing; it hits the volume when it
//! enters before it leaves. Crossings are pulled back by [`DIST_EPSILON`], so
//! a trace stops just short of the surface instead of exactly on it, which
//! keeps the next trace from starting inside the volume through rounding.
//! This is the classic Quake / Source brush clip (`CM_ClipBoxToBrush`).

use glam::Vec3;

/// How far short of a surface a trace stops, in units. Source's value: 1/32.
pub const DIST_EPSILON: f32 = 0.031_25;

/// "No entering plane found yet"; any real entering fraction is larger.
const NEVER_ENTERED: f32 = -1.0;

/// A box swept from `start` to `end` (its centre), half-size `extents`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sweep {
    pub(crate) start: Vec3,
    pub(crate) end: Vec3,
    pub(crate) extents: Vec3,
    /// Zero extents: a ray. Rays skip bevel planes.
    pub(crate) is_point: bool,
    /// Contents that block the sweep.
    pub(crate) mask: u32,
}

impl Sweep {
    /// Builds a sweep, deciding whether it is a ray.
    pub(crate) fn new(start: Vec3, end: Vec3, extents: Vec3, mask: u32) -> Self {
        Self {
            start,
            end,
            extents,
            is_point: extents == Vec3::ZERO,
            mask,
        }
    }

    /// Distance the box reaches past its centre along `normal`.
    pub(crate) fn support(&self, normal: Vec3) -> f32 {
        if self.is_point {
            0.0
        } else {
            normal.abs().dot(self.extents)
        }
    }

    /// The axis-aligned bounds of everything the swept box touches, grown by
    /// `margin`.
    pub(crate) fn bounds(&self, margin: f32) -> (Vec3, Vec3) {
        let grow = self.extents + Vec3::splat(margin);
        (
            self.start.min(self.end) - grow,
            self.start.max(self.end) + grow,
        )
    }
}

/// The nearest hit found so far by a sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Hit {
    /// Fraction of the sweep completed before the hit, `0..=1`.
    pub(crate) fraction: f32,
    /// Normal of the plane hit (zero if nothing was hit).
    pub(crate) normal: Vec3,
    /// Contents of the volume hit.
    pub(crate) contents: u32,
    /// The material of the plane hit, as the map's collision model indexes
    /// it.
    pub(crate) material: Option<u16>,
    /// The sweep started inside some volume.
    pub(crate) start_solid: bool,
    /// The sweep lies entirely inside some volume.
    pub(crate) all_solid: bool,
}

impl Default for Hit {
    fn default() -> Self {
        Self {
            fraction: 1.0,
            normal: Vec3::ZERO,
            contents: 0,
            material: None,
            start_solid: false,
            all_solid: false,
        }
    }
}

impl Hit {
    /// Keeps the nearer of `self` and `other`, accumulating the solid flags.
    pub(crate) fn merge(&mut self, other: Hit) {
        self.start_solid |= other.start_solid;
        self.all_solid |= other.all_solid;
        if other.fraction < self.fraction {
            self.fraction = other.fraction;
            self.normal = other.normal;
            self.contents = other.contents;
            self.material = other.material;
        }
    }
}

/// One plane of a convex volume: the volume lies behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ClipPlane {
    pub(crate) normal: Vec3,
    pub(crate) dist: f32,
    /// Bevels only exist for box sweeps; rays ignore them.
    pub(crate) bevel: bool,
    /// The plane's surface material, when it belongs to one.
    pub(crate) material: Option<u16>,
}

/// Clips `sweep` against the convex volume bounded by `planes`, updating
/// `hit` if the volume is hit nearer than anything before.
pub(crate) fn clip_convex(
    sweep: &Sweep,
    planes: impl IntoIterator<Item = ClipPlane>,
    contents: u32,
    hit: &mut Hit,
) {
    let mut enter = NEVER_ENTERED;
    let mut leave = 1.0f32;
    let mut enter_normal = Vec3::ZERO;
    let mut enter_material = None;
    let mut starts_out = false;
    let mut ends_out = false;

    for plane in planes {
        if plane.bevel && sweep.is_point {
            continue;
        }
        let dist = plane.dist + sweep.support(plane.normal);
        let d1 = plane.normal.dot(sweep.start) - dist;
        let d2 = plane.normal.dot(sweep.end) - dist;
        if d2 > 0.0 {
            ends_out = true;
        }
        // Resting exactly on a plane is outside it: touching is not
        // penetrating.
        if d1 >= 0.0 {
            starts_out = true;
        }
        // Starts in front of this plane and stays there (or gets farther
        // away): the segment never enters the volume.
        if d1 > 0.0 && (d2 >= DIST_EPSILON || d2 >= d1) {
            return;
        }
        // Wholly behind this plane: it does not limit the segment. Resting
        // exactly on it is not behind it, so a move into the face is stopped.
        if d1 < 0.0 && d2 < 0.0 {
            continue;
        }
        if d1 > d2 {
            // A start closer to the plane than the epsilon would pull the
            // crossing back past the start; over a short move that gives a
            // large negative fraction, the hit would be discarded and the
            // box would end inside. Clamp it to "hit at the start" instead
            // (as the Source engine does).
            let fraction = (d1 - DIST_EPSILON).max(0.0) / (d1 - d2);
            if fraction > enter {
                enter = fraction;
                enter_normal = plane.normal;
                enter_material = plane.material;
            }
        } else {
            let fraction = (d1 + DIST_EPSILON) / (d1 - d2);
            leave = leave.min(fraction);
        }
    }

    if !starts_out {
        hit.start_solid = true;
        if !ends_out {
            hit.all_solid = true;
            hit.fraction = 0.0;
            hit.contents = contents;
        }
        return;
    }
    if enter < leave && enter > NEVER_ENTERED && enter < hit.fraction {
        hit.fraction = enter.max(0.0);
        hit.normal = enter_normal;
        hit.contents = contents;
        hit.material = enter_material;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cube `-1..=1` on each axis.
    fn unit_cube() -> Vec<ClipPlane> {
        [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::Z,
            Vec3::NEG_Z,
        ]
        .into_iter()
        .map(|normal| ClipPlane {
            normal,
            dist: 1.0,
            bevel: false,
            material: None,
        })
        .collect()
    }

    fn sweep(start: Vec3, end: Vec3, extents: f32) -> Hit {
        let mut hit = Hit::default();
        let sweep = Sweep::new(start, end, Vec3::splat(extents), u32::MAX);
        clip_convex(&sweep, unit_cube(), 1, &mut hit);
        hit
    }

    #[test]
    fn a_ray_stops_just_short_of_the_face() {
        let hit = sweep(Vec3::new(-11.0, 0.0, 0.0), Vec3::new(9.0, 0.0, 0.0), 0.0);
        // Face at x = -1, 10 units in, pulled back by the epsilon.
        assert!((hit.fraction - (10.0 - DIST_EPSILON) / 20.0).abs() < 1e-6);
        assert_eq!(hit.normal, Vec3::NEG_X);
        assert!(!hit.start_solid);
    }

    #[test]
    fn a_box_hits_earlier_by_its_extent() {
        let hit = sweep(Vec3::new(-11.0, 0.0, 0.0), Vec3::new(9.0, 0.0, 0.0), 2.0);
        assert!((hit.fraction - (8.0 - DIST_EPSILON) / 20.0).abs() < 1e-6);
    }

    #[test]
    fn a_hit_carries_the_material_of_the_plane_entered() {
        let mut planes = unit_cube();
        for plane in &mut planes {
            if plane.normal == Vec3::NEG_X {
                plane.material = Some(7);
            }
        }
        let mut hit = Hit::default();
        let sweep = Sweep::new(
            Vec3::new(-11.0, 0.0, 0.0),
            Vec3::new(9.0, 0.0, 0.0),
            Vec3::ZERO,
            1,
        );
        clip_convex(&sweep, planes, 1, &mut hit);
        assert_eq!((hit.fraction < 1.0, hit.material), (true, Some(7)));
    }

    #[test]
    fn misses_and_moving_away_leave_the_hit_alone() {
        let miss = sweep(Vec3::new(-11.0, 5.0, 0.0), Vec3::new(9.0, 5.0, 0.0), 0.0);
        assert_eq!(miss, Hit::default());
        let away = sweep(Vec3::new(-2.0, 0.0, 0.0), Vec3::new(-10.0, 0.0, 0.0), 0.0);
        assert_eq!(away, Hit::default());
    }

    #[test]
    fn starting_inside_is_reported() {
        let leaving = sweep(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), 0.0);
        assert!(leaving.start_solid && !leaving.all_solid);
        let stuck = sweep(Vec3::ZERO, Vec3::new(0.5, 0.0, 0.0), 0.0);
        assert!(stuck.all_solid);
        assert_eq!(stuck.fraction, 0.0);
    }

    #[test]
    fn resting_exactly_on_a_face_is_outside_it() {
        // A box whose side lies exactly on the cube's face (centre 2 from the
        // origin, half-extent 1, face at 1): not stuck, and blocked moving in.
        let resting = Vec3::new(-2.0, 0.0, 0.0);
        let still = sweep(resting, resting, 1.0);
        assert!(!still.start_solid && still.fraction >= 1.0);
        let into = sweep(resting, resting + Vec3::new(0.5, 0.0, 0.0), 1.0);
        assert!(!into.start_solid);
        assert!(into.fraction.abs() < f32::EPSILON, "{into:?}");
    }

    #[test]
    fn a_box_already_within_the_epsilon_still_hits() {
        // 0.004 units from the face, moving 0.02 units into it: the pulled-
        // back crossing lies far behind the start, but this is still a hit.
        let start = Vec3::new(-1.004, 0.0, 0.0);
        let hit = sweep(start, start + Vec3::new(0.02, 0.0, 0.0), 0.0);
        assert_eq!(hit.fraction, 0.0);
        assert_eq!(hit.normal, Vec3::NEG_X);
    }

    #[test]
    fn rays_ignore_bevels() {
        let mut planes = unit_cube();
        // A bevel that would cut the cube's +X face off at x = -5.
        planes.push(ClipPlane {
            normal: Vec3::X,
            dist: -5.0,
            bevel: true,
            material: None,
        });
        let mut hit = Hit::default();
        let ray = Sweep::new(Vec3::new(-11.0, 0.0, 0.0), Vec3::ZERO, Vec3::ZERO, 1);
        clip_convex(&ray, planes, 1, &mut hit);
        assert!(hit.fraction < 1.0, "the ray still hits the cube");
    }

    #[test]
    fn merge_keeps_the_nearest_hit() {
        let mut best = Hit {
            fraction: 0.5,
            ..Hit::default()
        };
        best.merge(Hit {
            fraction: 0.7,
            start_solid: true,
            ..Hit::default()
        });
        assert_eq!(best.fraction, 0.5);
        assert!(best.start_solid);
        best.merge(Hit {
            fraction: 0.2,
            normal: Vec3::Z,
            ..Hit::default()
        });
        assert_eq!((best.fraction, best.normal), (0.2, Vec3::Z));
    }
}
