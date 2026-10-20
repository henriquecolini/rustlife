//! `hud.ron`: where the HUD's elements sit and what they look like.
//!
//! Each element is placed by an [`Anchor`] on the screen and an offset in
//! logical pixels from it (towards the screen's centre). Colours are sRGB
//! `[red, green, blue, alpha]` with each channel in `0..=1`, as colour
//! pickers show them.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// An sRGB colour with alpha, each channel `0..=1`.
pub type Rgba = [f32; 4];

/// The HUD layout and style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Hud", deny_unknown_fields)]
pub struct HudConfig {
    /// Text size of the big numbers (health, armor, ammo), logical pixels.
    pub font_size: f32,
    /// Text size of everything else, logical pixels.
    pub small_font_size: f32,
    /// Colours.
    pub colors: HudColors,
    /// Health at or below this is drawn in the warning colour.
    pub low_health: f32,
    /// The crosshair, at the centre of the screen.
    pub crosshair: CrosshairConfig,
    /// Health read-out.
    pub health: Placement,
    /// Armor read-out.
    pub armor: Placement,
    /// Clip and carried ammo of the weapon in hand.
    pub ammo: Placement,
    /// The carried weapons, by slot, with the one in hand marked.
    pub weapons: Placement,
    /// Recent kills.
    pub kill_feed: KillFeedConfig,
    /// The player's frags and the match clock.
    pub score: Placement,
    /// Messages: who killed you, respawn countdown, pickups.
    pub messages: Placement,
}

/// HUD colours.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HudColors {
    /// Normal text.
    pub text: Rgba,
    /// Warnings: low health, an empty clip.
    pub warning: Rgba,
    /// Highlights: the weapon in hand, the local player in lists.
    pub highlight: Rgba,
    /// Background panels behind read-outs.
    pub panel: Rgba,
}

/// The crosshair: four strokes around a gap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrosshairConfig {
    /// Length of each stroke, logical pixels.
    pub length: f32,
    /// Thickness of each stroke.
    pub thickness: f32,
    /// Gap between the centre and each stroke.
    pub gap: f32,
    /// Colour.
    pub color: Rgba,
}

/// Recent kills, newest last.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KillFeedConfig {
    /// Where the list sits.
    pub place: Placement,
    /// Most kills listed.
    pub lines: u32,
    /// Seconds a kill stays listed.
    pub seconds: f32,
}

/// Where an element sits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    /// The screen point it is placed from.
    pub anchor: Anchor,
    /// Offset from the anchor towards the screen's centre, logical pixels
    /// `(x, y)`.
    pub offset: (f32, f32),
}

/// A point on the screen's edge (or its centre).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Anchor {
    /// Top-left corner.
    TopLeft,
    /// Middle of the top edge.
    Top,
    /// Top-right corner.
    TopRight,
    /// Middle of the left edge.
    Left,
    /// The centre.
    Center,
    /// Middle of the right edge.
    Right,
    /// Bottom-left corner.
    BottomLeft,
    /// Middle of the bottom edge.
    Bottom,
    /// Bottom-right corner.
    BottomRight,
}

impl HudConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.positive("font_size", self.font_size);
        check.positive("small_font_size", self.small_font_size);
        for (field, color) in [
            ("colors.text", self.colors.text),
            ("colors.warning", self.colors.warning),
            ("colors.highlight", self.colors.highlight),
            ("colors.panel", self.colors.panel),
            ("crosshair.color", self.crosshair.color),
        ] {
            if color.iter().any(|channel| !(0.0..=1.0).contains(channel)) {
                check.fail(field, "channels must be within 0..=1");
            }
        }
        check.non_negative("crosshair.length", self.crosshair.length);
        check.non_negative("crosshair.thickness", self.crosshair.thickness);
        check.non_negative("crosshair.gap", self.crosshair.gap);
        if self.kill_feed.lines == 0 {
            check.fail("kill_feed.lines", "must be at least 1");
        }
        check.positive("kill_feed.seconds", self.kill_feed.seconds);
    }
}
