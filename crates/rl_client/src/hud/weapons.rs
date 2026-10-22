//! The weapon in hand's ammo (bottom right) and, above it, the list of
//! carried weapons that appears while the mouse wheel is picking one.

use bevy::prelude::*;

use super::anchored;
use super::text::{self, Ammo};
use crate::net::Game;
use crate::selector::WeaponSelector;
use crate::ui::{Fonts, theme};

/// Most weapons the list has room for.
const SLOTS: usize = 10;

#[derive(Component)]
pub(super) struct AmmoPanel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AmmoText {
    Weapon,
    Main,
    Reserve,
    Secondary,
}

#[derive(Component)]
pub(super) struct AmmoLine(AmmoText);

/// A row's background, and the one the cursor is on.
const SLOT_IDLE: Color = Color::srgba(0.03, 0.04, 0.06, 0.55);
const SLOT_PICKED: Color = Color::srgba(0.12, 0.14, 0.2, 0.82);

/// One row of the list.
/// The column of rows; hidden while there is no pick under way.
#[derive(Component)]
pub(super) struct WeaponList;

#[derive(Component)]
pub(super) struct SlotCell(usize);

/// The slot number and name inside a row.
#[derive(Component)]
pub(super) struct SlotNumber(usize);

#[derive(Component)]
pub(super) struct SlotName(usize);

pub(super) fn spawn(commands: &mut Commands, fonts: &Fonts) {
    commands
        .spawn(anchored(
            JustifyContent::FlexEnd,
            AlignItems::FlexEnd,
            UiRect::new(Val::Px(0.0), Val::Px(32.0), Val::Px(0.0), Val::Px(30.0)),
        ))
        .with_children(|root| {
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(12.0),
                ..default()
            })
            .with_children(|column| {
                spawn_list(column, fonts);
                spawn_ammo(column, fonts);
            });
        });
}

/// The weapon list, one row per possible weapon.
fn spawn_list(column: &mut ChildSpawnerCommands, fonts: &Fonts) {
    column
        .spawn((
            WeaponList,
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                display: Display::None,
                ..default()
            },
        ))
        .with_children(|list| {
            for index in 0..SLOTS {
                list.spawn((
                    SlotCell(index),
                    Node {
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(12.0),
                        padding: UiRect::axes(Val::Px(14.0), Val::Px(6.0)),
                        min_width: Val::Px(190.0),
                        border: UiRect::right(Val::Px(3.0)),
                        border_radius: BorderRadius::all(Val::Px(6.0)),
                        display: Display::None,
                        ..default()
                    },
                    BackgroundColor(SLOT_IDLE),
                    BorderColor::all(Color::NONE),
                ))
                .with_children(|row| {
                    row.spawn((SlotNumber(index), fonts.bold("", 18.0, theme::ACCENT)));
                    row.spawn((SlotName(index), fonts.text("", 17.0, theme::TEXT)));
                });
            }
        });
}

/// The ammo panel's nodes.
fn spawn_ammo(column: &mut ChildSpawnerCommands, fonts: &Fonts) {
    column
        .spawn((
            AmmoPanel,
            Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                padding: UiRect::axes(Val::Px(22.0), Val::Px(10.0)),
                min_width: Val::Px(190.0),
                border_radius: BorderRadius::all(Val::Px(theme::CONTROL_RADIUS)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.62)),
        ))
        .with_children(|panel| {
            panel.spawn((
                AmmoLine(AmmoText::Weapon),
                fonts.bold("", 16.0, theme::TEXT_DIM),
            ));
            panel
                .spawn(Node {
                    align_items: AlignItems::FlexEnd,
                    column_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|numbers| {
                    numbers.spawn((AmmoLine(AmmoText::Main), fonts.bold("", 64.0, theme::TEXT)));
                    numbers.spawn((
                        AmmoLine(AmmoText::Reserve),
                        Node {
                            margin: UiRect::bottom(Val::Px(8.0)),
                            ..default()
                        },
                        fonts.bold("", 30.0, theme::TEXT_DIM),
                    ));
                });
            panel.spawn((
                AmmoLine(AmmoText::Secondary),
                fonts.text("", 18.0, theme::TEXT_DIM),
            ));
        });
}

