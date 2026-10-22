//! The single conversion between Source and Bevy coordinate systems.
//!
//! The simulation and all content keep Source's convention: a right-handed
//! frame with **+X forward, +Y left, +Z up**, measured in inches. Bevy's world
//! is a right-handed frame with **+X right, +Y up, +Z back**, measured in
//! metres. Every position crossing from the game world into rendering passes
//! through here, so the convention lives in exactly one tested place.

use bevy_math::{Quat, Vec3};

/// Metres per Source unit. Source treats one unit as one inch, so a 72-unit
/// player is about 1.83 m tall in Bevy — a sensible human scale.
pub const METERS_PER_UNIT: f32 = 0.0254;

/// Converts a Source-space position to a Bevy-space position.
///
/// The axis remap `(x, y, z) -> (x, z, -y)` rotates Source's Z-up frame into
/// Bevy's Y-up frame while preserving handedness (its determinant is `+1`), and
/// the result is scaled from inches to metres.
#[must_use]
pub fn position(source: [f32; 3]) -> Vec3 {
    Vec3::new(source[0], source[2], -source[1]) * METERS_PER_UNIT
}

/// Converts a Source-space direction (such as a surface normal) to Bevy space.
///
/// Same axis remap as [`position`] but without the scale, so a unit vector
/// stays unit length.
#[must_use]
pub fn direction(source: [f32; 3]) -> Vec3 {
    Vec3::new(source[0], source[2], -source[1])
}

/// Converts a Bevy-space position back to Source space (the inverse of
/// [`position`]).
#[must_use]
pub fn to_source_position(bevy: Vec3) -> [f32; 3] {
    let v = bevy / METERS_PER_UNIT;
    [v.x, -v.z, v.y]
}

/// Converts a Bevy-space direction back to Source space (the inverse of
/// [`direction`]).
#[must_use]
pub fn to_source_direction(bevy: Vec3) -> [f32; 3] {
    [bevy.x, -bevy.z, bevy.y]
}

/// Converts a Source yaw (degrees around +Z, 0 facing +X) into a Bevy yaw
/// (radians around +Y, for a camera whose unrotated view looks along -Z).
///
/// A Source facing `θ` points along `(cos θ, sin θ, 0)`, which [`direction`]
/// maps to `(cos θ, 0, -sin θ)`. A Bevy yaw `φ` turns `-Z` into
/// `(-sin φ, 0, -cos φ)`; equating the two gives `φ = θ - 90°`.
#[must_use]
pub fn yaw(source_degrees: f32) -> f32 {
    source_degrees.to_radians() - std::f32::consts::FRAC_PI_2
}

/// Converts Source Euler angles `[pitch, yaw, roll]` in degrees (an entity's
/// or static prop's `angles`) into a Bevy rotation.
///
/// Source composes them as `Rz(yaw) · Ry(pitch) · Rx(roll)` about its own axes
/// (`AngleMatrix`), so positive pitch tips the nose down. Because the frame
/// change in [`direction`] is itself a rotation, the same rotation in Bevy
/// space turns about the remapped axes: Source Z is Bevy +Y, Source Y is
/// Bevy −Z and Source X stays +X.
#[must_use]
pub fn rotation(angles_degrees: [f32; 3]) -> Quat {
    let [pitch, yaw, roll] = angles_degrees.map(f32::to_radians);
    Quat::from_axis_angle(Vec3::Y, yaw)
        * Quat::from_axis_angle(Vec3::NEG_Z, pitch)
        * Quat::from_axis_angle(Vec3::X, roll)
}

/// Converts a Source-space rotation (such as a physics body's orientation)
/// into a Bevy rotation.
///
/// The frame change is itself a rotation `M`, so the Bevy rotation is
/// `M · R · Mᵀ`: the same angle about the remapped axis. For a quaternion
/// that is just the vector part mapped by [`direction`].
#[must_use]
pub fn rotation_from_source(source: Quat) -> Quat {
    let axis = direction([source.x, source.y, source.z]);
    Quat::from_xyzw(axis.x, axis.y, axis.z, source.w)
}

