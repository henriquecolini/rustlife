//! The settings screens: video, audio, controls and player.
//!
//! Each row is a widget tagged with the [`Setting`] it edits. One observer
//! hears every change, reads the widget's value and writes it through
//! [`apply_setting`]; the settings systems take it from there.

use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use rl_config::{ACTIONS, Antialiasing, Quality, WindowMode};

use super::{Ctx, Menu, Screen, Scrollable, heading};
use crate::settings::{Binding, Settings};
use crate::ui::{
    Activate, ButtonStyle, Choice, Focus, Reveal, Slider, TextField, Toggle, ValueChanged, button,
    choice, panel, slider, text_field, theme, toggle,
};

/// A tab of the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// Window and rendering.
    #[default]
    Video,
    /// Volumes.
    Audio,
    /// Mouse and keys.
    Controls,
    /// The player's name.
    Player,
}

impl SettingsTab {
    /// Every tab, in order.
    pub const ALL: [Self; 4] = [Self::Video, Self::Audio, Self::Controls, Self::Player];

    /// The tab's name.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Controls => "Controls",
            Self::Player => "Player",
        }
    }

    /// The tab called `name` (lowercase, or empty for the first).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if name.is_empty() {
            return Some(Self::Video);
        }
        Self::ALL
            .into_iter()
            .find(|tab| tab.label().eq_ignore_ascii_case(name))
    }
}

/// The setting a widget edits.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Setting {
    /// Windowed, borderless or fullscreen.
    WindowMode,
    /// Window size.
    Resolution,
    /// Vertical sync.
    Vsync,
    /// Field of view.
    Fov,
    /// Texture quality.
    Textures,
    /// Shadow quality.
    Shadows,
    /// Anti-aliasing.
    Antialiasing,
    /// Frame rate readout.
    ShowFps,
    /// Master volume.
    Master,
    /// Effects volume.
    Effects,
    /// Ambient volume.
    Ambient,
    /// Mouse sensitivity.
    Sensitivity,
    /// Inverted mouse.
    InvertY,
    /// Player name.
    Name,
}

/// A widget's value, as `apply_setting` takes it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    /// A switch.
    Bool(bool),
    /// A slider.
    Float(f32),
    /// The index of a chosen option.
    Index(usize),
    /// Typed text.
    Text(String),
}

/// Window sizes offered.
pub(super) const RESOLUTIONS: [(u32, u32); 6] = [
    (1280, 720),
    (1600, 900),
    (1920, 1080),
    (2560, 1080),
    (2560, 1440),
    (3840, 2160),
];

const MODES: [WindowMode; 3] = [
    WindowMode::Windowed,
    WindowMode::Borderless,
    WindowMode::Fullscreen,
];

/// Writes `value` into the setting `setting` of `settings`. Values that do
/// not fit the setting (a number for a switch) are ignored.
pub(super) fn apply_setting(settings: &mut rl_config::Settings, setting: Setting, value: &Value) {
    match (setting, value) {
        (Setting::WindowMode, Value::Index(i)) => {
            if let Some(mode) = MODES.get(*i) {
                settings.video.window_mode = *mode;
            }
        }
        (Setting::Resolution, Value::Index(i)) => {
            if let Some(size) = resolutions(settings.video.resolution).get(*i) {
                settings.video.resolution = *size;
            }
        }
        (Setting::Vsync, Value::Bool(on)) => settings.video.vsync = *on,
        (Setting::Fov, Value::Float(degrees)) => settings.video.fov = *degrees,
        (Setting::Textures, Value::Index(i)) => {
            if let Some(quality) = Quality::ALL.get(*i) {
                settings.video.textures = *quality;
            }
        }
        (Setting::Shadows, Value::Index(i)) => {
            if let Some(quality) = Quality::ALL.get(*i) {
                settings.video.shadows = *quality;
            }
        }
        (Setting::Antialiasing, Value::Index(i)) => {
            if let Some(level) = Antialiasing::ALL.get(*i) {
                settings.video.antialiasing = *level;
            }
        }
        (Setting::ShowFps, Value::Bool(on)) => settings.video.show_fps = *on,
        (Setting::Master, Value::Float(volume)) => settings.audio.master = *volume,
        (Setting::Effects, Value::Float(volume)) => settings.audio.effects = *volume,
        (Setting::Ambient, Value::Float(volume)) => settings.audio.ambient = *volume,
        (Setting::Sensitivity, Value::Float(sensitivity)) => {
            settings.controls.sensitivity = *sensitivity;
        }
        (Setting::InvertY, Value::Bool(on)) => settings.controls.invert_y = *on,
        (Setting::Name, Value::Text(name)) => settings.player.name.clone_from(name),
        _ => {}
    }
}

