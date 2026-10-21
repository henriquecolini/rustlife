//! Health and armor, bottom left: a big number and a bar that drains with a
//! trailing ghost of what was just lost.

use bevy::prelude::*;

use super::anchored;
use crate::net::Game;
use crate::player::Gameplay;
use crate::ui::anim::smooth_toward;
use crate::ui::{Fonts, theme};

/// Armor's bar colour.
const ARMOR: Color = Color::srgb(0.29, 0.64, 1.0);
/// How fast the ghost of lost health catches up, per second.
const GHOST_RATE: f32 = 2.5;
/// How fast the bars follow the value, per second.
const BAR_RATE: f32 = 18.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Vital {
    Health,
    Armor,
}

/// The number of a vital.
#[derive(Component)]
pub(super) struct VitalText(Vital);

/// The coloured part of a vital's bar, with its width as a fraction.
#[derive(Component)]
pub(super) struct VitalFill {
    vital: Vital,
    shown: f32,
}

/// The pale trail behind a bar's fill.
#[derive(Component)]
pub(super) struct VitalGhost {
    vital: Vital,
    shown: f32,
}

pub(super) fn spawn(commands: &mut Commands, fonts: &Fonts) {
    commands
        .spawn(anchored(
            JustifyContent::FlexStart,
            AlignItems::FlexEnd,
            UiRect::new(Val::Px(32.0), Val::Px(0.0), Val::Px(0.0), Val::Px(30.0)),
        ))
        .with_children(|root| {
            root.spawn(Node {
                column_gap: Val::Px(26.0),
                align_items: AlignItems::FlexEnd,
                ..default()
            })
            .with_children(|row| {
                vital(row, fonts, Vital::Health, "HEALTH", theme::TEXT);
                vital(row, fonts, Vital::Armor, "ARMOR", ARMOR);
            });
        });
}

fn vital(row: &mut ChildSpawnerCommands, fonts: &Fonts, vital: Vital, label: &str, tint: Color) {
    row.spawn((
        Node {
            flex_direction: FlexDirection::Column,
            padding: UiRect::axes(Val::Px(18.0), Val::Px(10.0)),
            row_gap: Val::Px(2.0),
            min_width: Val::Px(200.0),
            border_radius: BorderRadius::all(Val::Px(theme::CONTROL_RADIUS)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.62)),
    ))
    .with_children(|card| {
        card.spawn(fonts.bold(label, 16.0, theme::TEXT_DIM));
        card.spawn((VitalText(vital), fonts.bold("100", 56.0, tint)));
        card.spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Px(7.0),
                margin: UiRect::top(Val::Px(2.0)),
                border_radius: BorderRadius::MAX,
                ..default()
            },
            BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.12)),
        ))
        .with_children(|bar| {
            for ghost in [true, false] {
                let node = Node {
                    position_type: PositionType::Absolute,
                    height: Val::Percent(100.0),
                    width: Val::Percent(100.0),
                    border_radius: BorderRadius::MAX,
                    ..default()
                };
                if ghost {
                    bar.spawn((
                        node,
                        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.5)),
                        VitalGhost { vital, shown: 1.0 },
                    ));
                } else {
                    bar.spawn((
                        node,
                        BackgroundColor(if vital == Vital::Armor {
                            ARMOR
                        } else {
                            theme::ACCENT
                        }),
                        VitalFill { vital, shown: 1.0 },
                    ));
                }
            }
        });
    });
}

pub(super) fn update(
    time: Res<Time<Real>>,
    game: Game,
    gameplay: Res<Gameplay>,
    mut texts: Query<(&VitalText, &mut Text, &mut TextColor)>,
    mut fills: Query<(&mut VitalFill, &mut Node, &mut BackgroundColor), Without<VitalGhost>>,
    mut ghosts: Query<(&mut VitalGhost, &mut Node), Without<VitalFill>>,
) {
    let Some(player) = game
        .sim()
        .zip(game.local())
        .and_then(|(sim, local)| sim.player(local))
    else {
        return;
    };
    let data = &gameplay.0;
    let dt = time.delta_secs();
    let value = |vital: Vital| match vital {
        Vital::Health => player.vitals.health.max(0.0),
        Vital::Armor => player.vitals.armor.max(0.0),
    };
    let target = |vital: Vital| {
        let max = match vital {
            Vital::Health => data.player.health.max,
            Vital::Armor => data.player.armor.max,
        };
        (value(vital) / max.max(1.0)).clamp(0.0, 1.0)
    };
    let low = value(Vital::Health) <= data.hud.low_health;
    let pulse = 0.55 + 0.45 * (time.elapsed_secs() * 6.0).sin().abs();
    for (VitalText(vital), mut text, mut color) in &mut texts {
        let line = format!("{:.0}", value(*vital).ceil());
        if text.0 != line {
            text.0 = line;
        }
        color.0 = match vital {
            Vital::Health if low => theme::DANGER.with_alpha(pulse),
            Vital::Health => theme::TEXT,
            Vital::Armor => ARMOR,
        };
    }
    for (mut fill, mut node, mut background) in &mut fills {
        fill.shown = smooth_toward(fill.shown, target(fill.vital), BAR_RATE, dt);
        node.width = Val::Percent(fill.shown * 100.0);
        if fill.vital == Vital::Health {
            background.0 = if low { theme::DANGER } else { theme::ACCENT };
        }
    }
    for (mut ghost, mut node) in &mut ghosts {
        let wanted = target(ghost.vital);
        // Growth is instant; loss drains away slowly behind the fill.
        ghost.shown = if wanted > ghost.shown {
            wanted
        } else {
            smooth_toward(ghost.shown, wanted, GHOST_RATE, dt)
        };
        node.width = Val::Percent(ghost.shown * 100.0);
    }
}
