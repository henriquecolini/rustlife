//! The single conversion between Source and Bevy coordinate systems.
//!
//! The simulation and all content keep Source's convention: a right-handed
//! frame with **+X forward, +Y left, +Z up**, measured in inches. Bevy's world
//! is a right-handed frame with **+X right, +Y up, +Z back**, measured in
//! metres. Every position crossing from the game world into rendering passes
//! through here, so the convention lives in exactly one tested place.

use bevy_math::Vec3;

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

#[cfg(test)]
mod tests {
    use super::*;

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
