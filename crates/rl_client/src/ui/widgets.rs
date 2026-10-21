//! Widgets: buttons, toggles, sliders, choosers and text fields.
//!
//! Every interactive widget carries [`Interact`] (hover/press state),
//! [`Look`] (its colours) and, for keyboard use, [`Focusable`]. Clicking a
//! widget, or pressing Enter/Space on the focused one, triggers
//! [`Activate`] on it; widgets with a value (toggle, slider, choice) change
//! it and trigger [`ValueChanged`] so a screen can react, either with an
//! observer on the widget or by watching the component.
//!
//! The widgets are plain components and systems, so a screen is just a
//! function that spawns them.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::picking::Pickable;
use bevy::picking::events::{
    PointerClick, PointerDrag, PointerOut, PointerOver, PointerPress, PointerRelease,
};
use bevy::prelude::*;

use super::anim::Smooth;
use super::theme::{self, Fonts};

/// Which of the menu's looks a button has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// The main action of a screen.
    Primary,
    /// An ordinary action.
    Secondary,
    /// An action that throws something away or leaves.
    Danger,
    /// Barely there: back links and small actions.
    Ghost,
}

/// The colours of a widget at rest, hovered and pressed.
#[derive(Component, Debug, Clone, Copy)]
pub struct Look {
    /// At rest.
    pub idle: Color,
    /// Under the pointer, or focused.
    pub hover: Color,
    /// Being pressed.
    pub pressed: Color,
}

impl Look {
    /// The look of a button of `style`.
    #[must_use]
    pub fn of(style: ButtonStyle) -> Self {
        match style {
            ButtonStyle::Primary => Self {
                idle: theme::ACCENT,
                hover: theme::ACCENT_HOVER,
                pressed: theme::ACCENT,
            },
            ButtonStyle::Secondary => Self {
                idle: theme::CONTROL,
                hover: theme::CONTROL_HOVER,
                pressed: theme::CONTROL_PRESSED,
            },
            ButtonStyle::Danger => Self {
                idle: theme::DANGER.with_alpha(0.09),
                hover: theme::DANGER.with_alpha(0.2),
                pressed: theme::DANGER.with_alpha(0.06),
            },
            ButtonStyle::Ghost => Self {
                idle: Color::NONE,
                hover: theme::CONTROL,
                pressed: theme::CONTROL_PRESSED,
            },
        }
    }
}

/// The pointer's relation to a widget.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Interact {
    /// The pointer is over it.
    pub hovered: bool,
    /// A pointer button is held on it.
    pub pressed: bool,
}

/// A widget the keyboard can focus.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct Focusable;

/// A widget that ignores the pointer and keyboard and looks dimmed.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct Disabled;

/// The widget the keyboard is on.
#[derive(Resource, Debug, Default)]
pub struct Focus(pub Option<Entity>);

/// A widget was activated (clicked, or Enter pressed on it).
#[derive(EntityEvent)]
pub struct Activate {
    /// The widget.
    pub entity: Entity,
}

/// A widget's value changed.
#[derive(EntityEvent)]
pub struct ValueChanged {
    /// The widget.
    pub entity: Entity,
}

/// The root of a screen's widgets: keyboard focus moves among the
/// [`Focusable`]s below it.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct FocusScope;

/// A two-state switch.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Toggle(pub bool);

/// A value between `min` and `max`, in steps of `step`.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Slider {
    /// The value.
    pub value: f32,
    /// Smallest value.
    pub min: f32,
    /// Largest value.
    pub max: f32,
    /// Granularity of values (0 for continuous).
    pub step: f32,
    start: f32,
}

impl Slider {
    /// A slider at `value`.
    #[must_use]
    pub fn new(value: f32, min: f32, max: f32, step: f32) -> Self {
        Self {
            value: value.clamp(min, max),
            min,
            max,
            step,
            start: 0.0,
        }
    }