/// The window sizes to offer: the standard ones, plus the current size
/// when it is none of them.
#[must_use]
pub(super) fn resolutions(current: (u32, u32)) -> Vec<(u32, u32)> {
    let mut sizes = RESOLUTIONS.to_vec();
    if !sizes.contains(&current) {
        sizes.push(current);
        sizes.sort_unstable();
    }
    sizes
}

fn quality_choice(current: Quality) -> Choice {
    Choice {
        options: Quality::ALL.iter().map(|q| q.label().to_owned()).collect(),
        index: Quality::ALL.iter().position(|q| *q == current).unwrap_or(0),
    }
}

/// The action being rebound, waiting for a key.
#[derive(Resource, Debug, Default)]
pub(super) struct Rebinding(pub(super) Option<&'static str>);

/// A button that rebinds an action.
#[derive(Component)]
struct BindButton(&'static str);

pub(super) struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Rebinding>()
            .add_observer(on_value_changed)
            .add_systems(Update, (capture_binding, show_bindings).chain());
    }
}

fn percent(value: f32) -> String {
    format!("{:.0}%", value * 100.0)
}

fn degrees(value: f32) -> String {
    format!("{value:.0}°")
}

fn times(value: f32) -> String {
    format!("{value:.2}×")
}

pub(super) fn spawn(layer: &mut ChildSpawnerCommands, ctx: &Ctx, tab: SettingsTab) {
    layer
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|page| {
            page.spawn((
                panel(Val::Px(760.0), Val::Percent(90.0)),
                Reveal::new(Vec2::new(0.0, 24.0), 0.0),
                UiTransform::IDENTITY,
            ))
            .with_children(|panel| {
                heading(panel, ctx, "Settings", "");
                tab_bar(panel, ctx, tab);
                panel
                    .spawn((
                        Scrollable,
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(8.0),
                            overflow: Overflow::scroll_y(),
                            flex_shrink: 1.0,
                            padding: UiRect::right(Val::Px(6.0)),
                            ..default()
                        },
                    ))
                    .with_children(|rows| match tab {
                        SettingsTab::Video => video_rows(rows, ctx),
                        SettingsTab::Audio => audio_rows(rows, ctx),
                        SettingsTab::Controls => control_rows(rows, ctx),
                        SettingsTab::Player => player_rows(rows, ctx),
                    });
                panel
                    .spawn(button(ctx.fonts, "Back", ButtonStyle::Primary))
                    .observe(|_: On<Activate>, mut menu: ResMut<Menu>| {
                        menu.pop();
                    });
            });
        });
}

fn tab_bar(panel: &mut ChildSpawnerCommands, ctx: &Ctx, current: SettingsTab) {
    panel
        .spawn(Node {
            column_gap: Val::Px(8.0),
            margin: UiRect::bottom(Val::Px(6.0)),
            ..default()
        })
        .with_children(|bar| {
            for tab in SettingsTab::ALL {
                let style = if tab == current {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Secondary
                };
                bar.spawn(button(ctx.fonts, tab.label(), style)).observe(
                    move |_: On<Activate>, mut menu: ResMut<Menu>| {
                        menu.replace(Screen::Settings(tab));
                    },
                );
            }
        });
}

fn video_rows(rows: &mut ChildSpawnerCommands, ctx: &Ctx) {
    let video = &ctx.settings.video;
    let sizes = resolutions(video.resolution);
    rows.spawn((
        Setting::WindowMode,
        choice(
            ctx.fonts,
            "Window mode",
            Choice {
                options: vec!["Windowed".into(), "Borderless".into(), "Fullscreen".into()],
                index: MODES
                    .iter()
                    .position(|m| *m == video.window_mode)
                    .unwrap_or(0),
            },
        ),
    ));
    rows.spawn((
        Setting::Resolution,
        choice(
            ctx.fonts,
            "Window size",
            Choice {
                options: sizes.iter().map(|(w, h)| format!("{w} × {h}")).collect(),
                index: sizes
                    .iter()
                    .position(|s| *s == video.resolution)
                    .unwrap_or(0),
            },
        ),
    ));
    rows.spawn((
        Setting::Vsync,
        toggle(ctx.fonts, "Vertical sync", video.vsync),
    ));
    rows.spawn((
        Setting::Fov,
        slider(
            ctx.fonts,
            "Field of view",
            Slider::new(video.fov, 60.0, 120.0, 1.0),
            degrees,
        ),
    ));
    rows.spawn((
        Setting::Textures,
        choice(
            ctx.fonts,
            "Texture quality (next map)",
            quality_choice(video.textures),
        ),
    ));
    rows.spawn((
        Setting::Shadows,
        choice(ctx.fonts, "Shadow quality", quality_choice(video.shadows)),
    ));
    rows.spawn((
        Setting::Antialiasing,
        choice(
            ctx.fonts,
            "Anti-aliasing",
            Choice {
                options: Antialiasing::ALL
                    .iter()
                    .map(|a| a.label().to_owned())
                    .collect(),
                index: Antialiasing::ALL
                    .iter()
                    .position(|a| *a == video.antialiasing)
                    .unwrap_or(0),
            },
        ),
    ));
    rows.spawn((
        Setting::ShowFps,
        toggle(ctx.fonts, "Show frame rate", video.show_fps),
    ));
}