/// Converts a Bevy-space rotation back to Source space (the inverse of
/// [`rotation_from_source`]).
#[must_use]
pub fn rotation_to_source(bevy: Quat) -> Quat {
    let axis = to_source_direction(Vec3::new(bevy.x, bevy.y, bevy.z));
    Quat::from_xyzw(axis[0], axis[1], axis[2], bevy.w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Source's `AngleMatrix`: the model's forward, left and up axes in world
    /// space for the given angles (degrees), straight from the SDK formulas.
    fn angle_matrix(pitch: f32, yaw: f32, roll: f32) -> [[f32; 3]; 3] {
        let (sp, cp) = pitch.to_radians().sin_cos();
        let (sy, cy) = yaw.to_radians().sin_cos();
        let (sr, cr) = roll.to_radians().sin_cos();
        [
            [cp * cy, cp * sy, -sp],
            [sp * sr * cy - cr * sy, sp * sr * sy + cr * cy, sr * cp],
            [sp * cr * cy + sr * sy, sp * cr * sy - sr * cy, cr * cp],
        ]
    }

    proptest! {
        #[test]
        fn source_quaternions_convert_like_euler_angles(
            pitch in -180.0f32..180.0,
            yaw in -360.0f32..360.0,
            roll in -180.0f32..180.0,
        ) {
            let source = Quat::from_rotation_z(yaw.to_radians())
                * Quat::from_rotation_y(pitch.to_radians())
                * Quat::from_rotation_x(roll.to_radians());
            let converted = rotation_from_source(source);
            let expected = rotation([pitch, yaw, roll]);
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                prop_assert!((converted * axis).abs_diff_eq(expected * axis, 1e-4));
            }
        }
    }

    proptest! {
        #[test]
        fn rotation_matches_source_angle_matrix(
            pitch in -180.0f32..180.0,
            yaw in -360.0f32..360.0,
            roll in -180.0f32..180.0,
        ) {
            let quat = rotation([pitch, yaw, roll]);
            let [forward, left, up] = angle_matrix(pitch, yaw, roll);
            for (local, world) in [
                ([1.0, 0.0, 0.0], forward),
                ([0.0, 1.0, 0.0], left),
                ([0.0, 0.0, 1.0], up),
            ] {
                let rotated = quat * direction(local);
                prop_assert!((rotated - direction(world)).length() < 1e-4);
            }
        }
    }

    proptest! {
        #[test]
        fn source_round_trips(x in -16384.0f32..16384.0, y in -16384.0f32..16384.0, z in -16384.0f32..16384.0) {
            let back = to_source_position(position([x, y, z]));
            for (a, b) in back.iter().zip([x, y, z]) {
                prop_assert!((a - b).abs() < 1e-2);
            }
            let turned = to_source_direction(direction([x, y, z]));
            prop_assert!(turned.iter().zip([x, y, z]).all(|(a, b)| (a - b).abs() < 1e-6));
        }
    }

    #[test]
    fn positive_pitch_looks_down() {
        let forward = rotation([30.0, 0.0, 0.0]) * Vec3::X;
        assert!(forward.y < 0.0, "nose down: {forward}");
    }

    #[test]
    fn yaw_points_the_camera_along_the_source_facing() {
        for source_degrees in [0.0_f32, 90.0, 180.0, 270.0, 33.0] {
            let theta = source_degrees.to_radians();
            let expected = direction([theta.cos(), theta.sin(), 0.0]);
            let forward = Quat::from_rotation_y(yaw(source_degrees)) * -Vec3::Z;
            assert!(
                (forward - expected).length() < 1e-5,
                "{source_degrees}°: {forward} != {expected}"
            );
        }
    }

    #[test]
    fn up_forward_and_left_map_to_bevy_axes() {
        // Source +Z (up) -> Bevy +Y (up).
        assert_eq!(direction([0.0, 0.0, 1.0]), Vec3::Y);
        // Source +X (forward) -> Bevy +X.
        assert_eq!(direction([1.0, 0.0, 0.0]), Vec3::X);
        // Source +Y (left) -> Bevy -Z (which, with +Z back, is forward).
        assert_eq!(direction([0.0, 1.0, 0.0]), -Vec3::Z);
    }

    #[test]
    fn positions_are_scaled_to_metres() {
        let bevy = position([0.0, 0.0, 72.0]);
        assert_eq!(bevy, Vec3::new(0.0, 72.0 * METERS_PER_UNIT, 0.0));
        assert!((bevy.y - 1.8288).abs() < 1e-4, "72 units ~= 1.83 m");
    }

    #[test]
    fn the_remap_preserves_handedness() {
        // X' cross Y' must equal Z' for a right-handed result.
        let x = direction([1.0, 0.0, 0.0]);
        let y = direction([0.0, 1.0, 0.0]);
        let z = direction([0.0, 0.0, 1.0]);
        assert!((x.cross(y) - z).length() < 1e-6);
    }
}
