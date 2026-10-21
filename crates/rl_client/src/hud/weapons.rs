//! The weapon in hand's ammo (bottom right) and the strip of carried
//! weapons (bottom centre).

use bevy::prelude::*;

use super::anchored;
use super::text::{self, Ammo};
use crate::net::Game;
use crate::ui::{Fonts, theme};

/// Most weapons the strip has room for.
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

/// One cell of the strip.
#[derive(Component)]
pub(super) struct SlotCell(usize);

/// The slot number and name inside a cell.
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
            root.spawn((
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
                        numbers
                            .spawn((AmmoLine(AmmoText::Main), fonts.bold("", 64.0, theme::TEXT)));
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
        });
    commands
        .spawn(anchored(
            JustifyContent::Center,
            AlignItems::FlexEnd,
            UiRect::bottom(Val::Px(30.0)),
        ))
        .with_children(|root| {
            root.spawn(Node {
                column_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|strip| {
                for index in 0..SLOTS {
                    strip
                        .spawn((
                            SlotCell(index),
                            Node {
                                flex_direction: FlexDirection::Column,
                                align_items: AlignItems::Center,
                                padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                                min_width: Val::Px(82.0),
                                border: UiRect::bottom(Val::Px(3.0)),
                                border_radius: BorderRadius::all(Val::Px(6.0)),
                                display: Display::None,
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.55)),
                            BorderColor::all(Color::NONE),
                        ))
                        .with_children(|cell| {
                            cell.spawn((SlotNumber(index), fonts.bold("", 18.0, theme::ACCENT)));
                            cell.spawn((SlotName(index), fonts.text("", 17.0, theme::TEXT)));
                        });
                }
            });
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

/// The strip of carried weapons.
pub(super) fn update_strip(
    game: Game,
    mut cells: Query<(&SlotCell, &mut Node, &mut BackgroundColor, &mut BorderColor)>,
    mut numbers: Query<(&SlotNumber, &mut Text)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let Some(player) = sim.player(local) else {
        return;
    };
    let slots = text::weapon_slots(&player.inventory, sim.arsenal());
    for (SlotCell(index), mut node, mut background, mut border) in &mut cells {
        let Some(slot) = slots.get(*index) else {
            node.display = Display::None;
            continue;
        };
        node.display = Display::Flex;
        background.0 = if slot.active {
            Color::srgba(0.12, 0.14, 0.2, 0.82)
        } else {
            Color::srgba(0.03, 0.04, 0.06, 0.55)
        };
        *border = BorderColor::all(if slot.active {
            theme::ACCENT
        } else {
            Color::NONE
        });
    }
    for (SlotNumber(index), mut text) in &mut numbers {
        let value = slots
            .get(*index)
            .map_or_else(String::new, |slot| slot.slot.to_string());
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// The names on the strip, dimmed when a weapon cannot fire.
pub(super) fn update_names(game: Game, mut names: Query<(&SlotName, &mut Text, &mut TextColor)>) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let Some(player) = sim.player(local) else {
        return;
    };
    let slots = text::weapon_slots(&player.inventory, sim.arsenal());
    for (SlotName(index), mut text, mut color) in &mut names {
        let Some(slot) = slots.get(*index) else {
            continue;
        };
        if text.0 != slot.name {
            text.0.clone_from(&slot.name);
        }
        color.0 = if !slot.usable {
            theme::TEXT_DIM.with_alpha(0.55)
        } else if slot.active {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
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
