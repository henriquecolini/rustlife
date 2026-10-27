//! The player's settings in the running client: the [`Settings`] resource
//! (the config's, plus change detection), key [`Bindings`], and the systems
//! that apply and save them.
//!
//! Settings are edited by the menus. Changing one marks the resource
//! changed; the systems here then update the window, camera and bindings,
//! and a moment later write the config file (waiting a little so dragging a
//! slider does not write on every frame).

use std::collections::HashMap;

use bevy::input::keyboard::KeyCode;
use bevy::prelude::*;
use bevy::render::view::Msaa;
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, VideoModeSelection, WindowMode};
use rl_config::{ACTIONS, Antialiasing, ControlSettings, VideoSettings};

use crate::camera::FlyCam;
use crate::content::Content;

/// How long after the last change the settings are written to disk, so a
/// slider being dragged saves once.
const SAVE_DELAY_SECS: f32 = 0.75;

/// Radians a pixel of mouse motion turns the view at sensitivity 1.
pub const RADIANS_PER_PIXEL: f32 = 0.003;

/// The player's settings.
#[derive(Resource, Debug, Clone, PartialEq, Deref, DerefMut)]
pub struct Settings(pub rl_config::Settings);

/// A physical input an action can be bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// A keyboard key.
    Key(KeyCode),
    /// A mouse button (left, right or middle).
    Mouse(MouseButton),
}

