//! Where the player is: on a floor or in the air, and how deep in water.

use glam::Vec3;

use super::{MovementWorld, Mover, NON_JUMP_VELOCITY, WaterLevel};
use crate::collision::contents::MASK_WATER;

/// How far below the feet a floor still counts as stood on, units.
const GROUND_PROBE: f32 = 2.0;

impl<W: MovementWorld> Mover<'_, W> {
    /// Updates the water level, then looks for a floor just below the feet:
    /// a walkable surface (normal Z at least `floor_normal_z`) within
    /// [`GROUND_PROBE`] units. A player rising fast is never on the ground,
    /// so a jump is not cancelled by the floor it leaves.
    pub(super) fn categorize_position(&mut self) {
        self.update_water_level();
        if self.state.velocity.z > NON_JUMP_VELOCITY || self.state.ladder.is_some() {
            self.leave_ground();
            return;
        }
        let origin = self.state.origin;
        let trace = self.trace(origin, origin - Vec3::Z * GROUND_PROBE);
        if trace.hit() && trace.normal.z >= self.config.ground.floor_normal_z {
            self.state.on_ground = true;
            self.state.ground_normal = trace.normal;
        } else {
            self.leave_ground();
        }
    }

    pub(super) fn leave_ground(&mut self) {
        self.state.on_ground = false;
        self.state.ground_normal = Vec3::ZERO;
    }

    /// Water depth from the contents at the feet, the waist and the eyes.
    pub(super) fn update_water_level(&mut self) {
        let hull = self.hull();
        let origin = self.state.origin;
        let in_water =
            |height: f32| self.world.point_contents(origin + Vec3::Z * height) & MASK_WATER != 0;
        self.state.water_level = if !in_water(hull.mins.z + 1.0) {
            WaterLevel::None
        } else if !in_water(f32::midpoint(hull.mins.z, hull.maxs.z)) {
            WaterLevel::Feet
        } else if !in_water(self.state.eye_height(self.config)) {
            WaterLevel::Waist
        } else {
            WaterLevel::Eyes
        };
    }
}
