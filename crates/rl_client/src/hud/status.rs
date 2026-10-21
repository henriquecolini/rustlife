//! Points and clock (top), the message in the middle of the screen, the
//! pickup toast and the frame rate.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

use super::text::{self, Names};
use super::{HudLog, PICKUP_MESSAGE_SECS, anchored};
use crate::net::{Game, Roster};
use crate::settings::Settings;
use crate::ui::{Fonts, theme};

/// A text on the status elements.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Readout {
    Points,
    Clock,
    /// The line under the clock saying what the match is waiting for.
    Phase,
    MessageTitle,
    MessageSubtitle,
    Toast,
    Fps,
}

/// An element that is hidden when it has nothing to say.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Panel {
    Phase,
    Message,
    /// The clock and its divider.
    Clock,
}

pub(super) fn spawn(commands: &mut Commands, fonts: &Fonts) {
    spawn_scores(commands, fonts);
    spawn_message(commands, fonts);
    spawn_toast_and_fps(commands, fonts);
}

/// Points, clock and the match phase, top centre.
fn spawn_scores(commands: &mut Commands, fonts: &Fonts) {
    commands
        .spawn(anchored(
            JustifyContent::Center,
            AlignItems::FlexStart,
            UiRect::top(Val::Px(20.0)),
        ))
        .with_children(|root| {
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(8.0),
                ..default()
            })
            .with_children(|column| {
                column
                    .spawn((
                        Node {
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(20.0),
                            padding: UiRect::axes(Val::Px(26.0), Val::Px(6.0)),
                            border_radius: BorderRadius::MAX,
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.62)),
                    ))
                    .with_children(|capsule| {
                        capsule.spawn(fonts.bold("POINTS", 16.0, theme::TEXT_DIM));
                        capsule.spawn((Readout::Points, fonts.bold("0", 34.0, theme::ACCENT)));
                        capsule
                            .spawn((
                                Panel::Clock,
                                Node {
                                    align_items: AlignItems::Center,
                                    column_gap: Val::Px(20.0),
                                    ..default()
                                },
                            ))
                            .with_children(|clock| {
                                clock.spawn((
                                    Node {
                                        width: Val::Px(2.0),
                                        height: Val::Px(26.0),
                                        ..default()
                                    },
                                    BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.1)),
                                ));
                                clock.spawn((Readout::Clock, fonts.bold("", 34.0, theme::TEXT)));
                            });
                    });
                column
                    .spawn((
                        Panel::Phase,
                        Node {
                            padding: UiRect::axes(Val::Px(18.0), Val::Px(4.0)),
                            border_radius: BorderRadius::MAX,
                            display: Display::None,
                            ..default()
                        },
                        BackgroundColor(theme::ACCENT.with_alpha(0.9)),
                    ))
                    .with_children(|pill| {
                        pill.spawn((Readout::Phase, fonts.bold("", 20.0, theme::ON_ACCENT)));
                    });
            });
        });
}

/// The message in the middle of the screen.
fn spawn_message(commands: &mut Commands, fonts: &Fonts) {
    commands
        .spawn(anchored(
            JustifyContent::Center,
            AlignItems::Center,
            UiRect::top(Val::Px(340.0)),
        ))
        .with_children(|root| {
            root.spawn((
                Panel::Message,
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    padding: UiRect::axes(Val::Px(34.0), Val::Px(10.0)),
                    border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
                    display: Display::None,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.03, 0.04, 0.06, 0.55)),
            ))
            .with_children(|panel| {
                panel.spawn((Readout::MessageTitle, fonts.bold("", 44.0, theme::TEXT)));
                panel.spawn((
                    Readout::MessageSubtitle,
                    fonts.text("", 24.0, theme::TEXT_DIM),
                ));
            });
        });
}

/// The pickup toast and the frame rate.
fn spawn_toast_and_fps(commands: &mut Commands, fonts: &Fonts) {
    commands
        .spawn(anchored(
            JustifyContent::Center,
            AlignItems::FlexEnd,
            UiRect::bottom(Val::Px(112.0)),
        ))
        .with_children(|root| {
            root.spawn((Readout::Toast, fonts.bold("", 24.0, theme::ACCENT)));
        });
    // The frame rate.
    commands
        .spawn(anchored(
            JustifyContent::FlexStart,
            AlignItems::FlexStart,
            UiRect::new(Val::Px(14.0), Val::Px(0.0), Val::Px(10.0), Val::Px(0.0)),
        ))
        .with_children(|root| {
            root.spawn((Readout::Fps, fonts.text("", 18.0, theme::SUCCESS)));
        });
}

pub(super) fn update(
    time: Res<Time>,
    game: Game,
    roster: Res<Roster>,
    log: Res<HudLog>,
    settings: Res<Settings>,
    diagnostics: Option<Res<DiagnosticsStore>>,
    mut texts: Query<(&Readout, &mut Text, &mut TextColor)>,
    mut panels: Query<(&Panel, &mut Node)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let names = Names {
        local,
        roster: &roster.0,
    };
    let now = time.elapsed_secs();
    let status = text::status(sim, local);
    let message = text::message(sim, &names, None);
    let pickup = log
        .pickup
        .as_ref()
        .filter(|(_, at)| now - at < PICKUP_MESSAGE_SECS);
    let frame_rate = diagnostics
        .as_ref()
        .and_then(|store| store.get(&FrameTimeDiagnosticsPlugin::FPS))
        .and_then(bevy::diagnostic::Diagnostic::smoothed);
    for (readout, mut text, mut color) in &mut texts {
        let line = match readout {
            Readout::Points => match status.limit {
                Some(limit) => format!("{} / {limit}", status.points),
                None => status.points.to_string(),
            },
            Readout::Clock => status.clock.clone(),
            Readout::Phase => status.phase.clone(),
            Readout::MessageTitle => message.title.clone(),
            Readout::MessageSubtitle => message.subtitle.clone(),
            Readout::Toast => {
                let age = pickup.map_or(f32::MAX, |(_, at)| now - at);
                color.0 =
                    theme::ACCENT.with_alpha((1.0 - age / PICKUP_MESSAGE_SECS).clamp(0.0, 1.0));
                pickup.map(|(name, _)| name.clone()).unwrap_or_default()
            }
            Readout::Fps => match (settings.video.show_fps, frame_rate) {
                (true, Some(rate)) => format!("{rate:.0} FPS"),
                _ => String::new(),
            },
        };
        if text.0 != line {
            text.0 = line;
        }
    }
    for (panel, mut node) in &mut panels {
        let empty = match panel {
            Panel::Phase => status.phase.is_empty(),
            Panel::Message => message.title.is_empty(),
            Panel::Clock => status.clock.is_empty(),
        };
        let display = if empty { Display::None } else { Display::Flex };
        if node.display != display {
            node.display = display;
        }
    }
}