const KEYS: &[(KeyCode, &str, &str)] = &[
    (KeyCode::KeyA, "KeyA", "A"),
    (KeyCode::KeyB, "KeyB", "B"),
    (KeyCode::KeyC, "KeyC", "C"),
    (KeyCode::KeyD, "KeyD", "D"),
    (KeyCode::KeyE, "KeyE", "E"),
    (KeyCode::KeyF, "KeyF", "F"),
    (KeyCode::KeyG, "KeyG", "G"),
    (KeyCode::KeyH, "KeyH", "H"),
    (KeyCode::KeyI, "KeyI", "I"),
    (KeyCode::KeyJ, "KeyJ", "J"),
    (KeyCode::KeyK, "KeyK", "K"),
    (KeyCode::KeyL, "KeyL", "L"),
    (KeyCode::KeyM, "KeyM", "M"),
    (KeyCode::KeyN, "KeyN", "N"),
    (KeyCode::KeyO, "KeyO", "O"),
    (KeyCode::KeyP, "KeyP", "P"),
    (KeyCode::KeyQ, "KeyQ", "Q"),
    (KeyCode::KeyR, "KeyR", "R"),
    (KeyCode::KeyS, "KeyS", "S"),
    (KeyCode::KeyT, "KeyT", "T"),
    (KeyCode::KeyU, "KeyU", "U"),
    (KeyCode::KeyV, "KeyV", "V"),
    (KeyCode::KeyW, "KeyW", "W"),
    (KeyCode::KeyX, "KeyX", "X"),
    (KeyCode::KeyY, "KeyY", "Y"),
    (KeyCode::KeyZ, "KeyZ", "Z"),
    (KeyCode::Digit0, "Digit0", "0"),
    (KeyCode::Digit1, "Digit1", "1"),
    (KeyCode::Digit2, "Digit2", "2"),
    (KeyCode::Digit3, "Digit3", "3"),
    (KeyCode::Digit4, "Digit4", "4"),
    (KeyCode::Digit5, "Digit5", "5"),
    (KeyCode::Digit6, "Digit6", "6"),
    (KeyCode::Digit7, "Digit7", "7"),
    (KeyCode::Digit8, "Digit8", "8"),
    (KeyCode::Digit9, "Digit9", "9"),
    (KeyCode::F1, "F1", "F1"),
    (KeyCode::F2, "F2", "F2"),
    (KeyCode::F3, "F3", "F3"),
    (KeyCode::F4, "F4", "F4"),
    (KeyCode::F5, "F5", "F5"),
    (KeyCode::F6, "F6", "F6"),
    (KeyCode::F7, "F7", "F7"),
    (KeyCode::F8, "F8", "F8"),
    (KeyCode::F9, "F9", "F9"),
    (KeyCode::F10, "F10", "F10"),
    (KeyCode::F11, "F11", "F11"),
    (KeyCode::F12, "F12", "F12"),
    (KeyCode::Space, "Space", "Space"),
    (KeyCode::Tab, "Tab", "Tab"),
    (KeyCode::Enter, "Enter", "Enter"),
    (KeyCode::Backspace, "Backspace", "Backspace"),
    (KeyCode::ArrowUp, "ArrowUp", "Up"),
    (KeyCode::ArrowDown, "ArrowDown", "Down"),
    (KeyCode::ArrowLeft, "ArrowLeft", "Left"),
    (KeyCode::ArrowRight, "ArrowRight", "Right"),
    (KeyCode::ShiftLeft, "ShiftLeft", "Left Shift"),
    (KeyCode::ShiftRight, "ShiftRight", "Right Shift"),
    (KeyCode::ControlLeft, "ControlLeft", "Left Ctrl"),
    (KeyCode::ControlRight, "ControlRight", "Right Ctrl"),
    (KeyCode::AltLeft, "AltLeft", "Left Alt"),
    (KeyCode::AltRight, "AltRight", "Right Alt"),
    (KeyCode::Backquote, "Backquote", "`"),
    (KeyCode::Minus, "Minus", "-"),
    (KeyCode::Equal, "Equal", "="),
    (KeyCode::BracketLeft, "BracketLeft", "["),
    (KeyCode::BracketRight, "BracketRight", "]"),
    (KeyCode::Backslash, "Backslash", "\\"),
    (KeyCode::Semicolon, "Semicolon", ";"),
    (KeyCode::Quote, "Quote", "'"),
    (KeyCode::Comma, "Comma", ","),
    (KeyCode::Period, "Period", "."),
    (KeyCode::Slash, "Slash", "/"),
    (KeyCode::Insert, "Insert", "Insert"),
    (KeyCode::Delete, "Delete", "Delete"),
    (KeyCode::Home, "Home", "Home"),
    (KeyCode::End, "End", "End"),
    (KeyCode::PageUp, "PageUp", "Page Up"),
    (KeyCode::PageDown, "PageDown", "Page Down"),
    (KeyCode::CapsLock, "CapsLock", "Caps Lock"),
    (KeyCode::Numpad0, "Numpad0", "Num 0"),
    (KeyCode::Numpad1, "Numpad1", "Num 1"),
    (KeyCode::Numpad2, "Numpad2", "Num 2"),
    (KeyCode::Numpad3, "Numpad3", "Num 3"),
    (KeyCode::Numpad4, "Numpad4", "Num 4"),
    (KeyCode::Numpad5, "Numpad5", "Num 5"),
    (KeyCode::Numpad6, "Numpad6", "Num 6"),
    (KeyCode::Numpad7, "Numpad7", "Num 7"),
    (KeyCode::Numpad8, "Numpad8", "Num 8"),
    (KeyCode::Numpad9, "Numpad9", "Num 9"),
];