fn audio_rows(rows: &mut ChildSpawnerCommands, ctx: &Ctx) {
    let audio = &ctx.settings.audio;
    for (setting, label, value) in [
        (Setting::Master, "Master volume", audio.master),
        (Setting::Effects, "Effects", audio.effects),
        (Setting::Ambient, "Ambience", audio.ambient),
    ] {
        rows.spawn((
            setting,
            slider(
                ctx.fonts,
                label,
                Slider::new(value, 0.0, 1.0, 0.05),
                percent,
            ),
        ));
    }
}

fn control_rows(rows: &mut ChildSpawnerCommands, ctx: &Ctx) {
    let controls = &ctx.settings.controls;
    rows.spawn((
        Setting::Sensitivity,
        slider(
            ctx.fonts,
            "Mouse sensitivity",
            Slider::new(controls.sensitivity, 0.1, 3.0, 0.05),
            times,
        ),
    ));
    rows.spawn((
        Setting::InvertY,
        toggle(ctx.fonts, "Invert mouse", controls.invert_y),
    ));
    rows.spawn(ctx.fonts.bold("Key bindings", 18.0, theme::TEXT_DIM));
    for action in ACTIONS {
        let bound = Binding::label_of(controls.binding(action.id));
        rows.spawn(Node {
            width: Val::Percent(100.0),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|row| {
            row.spawn(ctx.fonts.text(action.label, theme::TEXT_SIZE, theme::TEXT));
            let id = action.id;
            row.spawn(Node {
                min_width: Val::Px(200.0),
                flex_direction: FlexDirection::Column,
                ..default()
            })
            .with_children(|slot| {
                slot.spawn((
                    BindButton(action.id),
                    button(ctx.fonts, bound, ButtonStyle::Secondary),
                ))
                .observe(
                    move |_: On<Activate>, mut rebinding: ResMut<Rebinding>| {
                        rebinding.0 = ACTIONS.iter().find(|a| a.id == id).map(|a| a.id);
                    },
                );
            });
        });
    }
    rows.spawn(button(ctx.fonts, "Reset bindings", ButtonStyle::Danger))
        .observe(|_: On<Activate>, mut settings: ResMut<Settings>| {
            settings.controls.reset_bindings();
        });
}

fn player_rows(rows: &mut ChildSpawnerCommands, ctx: &Ctx) {
    rows.spawn((
        Setting::Name,
        text_field(
            ctx.fonts,
            "Player name",
            &ctx.settings.player.name,
            "name",
            24,
        ),
    ));
    rows.spawn(ctx.fonts.text(
        "Other players see this name in the scoreboard and the kill feed.",
        18.0,
        theme::TEXT_DIM,
    ));
}

/// What a setting's widget can be: one of the four value widgets.
type SettingWidget = (
    &'static Setting,
    Option<&'static Toggle>,
    Option<&'static Slider>,
    Option<&'static Choice>,
    Option<&'static TextField>,
);

/// Reads a changed widget's value into the settings.
fn on_value_changed(
    changed: On<ValueChanged>,
    widgets: Query<SettingWidget>,
    mut settings: ResMut<Settings>,
) {
    let Ok((setting, toggle, slider, choice, field)) = widgets.get(changed.entity) else {
        return;
    };
    let value = match (toggle, slider, choice, field) {
        (Some(toggle), ..) => Value::Bool(toggle.0),
        (_, Some(slider), ..) => Value::Float(slider.value),
        (_, _, Some(choice), _) => Value::Index(choice.index),
        (.., Some(field)) => Value::Text(field.value.clone()),
        _ => return,
    };
    let mut next = settings.0.clone();
    apply_setting(&mut next, *setting, &value);
    if next != settings.0 {
        settings.0 = next;
    }
}

/// While an action is being rebound, takes the next key or mouse button.
/// `Esc` cancels.
fn capture_binding(
    mut rebinding: ResMut<Rebinding>,
    mut typed: MessageReader<KeyboardInput>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut settings: ResMut<Settings>,
    mut focus: ResMut<Focus>,
) {
    let Some(action) = rebinding.0 else {
        typed.clear();
        return;
    };
    let mut chosen = None;
    for event in typed.read().filter(|event| event.state.is_pressed()) {
        if event.key_code == KeyCode::Escape {
            rebinding.0 = None;
            return;
        }
        chosen = chosen.or(Some(Binding::Key(event.key_code)));
    }
    for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
        // The click that started the capture is still in this frame's input.
        if mouse.just_pressed(button) && focus.0.is_none() {
            chosen = chosen.or(Some(Binding::Mouse(button)));
        }
    }
    if let Some(name) = chosen.and_then(Binding::name) {
        settings.controls.rebind(action, name);
        rebinding.0 = None;
        focus.0 = None;
    }
}

