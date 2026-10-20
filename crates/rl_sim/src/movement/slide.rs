//! Moving through the world: sliding along what is hit, stepping up ledges
//! and sticking to slopes.
//!
//! [`Mover::slide_move`] is the heart of it (Source's `TryPlayerMove`): move
//! as far as possible along the velocity; on a hit, remove the velocity
//! component into the surface (`clip_velocity`) and spend the remaining time
//! moving along it, up to four times per tick. Against two surfaces at once
//! the only way forward is along their crease. Because a steep ramp is never
//! a floor, a player in the air against one keeps sliding along it with
//! gravity merely bending the path — that is surfing, with no special code.

use glam::Vec3;

use super::{MovementWorld, Mover};
use crate::collision::DIST_EPSILON;

/// Most surfaces one tick's move can bounce off.
const MAX_BUMPS: usize = 4;
/// Most distinct planes remembered while resolving one tick's move.
const MAX_CLIP_PLANES: usize = 5;
/// Overbounce used when sliding along surfaces: a hair over 1, so the
/// clipped velocity leaves the surface by a fraction of a unit per second.
///
/// With exactly 1 the result is parallel to the surface only up to rounding,
/// and can point a sliver *into* it. A hull resting within the clip epsilon
/// of a slope then hits it at fraction 0 on every bump and is stopped dead
/// mid-slide — the "ramp bug" that plagues Source-engine surfing. The extra
/// thousandth moves the player away from a surface by under a unit per
/// second of contact, which is invisible.
const SLIDE_OVERBOUNCE: f32 = 1.001;
/// A snap to the ground smaller than this is not worth doing, units.
const MIN_GROUND_SNAP: f32 = 0.5 * DIST_EPSILON;

/// Removes the part of `velocity` going into a surface with unit `normal`,
/// scaled by `overbounce` (`1` slides exactly along it; more bounces off).
#[must_use]
pub(crate) fn clip_velocity(velocity: Vec3, normal: Vec3, overbounce: f32) -> Vec3 {
    let backoff = velocity.dot(normal) * overbounce;
    let mut clipped = velocity - normal * backoff;
    // Rounding can leave a sliver of velocity into the surface; remove it so
    // the next trace does not start inside.
    let adjust = clipped.dot(normal);
    if adjust < 0.0 {
        clipped -= normal * adjust;
    }
    clipped
}