impl Binding {
    /// The input called `name` in the config file (`"KeyW"`,
    /// `"Mouse Left"`), if there is one.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "Mouse Left" => Some(Self::Mouse(MouseButton::Left)),
            "Mouse Right" => Some(Self::Mouse(MouseButton::Right)),
            "Mouse Middle" => Some(Self::Mouse(MouseButton::Middle)),
            _ => KEYS
                .iter()
                .find(|(_, key, _)| *key == name)
                .map(|(code, _, _)| Self::Key(*code)),
        }
    }

    /// The name stored in the config file, if the input can be bound.
    #[must_use]
    pub fn name(self) -> Option<&'static str> {
        match self {
            Self::Mouse(MouseButton::Left) => Some("Mouse Left"),
            Self::Mouse(MouseButton::Right) => Some("Mouse Right"),
            Self::Mouse(MouseButton::Middle) => Some("Mouse Middle"),
            Self::Mouse(_) => None,
            Self::Key(code) => KEYS
                .iter()
                .find(|(key, _, _)| *key == code)
                .map(|(_, name, _)| *name),
        }
    }

    /// What the menus call the input.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Mouse(MouseButton::Left) => "Left Mouse",
            Self::Mouse(MouseButton::Right) => "Right Mouse",
            Self::Mouse(MouseButton::Middle) => "Middle Mouse",
            Self::Mouse(_) => "Mouse",
            Self::Key(code) => KEYS
                .iter()
                .find(|(key, _, _)| *key == code)
                .map_or("?", |(_, _, label)| *label),
        }
    }

    /// Whether the input is held.
    #[must_use]
    pub fn pressed(
        self,
        keys: &ButtonInput<KeyCode>,
        mouse: Option<&ButtonInput<MouseButton>>,
    ) -> bool {
        match self {
            Self::Key(code) => keys.pressed(code),
            Self::Mouse(button) => mouse.is_some_and(|mouse| mouse.pressed(button)),
        }
    }

    /// Whether the input went down this frame.
    #[must_use]
    pub fn just_pressed(
        self,
        keys: &ButtonInput<KeyCode>,
        mouse: Option<&ButtonInput<MouseButton>>,
    ) -> bool {
        match self {
            Self::Key(code) => keys.just_pressed(code),
            Self::Mouse(button) => mouse.is_some_and(|mouse| mouse.just_pressed(button)),
        }
    }
}

/// The input each action is bound to, from the settings.
#[derive(Resource, Debug, Clone)]
pub struct Bindings(HashMap<&'static str, Binding>);

impl Bindings {
    /// The bindings `controls` describe; an action whose binding does not
    /// parse falls back to its default.
    #[must_use]
    pub fn from_controls(controls: &ControlSettings) -> Self {
        let bindings = ACTIONS
            .iter()
            .filter_map(|action| {
                let binding = Binding::parse(controls.binding(action.id))
                    .or_else(|| Binding::parse(action.default))?;
                Some((action.id, binding))
            })
            .collect();
        Self(bindings)
    }

    /// The input bound to `action` (an id from [`ACTIONS`]).
    #[must_use]
    pub fn get(&self, action: &str) -> Option<Binding> {
        self.0.get(action).copied()
    }

    /// Whether `action`'s input is held.
    #[must_use]
    pub fn pressed(
        &self,
        action: &str,
        keys: &ButtonInput<KeyCode>,
        mouse: Option<&ButtonInput<MouseButton>>,
    ) -> bool {
        self.get(action)
            .is_some_and(|binding| binding.pressed(keys, mouse))
    }

    /// Whether `action`'s input went down this frame.
    #[must_use]
    pub fn just_pressed(
        &self,
        action: &str,
        keys: &ButtonInput<KeyCode>,
        mouse: Option<&ButtonInput<MouseButton>>,
    ) -> bool {
        self.get(action)
            .is_some_and(|binding| binding.just_pressed(keys, mouse))
    }
}

impl Default for Bindings {
    fn default() -> Self {
        Self::from_controls(&ControlSettings::default())
    }
}

/// The vertical field of view, radians, that gives `horizontal_degrees`
/// across a view of the given aspect ratio (width over height).
#[must_use]
pub fn vertical_fov(horizontal_degrees: f32, aspect: f32) -> f32 {
    2.0 * ((horizontal_degrees.to_radians() / 2.0).tan() / aspect.max(0.1)).atan()
}

/// The window settings as Bevy's window types.
#[must_use]
pub fn window_mode(video: &VideoSettings) -> WindowMode {
    match video.window_mode {
        rl_config::WindowMode::Windowed => WindowMode::Windowed,
        rl_config::WindowMode::Borderless => {
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        }
        rl_config::WindowMode::Fullscreen => {
            WindowMode::Fullscreen(MonitorSelection::Current, VideoModeSelection::Current)
        }
    }
}

/// The present mode for the vsync setting.
#[must_use]
pub fn present_mode(video: &VideoSettings) -> PresentMode {
    if video.vsync {
        PresentMode::AutoVsync
    } else {
        PresentMode::AutoNoVsync
    }
}

/// The MSAA level for the anti-aliasing setting.
#[must_use]
pub fn msaa(video: &VideoSettings) -> Msaa {
    match video.antialiasing {
        Antialiasing::Off => Msaa::Off,
        Antialiasing::X2 => Msaa::Sample2,
        Antialiasing::X4 => Msaa::Sample4,
    }
}

/// Keeps the window, camera, bindings and config file in step with the
/// [`Settings`].
pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Bindings>().add_systems(
            Update,
            (
                rebuild_bindings,
                apply_window,
                apply_camera,
                apply_texture_detail,
                save_later,
            )
                .chain(),
        );
    }
}

