//! Swimming: moving at least waist-deep in water.
//!
//! In water there is no gravity and no floor. The player swims in the full
//! 3D direction of their view, slowed by `water.speed_scale`, held back by
//! water friction, and drifts down at `water.sink_speed` without input. The
//! jump button swims straight up; once the waist clears the surface,
//! ordinary movement takes over and a jump from the surface climbs out (see
//! `check_jump`).

use glam::Vec3;

use super::{Buttons, MovementWorld, Mover, view_axes};

impl<W: MovementWorld> Mover<'_, W> {
    /// One tick of swimming.
    pub(super) fn water_move(&mut self) {
        let (forward, right) = view_axes(self.cmd.pitch, self.cmd.yaw);
        let max_speed = self.max_speed();
        let mut wish = forward * (self.cmd.forward.clamp(-1.0, 1.0) * max_speed)
            + right * (self.cmd.side.clamp(-1.0, 1.0) * max_speed);
        if self.cmd.buttons.contains(Buttons::JUMP) {
            wish.z += max_speed;
        } else if self.cmd.forward == 0.0 && self.cmd.side == 0.0 {
            wish.z -= self.config.water.sink_speed;
        }
        let wish_speed = wish.length().min(max_speed) * self.config.water.speed_scale;
        let direction = wish.normalize_or_zero();

        // Water friction.
        let speed = self.state.velocity.length();
        let water = &self.config.water;
        let new_speed = if speed > 0.0 {
            let new_speed = (speed - self.dt * speed * water.friction).max(0.0);
            self.state.velocity *= new_speed / speed;
            new_speed
        } else {
            0.0
        };

        // Accelerate towards the wished velocity.
        if wish_speed >= 0.1 {
            let add = wish_speed - new_speed;
            if add > 0.0 {
                let gain = (water.accelerate * wish_speed * self.dt).min(add);
                self.state.velocity += direction * gain;
            }
        }

        self.leave_ground();
        if self.state.velocity == Vec3::ZERO {
            return;
        }
        self.slide_move();
    }
}
