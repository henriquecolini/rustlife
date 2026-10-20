//! Friction and acceleration: how input turns into velocity.
//!
//! Both ground and air acceleration add speed *along the wished direction*
//! only until the velocity's component along it reaches the wished speed.
//! On the ground that wished speed is the top speed. In the air it is capped
//! low (`air.speed_cap`, 30 units/s by default), but the acceleration is
//! still scaled by the full wished speed, so a player turning their view
//! while strafing keeps finding a direction in which their velocity
//! component is below the cap and keeps gaining speed — air-strafing. A jump
//! taken the instant a player lands skips ground friction entirely, so the
//! gain carries from hop to hop — bunny-hopping.

use glam::Vec3;

use super::{MovementWorld, Mover, flat_axes};

/// Speeds below this are treated as standing still, units/s.
const STOP_EPSILON: f32 = 0.1;
/// Walking slower than this snaps to a stop, units/s.
const WALK_STOP_SPEED: f32 = 1.0;

impl<W: MovementWorld> Mover<'_, W> {
    /// The horizontal wished direction and speed from the movement keys.
    pub(super) fn wish_flat(&self) -> (Vec3, f32) {
        let (forward, right) = flat_axes(self.cmd.yaw);
        let max_speed = self.max_speed();
        let forward_move = self.cmd.forward.clamp(-1.0, 1.0) * max_speed;
        let side_move = self.cmd.side.clamp(-1.0, 1.0) * max_speed;
        let wish = forward * forward_move + right * side_move;
        let speed = wish.length();
        if speed <= f32::EPSILON {
            return (Vec3::ZERO, 0.0);
        }
        (wish / speed, speed.min(max_speed))
    }

    /// Ground friction: removes a fraction of the speed every second, as if
    /// moving at least `stop_speed`, so slow sliding comes to a full stop.
    pub(super) fn friction(&mut self) {
        let speed = self.state.velocity.length();
        if speed < STOP_EPSILON {
            return;
        }
        let ground = &self.config.ground;
        let control = speed.max(ground.stop_speed);
        let drop = control * ground.friction * self.dt;
        let new_speed = (speed - drop).max(0.0);
        self.state.velocity *= new_speed / speed;
    }

    /// Adds speed towards `direction` until the velocity's component along
    /// it reaches `wish_speed`, at most `rate · wish_speed` per second.
    pub(super) fn accelerate(&mut self, direction: Vec3, wish_speed: f32, rate: f32) {
        let current = self.state.velocity.dot(direction);
        let add = wish_speed - current;
        if add <= 0.0 {
            return;
        }
        let gain = (rate * self.dt * wish_speed).min(add);
        self.state.velocity += direction * gain;
    }

    /// Air acceleration: like [`accelerate`](Self::accelerate), but the
    /// speed sought along `direction` is capped at `air.speed_cap` while the
    /// gain per tick still scales with the full wished speed.
    pub(super) fn air_accelerate(&mut self, direction: Vec3, wish_speed: f32) {
        let air = &self.config.air;
        let capped = wish_speed.min(air.speed_cap);
        let current = self.state.velocity.dot(direction);
        let add = capped - current;
        if add <= 0.0 {
            return;
        }
        let gain = (air.accelerate * wish_speed * self.dt).min(add);
        self.state.velocity += direction * gain;
    }

    /// Moving on a floor: accelerate, cap at top speed, then move — stepping
    /// up ledges if blocked — and stay glued to slopes going down.
    pub(super) fn walk_move(&mut self) {
        let (direction, wish_speed) = self.wish_flat();
        self.state.velocity.z = 0.0;
        self.accelerate(direction, wish_speed, self.config.ground.accelerate);
        self.state.velocity.z = 0.0;

        let max_speed = self.max_speed();
        let speed = self.state.velocity.length();
        if speed > max_speed {
            self.state.velocity *= max_speed / speed;
        }
        if self.state.velocity.length() < WALK_STOP_SPEED {
            self.state.velocity = Vec3::ZERO;
            return;
        }

        let destination = self.state.origin + self.state.velocity * self.dt;
        let trace = self.trace(self.state.origin, destination);
        if !trace.hit() && !trace.start_solid {
            self.state.origin = trace.end;
        } else {
            self.step_move();
        }
        self.stay_on_ground();
    }

    /// Moving through the air: capped air acceleration, then slide.
    pub(super) fn air_move(&mut self) {
        let (direction, wish_speed) = self.wish_flat();
        self.air_accelerate(direction, wish_speed);
        self.slide_move();
    }
}
