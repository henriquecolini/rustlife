//! Where a sound is heard from: Source's `Device_SpatializeChannel` for
//! two output channels.
//!
//! The direction to a sound, in the listener's frame, gives an angle round
//! the listener: 0° to the right, 90° ahead, 180° to the left, 270°
//! behind. Each output speaker sits at an angle and hears a sound the less
//! the further round the sound is: `1 - (difference / width)^1.5`.
//!
//! - Headphones put one speaker straight out of each ear (180° and 0°), so
//!   a sound ahead or behind is in both ears alike and one to a side in
//!   that ear only.
//! - Stereo speakers (HL2DM's default) mix four virtual speakers down to
//!   two: front ones at 45° and 135° heard out to 90° either side, rear
//!   ones at 225° and 315° added at three quarters. Sounds behind are
//!   quieter than sounds ahead.
//!
//! A sound far above or below the listener drifts towards the middle (it
//! is mixed towards a level 0.9 in every speaker from 45° of elevation up
//! to straight overhead), and an omnidirectional one (`@`) is there
//! already.

use glam::Vec3;

/// How the player listens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Speakers {
    /// A pair of speakers in front (HL2DM's default).
    #[default]
    Stereo,
    /// Headphones.
    Headphones,
}

/// Level every speaker heads towards for a sound mixed to mono.
const MONO_LEVEL: f32 = 0.9;
/// Share of a rear virtual speaker in the front one of its side.
const REAR_SHARE: f32 = 0.75;
/// Exponent of the fall-off between speakers.
const PAN_EXPONENT: f32 = 1.5;

/// The listener's frame, all unit vectors in the caller's space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Where the listener faces.
    pub forward: Vec3,
    /// The listener's right.
    pub right: Vec3,
    /// The listener's up.
    pub up: Vec3,
}

impl Default for Frame {
    fn default() -> Self {
        // Source's axes: x forward, y left, z up.
        Self {
            forward: Vec3::X,
            right: Vec3::NEG_Y,
            up: Vec3::Z,
        }
    }
}

/// The level of a sound in the (left, right) speakers, `0..=1`, for a
/// sound in `direction` (towards it from the listener; any length) and
/// `mono` (`0..=1`) of it mixed to the middle.
#[must_use]
pub fn stereo(frame: &Frame, direction: Vec3, mono: f32, speakers: Speakers) -> [f32; 2] {
    let side = direction.dot(frame.right);
    let ahead = direction.dot(frame.forward);
    let above = direction.dot(frame.up);
    let flat = side.hypot(ahead);
    let (angle, mono) = if flat < 1e-6 && above.abs() < 1e-6 {
        // At the listener: in the middle.
        (90.0, mono)
    } else {
        let angle = ahead.atan2(side).to_degrees().rem_euclid(360.0);
        let elevation = above.abs().atan2(flat).to_degrees();
        (angle, mono + ((elevation - 45.0) / 45.0).clamp(0.0, 1.0))
    };
    let mono = mono.clamp(0.0, 1.0);
    let speaker = |at: f32, width: f32| {
        let level = level(angle, at, width);
        level + (MONO_LEVEL - level) * mono
    };
    match speakers {
        Speakers::Headphones => [speaker(180.0, 180.0), speaker(0.0, 180.0)],
        Speakers::Stereo => [
            (speaker(135.0, 90.0) + REAR_SHARE * speaker(225.0, 90.0)).clamp(0.0, 1.0),
            (speaker(45.0, 90.0) + REAR_SHARE * speaker(315.0, 90.0)).clamp(0.0, 1.0),
        ],
    }
}

/// A speaker at `at` degrees hearing a sound at `angle`: full when they
/// agree, nothing once `width` degrees apart.
fn level(angle: f32, at: f32, width: f32) -> f32 {
    let mut apart = (angle - at).abs();
    if apart > 180.0 {
        apart = 360.0 - apart;
    }
    if apart >= width {
        return 0.0;
    }
    1.0 - (apart / width).powf(PAN_EXPONENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AHEAD: Vec3 = Vec3::X;
    const LEFT: Vec3 = Vec3::Y;
    const RIGHT: Vec3 = Vec3::NEG_Y;
    const BEHIND: Vec3 = Vec3::NEG_X;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-4 && (a[1] - b[1]).abs() < 1e-4
    }

    #[test]
    fn headphones_put_side_sounds_in_one_ear() {
        let frame = Frame::default();
        let centre = 1.0 - 0.5f32.powf(1.5);
        assert!(close(
            stereo(&frame, AHEAD, 0.0, Speakers::Headphones),
            [centre, centre]
        ));
        assert!(close(
            stereo(&frame, BEHIND, 0.0, Speakers::Headphones),
            [centre, centre]
        ));
        assert!(close(
            stereo(&frame, LEFT * 5.0, 0.0, Speakers::Headphones),
            [1.0, 0.0]
        ));
        assert!(close(
            stereo(&frame, RIGHT, 0.0, Speakers::Headphones),
            [0.0, 1.0]
        ));
    }

    #[test]
    fn stereo_speakers_hear_the_front_better_than_the_back() {
        let frame = Frame::default();
        let [ahead, _] = stereo(&frame, AHEAD, 0.0, Speakers::Stereo);
        let [behind, _] = stereo(&frame, BEHIND, 0.0, Speakers::Stereo);
        assert!(ahead > behind);
        assert!(close(
            stereo(&frame, LEFT, 0.0, Speakers::Stereo),
            [1.0, 0.0]
        ));
        assert!(close(
            stereo(&frame, RIGHT, 0.0, Speakers::Stereo),
            [0.0, 1.0]
        ));
        let [left, right] = stereo(&frame, AHEAD, 0.0, Speakers::Stereo);
        assert!((left - right).abs() < 1e-6, "ahead is centred");
    }

    #[test]
    fn overhead_and_omni_sounds_are_centred() {
        let frame = Frame::default();
        let up = stereo(&frame, Vec3::Z + LEFT * 0.01, 0.0, Speakers::Headphones);
        assert!((up[0] - up[1]).abs() < 0.02, "{up:?}");
        let omni = stereo(&frame, LEFT, 1.0, Speakers::Headphones);
        assert!(close(omni, [MONO_LEVEL, MONO_LEVEL]));
        let here = stereo(&frame, Vec3::ZERO, 0.0, Speakers::Headphones);
        assert!((here[0] - here[1]).abs() < 1e-6);
    }

    #[test]
    fn the_frame_turns_with_the_listener() {
        // Facing left, a sound to the world's left is ahead.
        let frame = Frame {
            forward: LEFT,
            right: AHEAD,
            up: Vec3::Z,
        };
        let [left, right] = stereo(&frame, LEFT, 0.0, Speakers::Headphones);
        assert!((left - right).abs() < 1e-6);
        assert!(close(
            stereo(&frame, BEHIND, 0.0, Speakers::Headphones),
            [1.0, 0.0]
        ));
    }
}