    /// Where the value sits between the ends, `0..=1`.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        if self.max <= self.min {
            return 0.0;
        }
        ((self.value - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
    }

    /// Sets the value from a position `fraction` of the way along,
    /// snapped to the step.
    pub fn set_fraction(&mut self, fraction: f32) {
        let raw = self.min + fraction.clamp(0.0, 1.0) * (self.max - self.min);
        self.value = self.snap(raw);
    }

    /// Moves the value by `steps` steps (a tenth of the range for a
    /// continuous slider).
    pub fn nudge(&mut self, steps: i32) {
        let unit = if self.step > 0.0 {
            self.step
        } else {
            (self.max - self.min) / 10.0
        };
        self.value = self.snap(self.value + unit * steps as f32);
    }

    fn snap(&self, value: f32) -> f32 {
        let value = if self.step > 0.0 {
            self.min + ((value - self.min) / self.step).round() * self.step
        } else {
            value
        };
        value.clamp(self.min, self.max)
    }
}

/// One of a list of options, cycled with arrows.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// The options' names.
    pub options: Vec<String>,
    /// Which one is chosen.
    pub index: usize,
}

impl Choice {
    /// Cycles by `delta` options, wrapping around.
    pub fn cycle(&mut self, delta: i32) {
        let count = self.options.len() as i32;
        if count > 0 {
            self.index = (self.index as i32 + delta).rem_euclid(count) as usize;
        }
    }

    /// The name of the chosen option.
    #[must_use]
    pub fn current(&self) -> &str {
        self.options.get(self.index).map_or("", String::as_str)
    }
}

/// A line of editable text.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct TextField {
    /// What was typed.
    pub value: String,
    /// Shown, dimmed, while empty.
    pub placeholder: String,
    /// Most characters accepted.
    pub max_chars: usize,
}

/// Adds the widget systems and observers.
pub struct WidgetsPlugin;

impl Plugin for WidgetsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Focus>()
            .add_observer(pointer_over)
            .add_observer(pointer_out)
            .add_observer(pointer_press)
            .add_observer(pointer_release)
            .add_observer(pointer_click)
            .add_observer(slider_press)
            .add_observer(slider_drag)
            .add_observer(activate_toggle)
            .add_observer(activate_choice)
            .add_observer(activate_field)
            .add_systems(
                Update,
                (
                    navigate,
                    type_into_fields,
                    show_toggles,
                    show_sliders,
                    show_choices,
                    show_fields,
                    show_focus,
                    show_looks,
                )
                    .chain(),
            );
    }
}

// ---------------------------------------------------------------- builders