fn rebuild_bindings(settings: Res<Settings>, mut bindings: ResMut<Bindings>) {
    if settings.is_changed() {
        *bindings = Bindings::from_controls(&settings.controls);
    }
}

fn apply_window(settings: Res<Settings>, mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    if !settings.is_changed() {
        return;
    }
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let video = &settings.video;
    window.mode = window_mode(video);
    window.present_mode = present_mode(video);
    if video.window_mode == rl_config::WindowMode::Windowed {
        window
            .resolution
            .set(video.resolution.0 as f32, video.resolution.1 as f32);
    }
}

/// Mips left out and anisotropic filtering for a texture quality: textures
/// that load afterwards (the next map's) use them.
#[must_use]
pub fn texture_detail(quality: rl_config::Quality) -> (u32, u16) {
    match quality {
        rl_config::Quality::Low => (2, 1),
        rl_config::Quality::Medium => (1, 4),
        rl_config::Quality::High => (0, 8),
        rl_config::Quality::Ultra => (0, 16),
    }
}

fn apply_texture_detail(settings: Res<Settings>, detail: Option<Res<rl_assets::TextureDetail>>) {
    if let Some(detail) = detail
        && settings.is_changed()
    {
        let (drop, anisotropy) = texture_detail(settings.video.textures);
        detail.set(drop, anisotropy);
    }
}

pub(crate) fn apply_camera(
    settings: Res<Settings>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&mut Projection, &mut Msaa, Has<FlyCam>)>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let aspect = window.width() / window.height().max(1.0);
    let fov = vertical_fov(settings.video.fov, aspect);
    let wanted = msaa(&settings.video);
    for (mut projection, mut msaa, is_main) in &mut cameras {
        // The view model's narrower view and the sky camera's scaled one are
        // their own; only the player's camera follows the FOV setting.
        if is_main
            && let Projection::Perspective(perspective) = &mut *projection
            && (perspective.fov - fov).abs() > 1e-4
        {
            perspective.fov = fov;
        }
        // Every 3D camera must sample alike. Cameras draw over one another
        // (the sky behind the world, the view model over it); with mixed
        // sample counts their passes cannot share the frame, and the world
        // can vanish behind the sky.
        if *msaa != wanted {
            *msaa = wanted;
        }
    }
}

