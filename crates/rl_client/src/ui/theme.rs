//! The look of the menus: colours, sizes and fonts.

use bevy::prelude::*;
use bevy::text::Font;

/// Page background, behind everything.
pub const BACKGROUND: Color = Color::srgb(0.043, 0.055, 0.078);
/// Panels floating on the page.
pub const PANEL: Color = Color::srgba(0.078, 0.098, 0.137, 0.94);
/// A panel's thin border.
pub const BORDER: Color = Color::srgb(0.165, 0.204, 0.263);
/// A control at rest.
pub const CONTROL: Color = Color::srgb(0.110, 0.137, 0.188);
/// A control under the pointer or focused.
pub const CONTROL_HOVER: Color = Color::srgb(0.157, 0.192, 0.259);
/// A control being pressed.
pub const CONTROL_PRESSED: Color = Color::srgb(0.086, 0.106, 0.149);
/// The accent: primary actions, selections, values.
pub const ACCENT: Color = Color::srgb(1.0, 0.616, 0.118);
/// The accent under the pointer.
pub const ACCENT_HOVER: Color = Color::srgb(1.0, 0.722, 0.302);
/// Text on the accent colour.
pub const ON_ACCENT: Color = Color::srgb(0.082, 0.063, 0.031);
/// Body text.
pub const TEXT: Color = Color::srgb(0.910, 0.925, 0.953);
/// Secondary text.
pub const TEXT_DIM: Color = Color::srgb(0.541, 0.580, 0.651);
/// Errors and destructive actions.
pub const DANGER: Color = Color::srgb(1.0, 0.353, 0.302);
/// Good news.
pub const SUCCESS: Color = Color::srgb(0.290, 0.871, 0.502);
/// Dims the page behind a modal screen such as the pause menu.
pub const SCRIM: Color = Color::srgba(0.02, 0.03, 0.05, 0.78);

/// Corner radius of panels, logical pixels.
pub const RADIUS: f32 = 10.0;
/// Corner radius of controls.
pub const CONTROL_RADIUS: f32 = 7.0;
/// Height of a button or control row.
pub const ROW_HEIGHT: f32 = 46.0;
/// The ordinary text size.
pub const TEXT_SIZE: f32 = 22.0;

/// The UI's fonts. Rajdhani, under the SIL Open Font License (see
/// `assets/fonts/OFL.txt`), is bundled so the menus look the same
/// everywhere.
#[derive(Resource, Clone)]
pub struct Fonts {
    /// Medium weight, for body text.
    pub regular: Handle<Font>,
    /// Bold, for headings, buttons and values.
    pub bold: Handle<Font>,
}

impl Fonts {
    /// Loads the bundled fonts into `fonts`.
    pub fn load(fonts: &mut Assets<Font>) -> Self {
        let font = |bytes: &[u8]| Font::from_bytes(bytes.to_vec());
        Self {
            regular: fonts.add(font(include_bytes!(
                "../../assets/fonts/Rajdhani-Medium.ttf"
            ))),
            bold: fonts.add(font(include_bytes!("../../assets/fonts/Rajdhani-Bold.ttf"))),
        }
    }

    /// A text bundle in the regular weight.
    #[must_use]
    pub fn text(&self, text: &str, size: f32, color: Color) -> impl Bundle + use<> {
        styled(text, size, color, self.regular.clone())
    }

    /// A text bundle in the bold weight.
    #[must_use]
    pub fn bold(&self, text: &str, size: f32, color: Color) -> impl Bundle + use<> {
        styled(text, size, color, self.bold.clone())
    }
}

fn styled(text: &str, size: f32, color: Color, font: Handle<Font>) -> impl Bundle + use<> {
    (
        Text::new(text),
        TextFont {
            font: font.into(),
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
        Pickable::IGNORE,
    )
}

/// `color` with its alpha replaced.
#[must_use]
pub fn with_alpha(color: Color, alpha: f32) -> Color {
    color.with_alpha(alpha)
}