/// A button: `label` on a rounded, animated background.
#[must_use]
pub fn button(fonts: &Fonts, label: &str, style: ButtonStyle) -> impl Bundle + use<> {
    let look = Look::of(style);
    let text_color = if style == ButtonStyle::Primary {
        theme::ON_ACCENT
    } else {
        theme::TEXT
    };
    (
        Node {
            min_height: Val::Px(theme::ROW_HEIGHT),
            padding: UiRect::axes(Val::Px(26.0), Val::Px(6.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(Val::Px(theme::CONTROL_RADIUS)),
            ..default()
        },
        BackgroundColor(look.idle),
        Outline::new(Val::Px(2.0), Val::Px(1.0), Color::NONE),
        UiTransform::IDENTITY,
        Smooth::new(look.idle),
        look,
        Interact::default(),
        Focusable,
        children![fonts.bold(label, 24.0, text_color)],
    )
}

/// A labelled switch: the label on the left, the switch on the right.
#[must_use]
pub fn toggle(fonts: &Fonts, label: &str, on: bool) -> impl Bundle + use<> {
    (
        row_node(),
        BackgroundColor(theme::CONTROL),
        Outline::new(Val::Px(2.0), Val::Px(1.0), Color::NONE),
        UiTransform::IDENTITY,
        Smooth::new(theme::CONTROL),
        Look::of(ButtonStyle::Secondary),
        Interact::default(),
        Focusable,
        Toggle(on),
        children![
            fonts.text(label, theme::TEXT_SIZE, theme::TEXT),
            (
                Node {
                    width: Val::Px(54.0),
                    height: Val::Px(28.0),
                    padding: UiRect::all(Val::Px(3.0)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BackgroundColor(theme::BORDER),
                Pickable::IGNORE,
                ToggleTrack,
                children![(
                    Node {
                        width: Val::Px(22.0),
                        height: Val::Px(22.0),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BackgroundColor(theme::TEXT),
                    Pickable::IGNORE,
                    UiTransform::IDENTITY,
                    ToggleKnobMark,
                )],
            )
        ],
    )
}

/// The track of a toggle (private marker).
#[derive(Component)]
struct ToggleTrack;

/// The knob of a toggle (private marker).
#[derive(Component)]
struct ToggleKnobMark;

/// A labelled slider with its value shown.
#[must_use]
pub fn slider(
    fonts: &Fonts,
    label: &str,
    slider: Slider,
    format: fn(f32) -> String,
) -> impl Bundle + use<> {
    (
        row_node(),
        BackgroundColor(theme::CONTROL),
        Outline::new(Val::Px(2.0), Val::Px(1.0), Color::NONE),
        UiTransform::IDENTITY,
        Smooth::new(theme::CONTROL),
        Look::of(ButtonStyle::Secondary),
        Interact::default(),
        Focusable,
        slider,
        SliderFormat(format),
        children![
            (
                Node {
                    width: Val::Percent(34.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![fonts.text(label, theme::TEXT_SIZE, theme::TEXT)],
            ),
            (
                Node {
                    flex_grow: 1.0,
                    height: Val::Px(8.0),
                    margin: UiRect::horizontal(Val::Px(14.0)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BackgroundColor(theme::BORDER),
                Pickable::IGNORE,
                children![(
                    Node {
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BackgroundColor(theme::ACCENT),
                    Pickable::IGNORE,
                    SliderFillMark,
                )],
            ),
            (
                Node {
                    width: Val::Px(64.0),
                    justify_content: JustifyContent::End,
                    ..default()
                },
                Pickable::IGNORE,
                children![(
                    fonts.bold("", theme::TEXT_SIZE, theme::ACCENT),
                    SliderValueMark
                )],
            ),
        ],
    )
}

/// Formats a slider's value for display.
#[derive(Component, Clone, Copy)]
pub struct SliderFormat(pub fn(f32) -> String);

#[derive(Component)]
struct SliderFillMark;

#[derive(Component)]
struct SliderValueMark;

/// A labelled chooser: the label, and `< value >` that cycles on click.
#[must_use]
pub fn choice(fonts: &Fonts, label: &str, choice: Choice) -> impl Bundle + use<> {
    (
        row_node(),
        BackgroundColor(theme::CONTROL),
        Outline::new(Val::Px(2.0), Val::Px(1.0), Color::NONE),
        UiTransform::IDENTITY,
        Smooth::new(theme::CONTROL),
        Look::of(ButtonStyle::Secondary),
        Interact::default(),
        Focusable,
        choice,
        children![
            fonts.text(label, theme::TEXT_SIZE, theme::TEXT),
            (
                Node {
                    column_gap: Val::Px(12.0),
                    align_items: AlignItems::Center,
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    fonts.bold("<", theme::TEXT_SIZE, theme::TEXT_DIM),
                    (
                        fonts.bold("", theme::TEXT_SIZE, theme::ACCENT),
                        ChoiceValueMark
                    ),
                    fonts.bold(">", theme::TEXT_SIZE, theme::TEXT_DIM),
                ],
            ),
        ],
    )
}

#[derive(Component)]
struct ChoiceValueMark;

/// A labelled text field.
#[must_use]
pub fn text_field(
    fonts: &Fonts,
    label: &str,
    value: &str,
    placeholder: &str,
    max_chars: usize,
) -> impl Bundle + use<> {
    (
        row_node(),
        BackgroundColor(theme::CONTROL),
        Outline::new(Val::Px(2.0), Val::Px(1.0), Color::NONE),
        UiTransform::IDENTITY,
        Smooth::new(theme::CONTROL),
        Look::of(ButtonStyle::Secondary),
        Interact::default(),
        Focusable,
        TextField {
            value: value.to_owned(),
            placeholder: placeholder.to_owned(),
            max_chars,
        },
        children![
            fonts.text(label, theme::TEXT_SIZE, theme::TEXT),
            (
                fonts.bold("", theme::TEXT_SIZE, theme::ACCENT),
                FieldValueMark
            ),
        ],
    )
}

#[derive(Component)]
struct FieldValueMark;

/// A row for a labelled setting.
fn row_node() -> Node {
    Node {
        width: Val::Percent(100.0),
        min_height: Val::Px(theme::ROW_HEIGHT),
        padding: UiRect::axes(Val::Px(18.0), Val::Px(6.0)),
        justify_content: JustifyContent::SpaceBetween,
        align_items: AlignItems::Center,
        border_radius: BorderRadius::all(Val::Px(theme::CONTROL_RADIUS)),
        ..default()
    }
}

/// A panel for a screen's content, `width` wide and at most `max_height`
/// tall.
#[must_use]
pub fn panel(width: Val, max_height: Val) -> impl Bundle + use<> {
    (
        Node {
            width,
            max_height,
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(28.0)),
            row_gap: Val::Px(10.0),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
            ..default()
        },
        BackgroundColor(theme::PANEL),
        BorderColor::all(theme::BORDER),
        BoxShadow::new(
            Color::srgba(0.0, 0.0, 0.0, 0.5),
            Val::Px(0.0),
            Val::Px(18.0),
            Val::Px(0.0),
            Val::Px(40.0),
        ),
    )
}

// --------------------------------------------------------------- pointer

fn pointer_over(over: On<PointerOver>, mut widgets: Query<&mut Interact, Without<Disabled>>) {
    if let Ok(mut interact) = widgets.get_mut(over.entity) {
        interact.hovered = true;
    }
}

fn pointer_out(out: On<PointerOut>, mut widgets: Query<&mut Interact>) {
    if let Ok(mut interact) = widgets.get_mut(out.entity) {
        interact.hovered = false;
        interact.pressed = false;
    }
}

fn pointer_press(press: On<PointerPress>, mut widgets: Query<&mut Interact, Without<Disabled>>) {
    if let Ok(mut interact) = widgets.get_mut(press.entity) {
        interact.pressed = true;
    }
}

fn pointer_release(release: On<PointerRelease>, mut widgets: Query<&mut Interact>) {
    if let Ok(mut interact) = widgets.get_mut(release.entity) {
        interact.pressed = false;
    }
}

fn pointer_click(
    click: On<PointerClick>,
    widgets: Query<(), (With<Interact>, Without<Disabled>)>,
    mut focus: ResMut<Focus>,
    mut commands: Commands,
) {
    if widgets.contains(click.entity) {
        focus.0 = Some(click.entity);
        commands.trigger(Activate {
            entity: click.entity,
        });
    }
}

/// Pressing on a slider sets its value there; dragging moves it.
fn slider_press(
    press: On<PointerPress>,
    mut sliders: Query<&mut Slider, Without<Disabled>>,
    mut commands: Commands,
) {
    let Ok(mut slider) = sliders.get_mut(press.entity) else {
        return;
    };
    // The hit position is normalised over the whole widget; the track is
    // most of it, so this is close enough to put the knob under the cursor.
    let Some(position) = press.hit.position else {
        return;
    };
    slider.set_fraction(slider_fraction_from_hit(position.x));
    slider.start = slider.fraction();
    commands.trigger(ValueChanged {
        entity: press.entity,
    });
}

fn slider_drag(
    drag: On<PointerDrag>,
    mut sliders: Query<(&mut Slider, &ComputedNode), Without<Disabled>>,
    mut commands: Commands,
) {
    let Ok((mut slider, node)) = sliders.get_mut(drag.entity) else {
        return;
    };
    let width = node.size().x * node.inverse_scale_factor();
    if width <= 0.0 {
        return;
    }
    let before = slider.value;
    let fraction = slider.start + drag.distance.x / (width * TRACK_SHARE);
    slider.set_fraction(fraction);
    if (slider.value - before).abs() > f32::EPSILON {
        commands.trigger(ValueChanged {
            entity: drag.entity,
        });
    }
}

/// The part of a slider row its track covers, roughly: used to make a
/// drag across the track move the knob with the cursor.
const TRACK_SHARE: f32 = 0.5;

/// The slider fraction under a hit at normalised `x` (`-0.5..=0.5` across
/// the row): the row's label takes the first third and the value the last
/// tenth, so the track is mapped onto what is between.
#[must_use]
pub fn slider_fraction_from_hit(x: f32) -> f32 {
    let across = x + 0.5;
    ((across - 0.34) / (0.9 - 0.34)).clamp(0.0, 1.0)
}

fn activate_toggle(
    activate: On<Activate>,
    mut toggles: Query<&mut Toggle>,
    mut commands: Commands,
) {
    if let Ok(mut toggle) = toggles.get_mut(activate.entity) {
        toggle.0 = !toggle.0;
        commands.trigger(ValueChanged {
            entity: activate.entity,
        });
    }
}

fn activate_choice(
    activate: On<Activate>,
    mut choices: Query<&mut Choice>,
    mut commands: Commands,
) {
    if let Ok(mut choice) = choices.get_mut(activate.entity) {
        choice.cycle(1);
        commands.trigger(ValueChanged {
            entity: activate.entity,
        });
    }
}

fn activate_field(
    activate: On<Activate>,
    fields: Query<(), With<TextField>>,
    mut focus: ResMut<Focus>,
) {
    if fields.contains(activate.entity) {
        focus.0 = Some(activate.entity);
    }
}

// -------------------------------------------------------------- keyboard

/// Widgets the keyboard can land on.
type Navigable = (With<Focusable>, Without<Disabled>);

/// Moves focus with Up/Down (and Tab), activates with Enter/Space and
/// adjusts the focused slider or choice with Left/Right.
fn navigate(
    keys: Res<ButtonInput<KeyCode>>,
    mut focus: ResMut<Focus>,
    focusables: Query<(Entity, &UiGlobalTransform), Navigable>,
    mut sliders: Query<&mut Slider>,
    mut choices: Query<&mut Choice>,
    fields: Query<(), With<TextField>>,
    mut commands: Commands,
) {
    let typing = focus.0.is_some_and(|entity| fields.contains(entity));
    let down = keys.just_pressed(KeyCode::ArrowDown)
        || (!typing && keys.just_pressed(KeyCode::KeyS))
        || (keys.just_pressed(KeyCode::Tab) && !keys.pressed(KeyCode::ShiftLeft));
    let up = keys.just_pressed(KeyCode::ArrowUp)
        || (!typing && keys.just_pressed(KeyCode::KeyW))
        || (keys.just_pressed(KeyCode::Tab) && keys.pressed(KeyCode::ShiftLeft));
    if down || up {
        let mut order: Vec<(Entity, f32)> = focusables
            .iter()
            .map(|(entity, transform)| (entity, transform.translation.y))
            .collect();
        order.sort_by(|a, b| a.1.total_cmp(&b.1));
        let next = focus_after(&order, focus.0, if down { 1 } else { -1 });
        if next.is_some() {
            focus.0 = next;
        }
    }
    let Some(focused) = focus.0.filter(|&entity| focusables.contains(entity)) else {
        return;
    };
    let sideways = i32::from(keys.just_pressed(KeyCode::ArrowRight))
        - i32::from(keys.just_pressed(KeyCode::ArrowLeft));
    if sideways != 0 {
        if let Ok(mut slider) = sliders.get_mut(focused) {
            slider.nudge(sideways);
            commands.trigger(ValueChanged { entity: focused });
        } else if let Ok(mut choice) = choices.get_mut(focused) {
            choice.cycle(sideways);
            commands.trigger(ValueChanged { entity: focused });
        }
    }
    let activate = keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::NumpadEnter)
        || (!typing && keys.just_pressed(KeyCode::Space));
    if activate && !fields.contains(focused) {
        commands.trigger(Activate { entity: focused });
    }
}

/// The widget `delta` places after `current` in `order` (top to bottom),
/// wrapping; the first (or last, going up) when nothing is focused.
#[must_use]
pub fn focus_after(order: &[(Entity, f32)], current: Option<Entity>, delta: i32) -> Option<Entity> {
    if order.is_empty() {
        return None;
    }
    let count = order.len() as i32;
    let position = current.and_then(|entity| order.iter().position(|(e, _)| *e == entity));
    let target = match position {
        Some(position) => (position as i32 + delta).rem_euclid(count),
        None if delta > 0 => 0,
        None => count - 1,
    };
    Some(order[target as usize].0)
}

fn type_into_fields(
    mut typed: MessageReader<KeyboardInput>,
    focus: Res<Focus>,
    mut fields: Query<(&mut TextField, &Interact)>,
    mut commands: Commands,
) {
    let Some(entity) = focus.0 else {
        typed.clear();
        return;
    };
    let Ok((mut field, _)) = fields.get_mut(entity) else {
        typed.clear();
        return;
    };
    let mut changed = false;
    for event in typed.read().filter(|event| event.state.is_pressed()) {
        match &event.logical_key {
            Key::Backspace => changed |= field.value.pop().is_some(),
            Key::Character(text) => {
                for character in text.chars().filter(|c| !c.is_control()) {
                    if field.value.chars().count() < field.max_chars {
                        field.value.push(character);
                        changed = true;
                    }
                }
            }
            Key::Space if field.value.chars().count() < field.max_chars => {
                field.value.push(' ');
                changed = true;
            }
            _ => {}
        }
    }
    if changed {
        commands.trigger(ValueChanged { entity });
    }
}

// --------------------------------------------------------------- display

/// What `show_looks` reads and drives on a widget.
type LookItem = (
    Entity,
    &'static Look,
    &'static Interact,
    &'static mut Smooth,
    Option<&'static mut Outline>,
    Has<Disabled>,
);

/// A widget whose look may need updating.
type LookChanged = Or<(Changed<Interact>, Changed<Look>, Added<Smooth>)>;

fn show_looks(focus: Res<Focus>, mut widgets: Query<LookItem, LookChanged>) {
    for (entity, look, interact, mut smooth, outline, disabled) in &mut widgets {
        let focused = focus.0 == Some(entity);
        let (color, scale) = if disabled {
            (look.idle.with_alpha(look.idle.alpha() * 0.4), 1.0)
        } else if interact.pressed {
            (look.pressed, 0.985)
        } else if interact.hovered || focused {
            (look.hover, 1.012)
        } else {
            (look.idle, 1.0)
        };
        smooth.set_color(color);
        smooth.scale = scale;
        if let Some(mut outline) = outline {
            outline.color = if focused {
                theme::ACCENT.with_alpha(0.85)
            } else {
                Color::NONE
            };
        }
    }
}

/// Refreshes the focus ring and highlight when focus moves.
fn show_focus(focus: Res<Focus>, mut interacts: Query<&mut Interact>) {
    if !focus.is_changed() {
        return;
    }
    // Touching every widget's `Interact` re-runs `show_looks` for it.
    for mut interact in &mut interacts {
        interact.set_changed();
    }
}

fn show_toggles(
    toggles: Query<(&Toggle, &Children), Changed<Toggle>>,
    tracks: Query<&Children, With<ToggleTrack>>,
    mut knobs: Query<&mut UiTransform, With<ToggleKnobMark>>,
    mut backgrounds: Query<&mut BackgroundColor>,
) {
    for (toggle, children) in &toggles {
        for &child in children {
            let Ok(track_children) = tracks.get(child) else {
                continue;
            };
            if let Ok(mut track) = backgrounds.get_mut(child) {
                track.0 = if toggle.0 {
                    theme::ACCENT
                } else {
                    theme::BORDER
                };
            }
            for &knob in track_children {
                if let Ok(mut transform) = knobs.get_mut(knob) {
                    transform.translation = Val2::px(if toggle.0 { 26.0 } else { 0.0 }, 0.0);
                }
            }
        }
    }
}

fn show_sliders(
    sliders: Query<(&Slider, &SliderFormat, &Children), Changed<Slider>>,
    children_of: Query<&Children>,
    mut fills: Query<&mut Node, With<SliderFillMark>>,
    mut values: Query<&mut Text, With<SliderValueMark>>,
) {
    for (slider, format, children) in &sliders {
        for &child in children {
            let Ok(grandchildren) = children_of.get(child) else {
                continue;
            };
            for &node in grandchildren {
                if let Ok(mut fill) = fills.get_mut(node) {
                    fill.width = Val::Percent(slider.fraction() * 100.0);
                }
                if let Ok(mut text) = values.get_mut(node) {
                    text.0 = (format.0)(slider.value);
                }
            }
        }
    }
}

fn show_choices(
    choices: Query<(&Choice, &Children), Changed<Choice>>,
    children_of: Query<&Children>,
    mut values: Query<&mut Text, With<ChoiceValueMark>>,
) {
    for (choice, children) in &choices {
        for &child in children {
            let Ok(grandchildren) = children_of.get(child) else {
                continue;
            };
            for &node in grandchildren {
                if let Ok(mut text) = values.get_mut(node) {
                    choice.current().clone_into(&mut text.0);
                }
            }
        }
    }
}

/// A text field whose text or focus may have changed.
type FieldChanged = Or<(Changed<TextField>, Changed<Interact>)>;

fn show_fields(
    focus: Res<Focus>,
    fields: Query<(Entity, &TextField, &Children), FieldChanged>,
    mut values: Query<(&mut Text, &mut TextColor), With<FieldValueMark>>,
) {
    for (entity, field, children) in &fields {
        for &child in children {
            if let Ok((mut text, mut color)) = values.get_mut(child) {
                let caret = if focus.0 == Some(entity) { "|" } else { "" };
                if field.value.is_empty() {
                    text.0 = format!("{}{caret}", field.placeholder);
                    color.0 = theme::TEXT_DIM;
                } else {
                    text.0 = format!("{}{caret}", field.value);
                    color.0 = theme::ACCENT;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliders_snap_to_their_step_and_stay_in_range() {
        let mut slider = Slider::new(0.5, 0.0, 1.0, 0.1);
        slider.set_fraction(0.34);
        assert!((slider.value - 0.3).abs() < 1e-6, "{}", slider.value);
        slider.set_fraction(7.0);
        assert_eq!(slider.value, 1.0);
        slider.set_fraction(-1.0);
        assert_eq!(slider.value, 0.0);
        assert_eq!(slider.fraction(), 0.0);
        slider.nudge(3);
        assert!((slider.value - 0.3).abs() < 1e-6);
        slider.nudge(-100);
        assert_eq!(slider.value, 0.0);
        assert_eq!(Slider::new(5.0, 0.0, 1.0, 0.1).value, 1.0);
    }

    #[test]
    fn continuous_sliders_nudge_by_a_tenth_of_their_range() {
        let mut slider = Slider::new(60.0, 60.0, 120.0, 0.0);
        slider.nudge(1);
        assert!((slider.value - 66.0).abs() < 1e-4);
        assert!((slider.fraction() - 0.1).abs() < 1e-4);
        assert_eq!(Slider::new(1.0, 1.0, 1.0, 0.0).fraction(), 0.0);
    }

    #[test]
    fn choices_wrap_both_ways() {
        let mut choice = Choice {
            options: vec!["a".into(), "b".into(), "c".into()],
            index: 0,
        };
        choice.cycle(-1);
        assert_eq!(choice.current(), "c");
        choice.cycle(1);
        choice.cycle(1);
        assert_eq!(choice.current(), "b");
        let mut empty = Choice {
            options: Vec::new(),
            index: 0,
        };
        empty.cycle(1);
        assert_eq!(empty.current(), "");
    }

    #[test]
    fn focus_moves_down_and_up_the_list_and_wraps() {
        let ids: Vec<Entity> = (0..3)
            .map(Entity::from_raw_u32)
            .map(Option::unwrap)
            .collect();
        let order: Vec<(Entity, f32)> = ids.iter().copied().zip([0.0, 1.0, 2.0]).collect();
        assert_eq!(focus_after(&order, None, 1), Some(ids[0]));
        assert_eq!(focus_after(&order, None, -1), Some(ids[2]));
        assert_eq!(focus_after(&order, Some(ids[0]), 1), Some(ids[1]));
        assert_eq!(focus_after(&order, Some(ids[2]), 1), Some(ids[0]));
        assert_eq!(focus_after(&order, Some(ids[0]), -1), Some(ids[2]));
        assert_eq!(focus_after(&[], None, 1), None);
    }

    #[test]
    fn a_click_on_the_track_maps_to_a_fraction() {
        assert_eq!(slider_fraction_from_hit(-0.5), 0.0);
        assert_eq!(slider_fraction_from_hit(0.5), 1.0);
        let middle = slider_fraction_from_hit(0.12);
        assert!((0.4..0.6).contains(&middle), "{middle}");
    }
}