impl<W: MovementWorld> Mover<'_, W> {
    /// Moves along the velocity for the tick, sliding along every surface
    /// hit (Source's `TryPlayerMove`).
    pub(super) fn slide_move(&mut self) {
        let primal_velocity = self.state.velocity;
        let mut original_velocity = self.state.velocity;
        let mut planes = [Vec3::ZERO; MAX_CLIP_PLANES];
        let mut plane_count = 0;
        let mut time_left = self.dt;
        let mut total_fraction = 0.0;

        for _ in 0..MAX_BUMPS {
            if self.state.velocity == Vec3::ZERO {
                break;
            }
            let end = self.state.origin + self.state.velocity * time_left;
            let trace = self.trace(self.state.origin, end);
            total_fraction += trace.fraction;
            if trace.all_solid {
                // Stuck inside something: going nowhere.
                self.state.velocity = Vec3::ZERO;
                return;
            }
            if trace.fraction > 0.0 {
                // Moved some distance: forget the planes hit before it.
                self.state.origin = trace.end;
                original_velocity = self.state.velocity;
                plane_count = 0;
            }
            if !trace.hit() {
                break;
            }
            time_left -= time_left * trace.fraction;
            if plane_count >= MAX_CLIP_PLANES {
                self.state.velocity = Vec3::ZERO;
                break;
            }
            planes[plane_count] = trace.normal;
            plane_count += 1;

            if plane_count == 1 && !self.state.on_ground {
                // In the air against one surface: slide along it.
                self.state.velocity = clip_velocity(original_velocity, planes[0], SLIDE_OVERBOUNCE);
                original_velocity = self.state.velocity;
            } else if let Some(velocity) = clip_to_all(original_velocity, &planes[..plane_count]) {
                self.state.velocity = velocity;
            } else if plane_count == 2 {
                // Two surfaces and no single clip satisfies both: go along
                // their crease.
                let crease = planes[0].cross(planes[1]).normalize_or_zero();
                self.state.velocity = crease * crease.dot(self.state.velocity);
            } else {
                // Boxed in a corner of three or more surfaces.
                self.state.velocity = Vec3::ZERO;
                break;
            }

            // Turned back on itself: stop dead rather than jitter in
            // corners.
            if self.state.velocity.dot(primal_velocity) <= 0.0 {
                self.state.velocity = Vec3::ZERO;
                break;
            }
        }
        if total_fraction == 0.0 {
            self.state.velocity = Vec3::ZERO;
        }
    }

    /// Walking into something: tries the move both as-is and after lifting
    /// by the step size (then dropping back down), and keeps whichever got
    /// farther horizontally. A ledge no higher than the step is climbed;
    /// anything taller just blocks.
    pub(super) fn step_move(&mut self) {
        let start = self.state;
        let step = self.config.ground.step_size;

        // Plain slide.
        self.slide_move();
        let down = self.state;

        // Stepped slide: up, across, back down.
        self.state = start;
        let up = self.trace(self.state.origin, self.state.origin + Vec3::Z * step);
        if !up.start_solid && !up.all_solid {
            self.state.origin = up.end;
        }
        self.slide_move();
        let down_trace = self.trace(
            self.state.origin,
            self.state.origin - Vec3::Z * (step + DIST_EPSILON),
        );
        if !down_trace.start_solid && !down_trace.all_solid {
            self.state.origin = down_trace.end;
        }

        // Not standing on a floor after stepping (e.g. stepped onto a steep
        // slope or into the air): the plain slide was right.
        if down_trace.normal.z < self.config.ground.floor_normal_z {
            self.state = down;
            return;
        }

        let distance =
            |state: &super::PlayerState| (state.origin - start.origin).truncate().length_squared();
        if distance(&down) > distance(&self.state) {
            self.state = down;
        } else {
            // Keep the stepped position but the plain slide's vertical
            // speed, so stepping never launches the player.
            self.state.velocity.z = down.velocity.z;
        }
    }

    /// Walking down a slope or stairs: if a floor lies within a step below,
    /// move down onto it so the player stays grounded instead of skipping
    /// down in small falls.
    pub(super) fn stay_on_ground(&mut self) {
        let origin = self.state.origin;
        let lifted = self.trace(origin, origin + Vec3::Z * 2.0).end;
        let below = self.trace(lifted, origin - Vec3::Z * self.config.ground.step_size);
        if below.hit()
            && below.fraction > 0.0
            && !below.start_solid
            && below.normal.z >= self.config.ground.floor_normal_z
            && (below.end.z - origin.z).abs() > MIN_GROUND_SNAP
        {
            self.state.origin = below.end;
        }
    }
}

/// Clips `velocity` against each plane in turn and returns the first clip
/// that does not move into any of the planes, if one does.
fn clip_to_all(velocity: Vec3, planes: &[Vec3]) -> Option<Vec3> {
    planes.iter().enumerate().find_map(|(i, &plane)| {
        let clipped = clip_velocity(velocity, plane, SLIDE_OVERBOUNCE);
        let into_another = planes
            .iter()
            .enumerate()
            .any(|(j, &other)| j != i && clipped.dot(other) < 0.0);
        (!into_another).then_some(clipped)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_removes_the_component_into_the_surface() {
        let clipped = clip_velocity(Vec3::new(100.0, 0.0, -50.0), Vec3::Z, 1.0);
        assert_eq!(clipped, Vec3::new(100.0, 0.0, 0.0));
        let ramp = Vec3::new(-1.0, 0.0, 1.0).normalize();
        let along = clip_velocity(Vec3::new(300.0, 0.0, 0.0), ramp, 1.0);
        assert!(along.dot(ramp).abs() < 1e-4);
        assert!(along.z > 0.0, "running into a ramp slides up it");
    }

    #[test]
    fn a_crease_clip_is_found_when_one_exists() {
        let floor = Vec3::Z;
        let wall = Vec3::NEG_X;
        // Moving into the wall and down: clipping against the floor still
        // goes into the wall, but clipping against the wall goes into the
        // floor; neither alone satisfies both.
        assert_eq!(
            clip_to_all(Vec3::new(10.0, 5.0, -10.0), &[floor, wall]),
            None
        );
        // Moving along the wall and down: the floor clip works.
        let along = clip_to_all(Vec3::new(0.0, 5.0, -10.0), &[floor, wall]).expect("clip");
        assert!(
            along.abs_diff_eq(Vec3::new(0.0, 5.0, 0.01), 1e-4),
            "{along}"
        );
    }
}