/// Shows each binding button's key, or "Press a key…" while it waits.
fn show_bindings(
    rebinding: Res<Rebinding>,
    settings: Res<Settings>,
    buttons: Query<(&BindButton, &Children)>,
    mut texts: Query<&mut Text>,
) {
    if !rebinding.is_changed() && !settings.is_changed() {
        return;
    }
    for (bind, children) in &buttons {
        let label = if rebinding.0 == Some(bind.0) {
            "Press a key…".to_owned()
        } else {
            Binding::label_of(settings.controls.binding(bind.0)).to_owned()
        };
        for &child in children {
            if let Ok(mut text) = texts.get_mut(child) {
                text.0.clone_from(&label);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_values_land_in_the_right_setting() {
        let mut settings = rl_config::Settings::default();
        apply_setting(&mut settings, Setting::Vsync, &Value::Bool(false));
        apply_setting(&mut settings, Setting::Fov, &Value::Float(100.0));
        apply_setting(&mut settings, Setting::Master, &Value::Float(0.3));
        apply_setting(&mut settings, Setting::WindowMode, &Value::Index(2));
        apply_setting(&mut settings, Setting::Textures, &Value::Index(0));
        apply_setting(&mut settings, Setting::Shadows, &Value::Index(3));
        apply_setting(&mut settings, Setting::Antialiasing, &Value::Index(1));
        apply_setting(&mut settings, Setting::Name, &Value::Text("Alyx".into()));
        apply_setting(&mut settings, Setting::InvertY, &Value::Bool(true));
        assert!(!settings.video.vsync);
        assert_eq!(settings.video.fov, 100.0);
        assert_eq!(settings.audio.master, 0.3);
        assert_eq!(settings.video.window_mode, WindowMode::Fullscreen);
        assert_eq!(settings.video.textures, Quality::Low);
        assert_eq!(settings.video.shadows, Quality::Ultra);
        assert_eq!(settings.video.antialiasing, Antialiasing::X2);
        assert_eq!(settings.player.name, "Alyx");
        assert!(settings.controls.invert_y);
    }

    #[test]
    fn values_of_the_wrong_kind_or_out_of_range_change_nothing() {
        let mut settings = rl_config::Settings::default();
        let before = settings.clone();
        apply_setting(&mut settings, Setting::Vsync, &Value::Float(1.0));
        apply_setting(&mut settings, Setting::Textures, &Value::Index(99));
        apply_setting(&mut settings, Setting::Name, &Value::Bool(true));
        assert_eq!(settings, before);
    }

    #[test]
    fn the_current_window_size_is_always_offered_once() {
        assert_eq!(resolutions((1920, 1080)), RESOLUTIONS.to_vec());
        let odd = resolutions((1000, 700));
        assert_eq!(odd.len(), RESOLUTIONS.len() + 1);
        assert!(odd.contains(&(1000, 700)));
        // The choice indexes the list built around the current size.
        let mut settings = rl_config::Settings::default();
        settings.video.resolution = (1000, 700);
        apply_setting(&mut settings, Setting::Resolution, &Value::Index(1));
        assert_eq!(settings.video.resolution, odd[1]);
        assert_eq!(settings.video.resolution, RESOLUTIONS[0]);
    }

    #[test]
    fn tabs_parse_by_name() {
        assert_eq!(SettingsTab::parse(""), Some(SettingsTab::Video));
        assert_eq!(SettingsTab::parse("controls"), Some(SettingsTab::Controls));
        assert_eq!(SettingsTab::parse("PLAYER"), Some(SettingsTab::Player));
        assert_eq!(SettingsTab::parse("x"), None);
    }
}
