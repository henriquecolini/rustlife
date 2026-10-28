//! The menu toolkit: a theme, animations and widgets for building screens.
//!
//! Screens (the main menu, settings, the pause menu) are written against
//! this module only, so they look and behave alike: the same fonts and
//! colours, the same hover and press animation, the same keyboard
//! navigation.

pub mod anim;
pub mod theme;
pub mod widgets;

use bevy::prelude::*;
use bevy::text::Font;
use bevy::window::PrimaryWindow;

pub use anim::{Curtain, Reveal, Smooth};
pub use theme::Fonts;
pub use widgets::{
    Activate, ButtonStyle, Choice, Disabled, Focus, FocusScope, Focusable, Slider, TextField,
    Toggle, UiSound, ValueChanged, WidgetsPlugin, button, choice, panel, slider, text_field,
    toggle,
};

/// Loads the fonts and adds the animation and widget systems.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // Inserted now rather than at startup: a game started from the
        // command line enters its first state before the startup systems'
        // commands are applied, and the HUD needs the fonts at once.
        let fonts = Fonts::load(&mut app.world_mut().resource_mut::<Assets<Font>>());
        app.insert_resource(fonts)
            .add_plugins((anim::AnimPlugin, WidgetsPlugin))
            .add_systems(Update, scale_ui);
    }
}

/// The height, in logical pixels, the UI is designed for.
const DESIGN_HEIGHT: f32 = 900.0;

/// The UI scale for a window `height` logical pixels tall: proportional to
/// the design height, in steps of 5%, within reason.
#[must_use]
pub fn ui_scale_for(height: f32) -> f32 {
    ((height / DESIGN_HEIGHT).clamp(0.8, 2.5) * 20.0).round() / 20.0
}

/// Scales the whole UI with the window, so menus and the HUD keep their
/// proportions on small and large screens alike.
fn scale_ui(windows: Query<&Window, With<PrimaryWindow>>, mut scale: ResMut<UiScale>) {
    let Ok(window) = windows.single() else {
        return;
    };
    let wanted = ui_scale_for(window.height());
    if (scale.0 - wanted).abs() > f32::EPSILON {
        scale.0 = wanted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ui_scales_with_the_window_in_steps_within_limits() {
        assert_eq!(ui_scale_for(900.0), 1.0);
        assert_eq!(ui_scale_for(1080.0), 1.2);
        assert_eq!(ui_scale_for(2160.0), 2.4);
        assert_eq!(ui_scale_for(300.0), 0.8);
        assert_eq!(ui_scale_for(10_000.0), 2.5);
        assert_eq!(ui_scale_for(1000.0), 1.1);
    }
}
