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

pub use anim::{Curtain, Reveal, Smooth};
pub use theme::Fonts;
pub use widgets::{
    Activate, ButtonStyle, Choice, Disabled, Focus, FocusScope, Focusable, Slider, TextField,
    Toggle, ValueChanged, WidgetsPlugin, button, choice, panel, slider, text_field, toggle,
};

/// Loads the fonts and adds the animation and widget systems.
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((anim::AnimPlugin, WidgetsPlugin))
            .add_systems(PreStartup, load_fonts);
    }
}

fn load_fonts(mut commands: Commands, mut fonts: ResMut<Assets<Font>>) {
    commands.insert_resource(Fonts::load(&mut fonts));
}
