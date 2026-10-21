//! The kill feed, top right: who killed whom, and with what.

use bevy::prelude::*;

use super::{HudLog, anchored};
use crate::player::Gameplay;
use crate::ui::{Fonts, theme};

/// One row of the feed.
#[derive(Component)]
pub(super) struct FeedRow(usize);

/// A piece of a row.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Killer,
    How,
    Victim,
}

#[derive(Component)]
pub(super) struct FeedPart(usize, Part);

pub(super) fn spawn(commands: &mut Commands, fonts: &Fonts, rows: usize) {
    commands
        .spawn(anchored(
            JustifyContent::FlexEnd,
            AlignItems::FlexStart,
            UiRect::new(Val::Px(0.0), Val::Px(28.0), Val::Px(24.0), Val::Px(0.0)),
        ))
        .with_children(|root| {
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|feed| {
                for index in 0..rows {
                    feed.spawn((
                        FeedRow(index),
                        Node {
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(10.0),
                            padding: UiRect::axes(Val::Px(14.0), Val::Px(4.0)),
                            border: UiRect::all(Val::Px(2.0)),
                            border_radius: BorderRadius::all(Val::Px(6.0)),
                            display: Display::None,
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.66)),
                        BorderColor::all(Color::NONE),
                    ))
                    .with_children(|row| {
                        row.spawn((
                            FeedPart(index, Part::Killer),
                            fonts.bold("", 21.0, theme::TEXT),
                        ));
                        row.spawn((
                            FeedPart(index, Part::How),
                            Node {
                                padding: UiRect::axes(Val::Px(8.0), Val::Px(1.0)),
                                border_radius: BorderRadius::all(Val::Px(4.0)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.1)),
                            fonts.text("", 17.0, theme::TEXT_DIM),
                        ));
                        row.spawn((
                            FeedPart(index, Part::Victim),
                            fonts.bold("", 21.0, theme::TEXT),
                        ));
                    });
                }
            });
        });
}

pub(super) fn update(
    time: Res<Time>,
    log: Res<HudLog>,
    gameplay: Res<Gameplay>,
    mut rows: Query<(&FeedRow, &mut Node, &mut BorderColor, &mut BackgroundColor)>,
    mut parts: Query<(&FeedPart, &mut Text, &mut TextColor), Without<FeedRow>>,
) {
    let now = time.elapsed_secs();
    let seconds = gameplay.0.hud.kill_feed.seconds;
    // Newest at the bottom: the feed fills from the top with the oldest.
    for (FeedRow(index), mut node, mut border, mut background) in &mut rows {
        let Some((entry, at)) = log.kills.get(*index) else {
            node.display = Display::None;
            continue;
        };
        node.display = Display::Flex;
        let fade = (seconds - (now - at)).clamp(0.0, 0.5) / 0.5;
        let involved = entry.by_local || entry.of_local;
        *border = BorderColor::all(if involved {
            (if entry.by_local {
                theme::ACCENT
            } else {
                theme::DANGER
            })
            .with_alpha(fade)
        } else {
            Color::NONE
        });
        background.0 = Color::srgba(0.03, 0.04, 0.06, 0.66 * fade);
    }
    for (FeedPart(index, part), mut text, mut color) in &mut parts {
        let Some((entry, at)) = log.kills.get(*index) else {
            continue;
        };
        let fade = (seconds - (now - at)).clamp(0.0, 0.5) / 0.5;
        let (line, tint) = match part {
            Part::Killer => (
                entry.killer.clone().unwrap_or_default(),
                if entry.by_local {
                    theme::ACCENT
                } else {
                    theme::TEXT
                },
            ),
            Part::How => (entry.how.clone(), theme::TEXT_DIM),
            Part::Victim => (
                entry.victim.clone(),
                if entry.of_local {
                    theme::DANGER
                } else {
                    theme::TEXT
                },
            ),
        };
        if text.0 != line {
            text.0 = line;
        }
        color.0 = tint.with_alpha(fade);
    }
}
