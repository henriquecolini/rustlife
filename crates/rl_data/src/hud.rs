//! `hud.ron`: the parts of the HUD that are gameplay tuning rather than
//! look: when health is low, the crosshair, and how long kills stay in the
//! feed. How the HUD is drawn (fonts, colours, layout) belongs to the
//! client's theme.
//!
//! Colours are sRGB `[red, green, blue, alpha]` with each channel in
//! `0..=1`, as colour pickers show them.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// An sRGB colour with alpha, each channel `0..=1`.
pub type Rgba = [f32; 4];

/// The HUD's tunables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Hud", deny_unknown_fields)]
pub struct HudConfig {
    /// Health at or below this is drawn as a warning.
    pub low_health: f32,
    /// The crosshair, at the centre of the screen.
    pub crosshair: CrosshairConfig,
    /// Recent kills.
    pub kill_feed: KillFeedConfig,
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
    /// Most kills listed.
    pub lines: u32,
    /// Seconds a kill stays listed.
    pub seconds: f32,
}

impl HudConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.non_negative("low_health", self.low_health);
        if self
            .crosshair
            .color
            .iter()
            .any(|channel| !(0.0..=1.0).contains(channel))
        {
            check.fail("crosshair.color", "channels must be within 0..=1");
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