/// The ammo panel.
pub(super) fn update_ammo(
    time: Res<Time<Real>>,
    game: Game,
    mut lines: Query<(&AmmoLine, &mut Text, &mut TextColor)>,
    mut panel: Query<&mut Node, With<AmmoPanel>>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let Some(player) = sim.player(local) else {
        return;
    };
    let ammo = text::ammo(&player.inventory, sim.arsenal());
    let blink = if ammo.reloading {
        0.5 + 0.5 * (time.elapsed_secs() * 8.0).sin().abs()
    } else {
        1.0
    };
    for (line, mut text, mut color) in &mut lines {
        let (value, tint) = ammo_line(line.0, &ammo, blink);
        if text.0 != value {
            text.0 = value;
        }
        color.0 = tint;
    }
    for mut node in &mut panel {
        let display = if ammo.main.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
}

/// The list of carried weapons, with the cursor's pick lit; shown (and
/// faded) as the selector says.
pub(super) fn update_list(
    game: Game,
    selector: Res<WeaponSelector>,
    mut list: Query<&mut Node, (With<WeaponList>, Without<SlotCell>)>,
    mut cells: Query<(&SlotCell, &mut Node, &mut BackgroundColor, &mut BorderColor)>,
    mut numbers: Query<(&SlotNumber, &mut Text, &mut TextColor), Without<SlotName>>,
    mut names: Query<(&SlotName, &mut Text, &mut TextColor), Without<SlotNumber>>,
) {
    let opacity = selector.opacity();
    let display = if opacity > 0.0 {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut list {
        if node.display != display {
            node.display = display;
        }
    }
    if opacity <= 0.0 {
        return;
    }
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let Some(player) = sim.player(local) else {
        return;
    };
    let slots = text::weapon_slots(&player.inventory, sim.arsenal());
    let picked = selector.cursor();
    let is_picked = |index: usize| {
        slots
            .get(index)
            .is_some_and(|slot| Some(slot.weapon) == picked)
    };
    for (SlotCell(index), mut node, mut background, mut border) in &mut cells {
        if slots.get(*index).is_none() {
            node.display = Display::None;
            continue;
        }
        node.display = Display::Flex;
        let base = if is_picked(*index) {
            SLOT_PICKED
        } else {
            SLOT_IDLE
        };
        background.0 = base.with_alpha(base.alpha() * opacity);
        *border = BorderColor::all(if is_picked(*index) {
            theme::ACCENT.with_alpha(opacity)
        } else {
            Color::NONE
        });
    }
    for (SlotNumber(index), mut text, mut color) in &mut numbers {
        let value = slots
            .get(*index)
            .map_or_else(String::new, |slot| slot.slot.to_string());
        if text.0 != value {
            text.0 = value;
        }
        color.0 = theme::ACCENT.with_alpha(opacity);
    }
    for (SlotName(index), mut text, mut color) in &mut names {
        let Some(slot) = slots.get(*index) else {
            continue;
        };
        if text.0 != slot.name {
            text.0.clone_from(&slot.name);
        }
        let tint = if !slot.usable {
            theme::TEXT_DIM.with_alpha(0.55)
        } else if is_picked(*index) {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        color.0 = tint.with_alpha(tint.alpha() * opacity);
    }
}

fn ammo_line(kind: AmmoText, ammo: &Ammo, blink: f32) -> (String, Color) {
    match kind {
        AmmoText::Weapon => (
            if ammo.reloading {
                format!("{} · reloading", ammo.weapon)
            } else {
                ammo.weapon.clone()
            },
            theme::TEXT_DIM.with_alpha(blink),
        ),
        AmmoText::Main => (
            ammo.main.map(|n| n.to_string()).unwrap_or_default(),
            if ammo.empty {
                theme::DANGER
            } else {
                theme::TEXT
            },
        ),
        AmmoText::Reserve => (
            ammo.reserve.map(|n| format!("/ {n}")).unwrap_or_default(),
            theme::TEXT_DIM,
        ),
        AmmoText::Secondary => (ammo.secondary.clone(), theme::TEXT_DIM),
    }
}