/// Writes the settings to the config file once they have stopped changing.
fn save_later(
    time: Res<Time<Real>>,
    settings: Res<Settings>,
    mut content: ResMut<Content>,
    mut waiting: Local<Option<f32>>,
    mut started: Local<bool>,
) {
    // The first run sees the settings as new, not as changed.
    if !*started {
        *started = true;
        return;
    }
    if settings.is_changed() {
        *waiting = Some(SAVE_DELAY_SECS);
    }
    let Some(left) = *waiting else {
        return;
    };
    let left = left - time.delta_secs();
    if left > 0.0 {
        *waiting = Some(left);
        return;
    }
    *waiting = None;
    content.config.settings = settings.0.clone();
    if let Err(error) = content.save() {
        warn!("cannot save the settings: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_action_binding_parses_and_round_trips() {
        for action in ACTIONS {
            let binding = Binding::parse(action.default)
                .unwrap_or_else(|| panic!("{} does not parse", action.default));
            assert_eq!(binding.name(), Some(action.default));
        }
        let bindings = Bindings::default();
        assert_eq!(bindings.get("jump"), Some(Binding::Key(KeyCode::Space)));
        assert_eq!(
            bindings.get("attack"),
            Some(Binding::Mouse(MouseButton::Left))
        );
    }

    #[test]
    fn key_names_are_unique_and_unknown_ones_fall_back() {
        let mut names: Vec<&str> = KEYS.iter().map(|(_, name, _)| *name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "a key name repeats");
        assert_eq!(Binding::parse("NoSuchKey"), None);

        let mut controls = ControlSettings::default();
        controls.bindings.insert("jump".into(), "Garbage".into());
        assert_eq!(
            Bindings::from_controls(&controls).get("jump"),
            Some(Binding::Key(KeyCode::Space)),
            "an unparsable binding uses the default"
        );
        controls.bindings.insert("jump".into(), "KeyJ".into());
        assert_eq!(
            Bindings::from_controls(&controls).get("jump"),
            Some(Binding::Key(KeyCode::KeyJ))
        );
    }

    #[test]
    fn bindings_read_held_keys_and_mouse_buttons() {
        let bindings = Bindings::default();
        let mut keys = ButtonInput::<KeyCode>::default();
        let mut mouse = ButtonInput::<MouseButton>::default();
        assert!(!bindings.pressed("forward", &keys, Some(&mouse)));
        keys.press(KeyCode::KeyW);
        mouse.press(MouseButton::Left);
        assert!(bindings.pressed("forward", &keys, Some(&mouse)));
        assert!(bindings.just_pressed("attack", &keys, Some(&mouse)));
        assert!(!bindings.pressed("attack", &keys, None), "no mouse input");
        assert!(!bindings.pressed("nonsense", &keys, Some(&mouse)));
    }

    #[test]
    fn horizontal_fov_converts_to_the_vertical_one() {
        // A 90 degree horizontal view on a 4:3 screen is about 73.7 vertical.
        let vertical = vertical_fov(90.0, 4.0 / 3.0).to_degrees();
        assert!((vertical - 73.74).abs() < 0.05, "{vertical}");
        // A square view has equal angles.
        assert!((vertical_fov(90.0, 1.0).to_degrees() - 90.0).abs() < 1e-3);
        // Wider screens see less vertically for the same horizontal angle.
        assert!(vertical_fov(90.0, 16.0 / 9.0) < vertical_fov(90.0, 4.0 / 3.0));
        assert!(vertical_fov(90.0, 0.0).is_finite());
    }

    #[test]
    fn video_settings_map_onto_bevy_types() {
        let mut video = VideoSettings::default();
        assert_eq!(window_mode(&video), WindowMode::Windowed);
        assert_eq!(present_mode(&video), PresentMode::AutoVsync);
        video.vsync = false;
        video.window_mode = rl_config::WindowMode::Borderless;
        video.antialiasing = Antialiasing::Off;
        assert_eq!(present_mode(&video), PresentMode::AutoNoVsync);
        assert!(matches!(
            window_mode(&video),
            WindowMode::BorderlessFullscreen(_)
        ));
        assert_eq!(msaa(&video), Msaa::Off);
    }
}

#[cfg(test)]
mod texture_detail_tests {
    use super::*;

    #[test]
    fn lower_texture_quality_drops_more_mips_and_filters_less() {
        let levels: Vec<_> = rl_config::Quality::ALL
            .iter()
            .map(|&q| texture_detail(q))
            .collect();
        assert!(
            levels
                .windows(2)
                .all(|w| w[0].0 >= w[1].0 && w[0].1 <= w[1].1)
        );
    }
}
