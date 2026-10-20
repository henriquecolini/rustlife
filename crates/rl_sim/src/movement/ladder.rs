//! Ladder climbing.
//!
//! Ladders in HL2-era maps are segments between two feet positions
//! (`func_useableladder`), not brushes. rustlife's rules for them:
//!
//! - **Grab** with the use key within `ladder.mount_distance` of the
//!   ladder's axis (and no more than a step beyond either end); the player
//!   snaps onto the axis if there is room.
//! - **Climb** with forward/back at `ladder.climb_speed`. Forward climbs up,
//!   unless the player looks more than [`LOOK_DOWN_PITCH`] down, which makes
//!   forward climb down. There is no gravity on a ladder.
//! - **Let go** with use: the player steps onto the nearest free dismount
//!   point (`info_ladder_dismount`) within `ladder.dismount_range`, or simply
//!   drops where they are. Climbing past the top does the same, falling back
//!   to a step forward when there is no dismount point; climbing past the
//!   bottom lets go there.
//! - **Jump off** with jump: a push backwards, away from the ladder the
//!   player is facing, at `ladder.jump_off_speed`.

use glam::Vec3;

use super::{Buttons, MovementWorld, Mover, flat_axes};
use crate::collision::Ladder;

/// Looking down by more than this many degrees reverses forward climbing.
const LOOK_DOWN_PITCH: f32 = 45.0;
/// Within this distance of an end, units, the climber has reached it.
const END_EPSILON: f32 = 0.5;

impl<W: MovementWorld> Mover<'_, W> {
    /// Handles a tick on (or mounting) a ladder. Returns `true` if the tick
    /// was spent on the ladder, `false` to move normally.
    pub(super) fn ladder_tick(&mut self) -> bool {
        let use_pressed = self.cmd.buttons.contains(Buttons::USE)
            && !self.state.old_buttons.contains(Buttons::USE);
        let jump = self.cmd.buttons.contains(Buttons::JUMP);
        let jump_pressed = jump && !self.jump_held();
        let ladders = self.world.ladders();

        let Some(index) = self.state.ladder else {
            if use_pressed && let Some((index, position)) = self.find_ladder() {
                self.state.ladder = Some(index);
                self.state.origin = position;
                self.state.velocity = Vec3::ZERO;
                self.leave_ground();
                self.set_jump_held(jump);
                return true;
            }
            return false;
        };
        let Some(&ladder) = ladders.get(usize::from(index)) else {
            // The map changed under us: let go.
            self.state.ladder = None;
            return false;
        };

        self.set_jump_held(jump);
        if use_pressed {
            self.dismount(false);
        } else if jump_pressed {
            self.jump_off();
        } else {
            self.climb(&ladder);
        }
        true
    }

    /// The nearest ladder within reach and the position on its axis to snap
    /// to, if one is in reach and the hull fits there.
    fn find_ladder(&self) -> Option<(u16, Vec3)> {
        let origin = self.state.origin;
        let reach = self.config.ground.step_size;
        let mut best: Option<(u16, Vec3, f32)> = None;
        for (index, ladder) in self.world.ladders().iter().enumerate() {
            if origin.z < ladder.bottom.z - reach || origin.z > ladder.top.z + reach {
                continue;
            }
            let position = point_at_height(ladder, origin.z);
            let distance = (position - origin).truncate().length();
            if distance > self.config.ladder.mount_distance
                || best.is_some_and(|(_, _, nearest)| distance >= nearest)
            {
                continue;
            }
            let Ok(index) = u16::try_from(index) else {
                continue;
            };
            if !self.trace(position, position).start_solid {
                best = Some((index, position, distance));
            }
        }
        best.map(|(index, position, _)| (index, position))
    }

    /// Moves along the ladder by one tick.
    fn climb(&mut self, ladder: &Ladder) {
        let direction = if self.cmd.pitch > LOOK_DOWN_PITCH {
            -1.0
        } else {
            1.0
        };
        let climb = self.cmd.forward.clamp(-1.0, 1.0) * direction * self.config.ladder.climb_speed;
        let target_z = (self.state.origin.z + climb * self.dt).clamp(ladder.bottom.z, ladder.top.z);
        let start = self.state.origin;
        let trace = self.trace(start, point_at_height(ladder, target_z));
        self.state.origin = trace.end;
        self.state.velocity = (trace.end - start) / self.dt;
        self.leave_ground();

        if climb > 0.0 && self.state.origin.z >= ladder.top.z - END_EPSILON {
            self.dismount(true);
        } else if climb < 0.0 && self.state.origin.z <= ladder.bottom.z + END_EPSILON {
            self.state.ladder = None;
            self.state.velocity = Vec3::ZERO;
        }
    }

    /// Lets go of the ladder, stepping onto the nearest free dismount point
    /// in range. At the top, with no such point, the player steps forward
    /// instead.
    fn dismount(&mut self, at_top: bool) {
        self.state.ladder = None;
        self.state.velocity = Vec3::ZERO;
        let origin = self.state.origin;
        let range = self.config.ladder.dismount_range;
        let mut best: Option<(Vec3, f32)> = None;
        for &point in self.world.ladder_dismounts() {
            let distance = (point - origin).length();
            if distance > range || best.is_some_and(|(_, nearest)| distance >= nearest) {
                continue;
            }
            if !self.trace(point, point).start_solid {
                best = Some((point, distance));
            }
        }
        if let Some((point, _)) = best {
            self.state.origin = point;
        } else if at_top {
            let (forward, _) = flat_axes(self.cmd.yaw);
            self.state.velocity = forward * self.config.ladder.climb_speed;
        }
    }

    /// Jumps backwards off the ladder.
    fn jump_off(&mut self) {
        let (forward, _) = flat_axes(self.cmd.yaw);
        self.state.ladder = None;
        self.state.velocity = -forward * self.config.ladder.jump_off_speed;
        self.events.jumped = true;
    }
}

/// The point on the ladder's axis at height `z` (clamped to its ends).
fn point_at_height(ladder: &Ladder, z: f32) -> Vec3 {
    let span = ladder.top.z - ladder.bottom.z;
    if span <= f32::EPSILON {
        return ladder.bottom;
    }
    let t = ((z - ladder.bottom.z) / span).clamp(0.0, 1.0);
    ladder.bottom.lerp(ladder.top, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slanted_ladders_are_followed_along_their_axis() {
        let ladder = Ladder {
            bottom: Vec3::new(0.0, 0.0, 0.0),
            top: Vec3::new(10.0, 0.0, 100.0),
        };
        assert_eq!(point_at_height(&ladder, 50.0), Vec3::new(5.0, 0.0, 50.0));
        assert_eq!(point_at_height(&ladder, 500.0), ladder.top);
        assert_eq!(point_at_height(&ladder, -5.0), ladder.bottom);
    }
}
