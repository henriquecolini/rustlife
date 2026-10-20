//! Crouching.
//!
//! On the ground, crouching takes `duck.duck_time`: the view lowers over
//! that time and the hull shrinks to the ducked hull only at the end.
//! Standing back up is only possible where the standing hull fits — under a
//! low ceiling the player stays down — and then the hull grows back at once
//! while the view rises over `duck.unduck_time`.
//!
//! In the air, crouching is instant and lifts the feet rather than lowering
//! the head: the origin rises by the difference in hull heights. That is what
//! lets a crouch-jump clear ledges a plain jump cannot. Standing up in the air
//! lowers the feet back down, if there is room for them.

use glam::Vec3;

use super::{Buttons, MovementWorld, Mover};
use crate::collision::Hull;
use crate::collision::contents::MASK_PLAYER_SOLID;

impl<W: MovementWorld> Mover<'_, W> {
    /// Advances crouching or standing up by one tick.
    pub(super) fn update_duck(&mut self) {
        if self.state.ladder.is_some() {
            return;
        }
        let duck = &self.config.duck;
        let held = self.cmd.buttons.contains(Buttons::DUCK);
        if held {
            if self.state.ducked {
                self.state.duck_amount = 1.0;
            } else if self.state.on_ground {
                self.state.duck_amount += self.dt / duck.duck_time;
                if self.state.duck_amount >= 1.0 {
                    self.finish_duck(false);
                }
            } else {
                self.finish_duck(true);
            }
        } else if self.state.ducked {
            self.try_unduck();
        } else {
            self.state.duck_amount = (self.state.duck_amount - self.dt / duck.unduck_time).max(0.0);
        }
    }

    /// Switches to the ducked hull. In the air the feet are lifted so the
    /// head stays put.
    fn finish_duck(&mut self, in_air: bool) {
        if in_air {
            self.state.origin += Vec3::Z * self.hull_height_difference();
        }
        self.state.ducked = true;
        self.state.duck_amount = 1.0;
    }

    /// Stands up over `unduck_time` on the ground, or at once in the air,
    /// provided the standing hull fits where it would be.
    fn try_unduck(&mut self) {
        let in_air = !self.state.on_ground;
        let standing_origin = if in_air {
            self.state.origin - Vec3::Z * self.hull_height_difference()
        } else {
            self.state.origin
        };
        let standing = Hull::from(&self.config.hulls.standing);
        let fits = !self
            .world
            .trace_hull(
                standing_origin,
                standing_origin,
                &standing,
                MASK_PLAYER_SOLID,
            )
            .start_solid;
        if !fits {
            self.state.duck_amount = 1.0;
            return;
        }
        if in_air {
            self.state.origin = standing_origin;
            self.state.ducked = false;
            self.state.duck_amount = 0.0;
            return;
        }
        // The hull swaps back as soon as standing starts; the view rises
        // over `unduck_time`.
        self.state.ducked = false;
        self.state.duck_amount = (1.0 - self.dt / self.config.duck.unduck_time).max(0.0);
    }

    /// How much taller the standing hull is than the ducked one.
    fn hull_height_difference(&self) -> f32 {
        let hulls = &self.config.hulls;
        (hulls.standing.maxs[2] - hulls.standing.mins[2])
            - (hulls.ducked.maxs[2] - hulls.ducked.mins[2])
    }
}
