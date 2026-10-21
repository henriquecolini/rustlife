//! The main menu.

use bevy::prelude::*;

use super::{Ctx, Menu, Screen, SettingsTab, quit};
use crate::net::NetStatus;
use crate::ui::{Activate, ButtonStyle, Disabled, Reveal, button, theme};

/// How far each button's entrance is delayed after the one above.
const STAGGER: f32 = 0.06;

pub(super) fn spawn(layer: &mut ChildSpawnerCommands, ctx: &Ctx) {
    layer
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            padding: UiRect::axes(Val::Percent(7.0), Val::Px(64.0)),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        })
        .with_children(|page| {
            page.spawn(Node {
                flex_direction: FlexDirection::Column,
                width: Val::Px(440.0),
                row_gap: Val::Px(12.0),
                ..default()
            })
            .with_children(|column| {
                column.spawn((
                    Node {
                        margin: UiRect::bottom(Val::Px(28.0)),
                        flex_direction: FlexDirection::Column,
                        ..default()
                    },
                    Reveal::new(Vec2::new(-40.0, 0.0), 0.0),
                    UiTransform::IDENTITY,
                    children![
                        ctx.fonts.bold("RUSTLIFE", 96.0, theme::TEXT),
                        ctx.fonts.text(
                            "An arena shooter for your Source maps",
                            22.0,
                            theme::TEXT_DIM
                        ),
                    ],
                ));
                let playable = ctx.content.any_mounted();
                let entries: [(&str, ButtonStyle, Screen); 4] = [
                    ("Play", ButtonStyle::Primary, Screen::Play),
                    ("Join a server", ButtonStyle::Secondary, Screen::Join),
                    ("Mounted games", ButtonStyle::Secondary, Screen::Games),
                    (
                        "Settings",
                        ButtonStyle::Secondary,
                        Screen::Settings(SettingsTab::Video),
                    ),
                ];
                for (index, (label, style, screen)) in entries.into_iter().enumerate() {
                    let needs_game = matches!(screen, Screen::Play | Screen::Join);
                    let mut entry = column.spawn((
                        button(ctx.fonts, label, style),
                        Reveal::new(Vec2::new(-60.0, 0.0), 0.12 + STAGGER * index as f32),
                    ));
                    if needs_game && !playable {
                        entry.insert(Disabled);
                    }
                    entry.observe(move |_: On<Activate>, mut menu: ResMut<Menu>| {
                        menu.push(screen.clone());
                    });
                }
                column
                    .spawn((
                        button(ctx.fonts, "Quit", ButtonStyle::Danger),
                        Reveal::new(Vec2::new(-60.0, 0.0), 0.12 + STAGGER * 4.0),
                    ))
                    .observe(|_: On<Activate>, exit: MessageWriter<AppExit>| quit(exit));
            });

            page.spawn(Node {
                width: Val::Percent(100.0),
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::End,
                ..default()
            })
            .with_children(|footer| {
                footer.spawn(ctx.fonts.text(
                    &format!("rustlife {}", ctx.version),
                    18.0,
                    theme::TEXT_DIM,
                ));
                games_card(footer, ctx);
            });
        });
    if let NetStatus::Failed(reason) = ctx.status {
        banner(layer, ctx, reason);
    }
}

/// The mounted-games summary in the corner.
fn games_card(parent: &mut ChildSpawnerCommands, ctx: &Ctx) {
    parent
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(16.0)),
                row_gap: Val::Px(6.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
                ..default()
            },
            BackgroundColor(theme::PANEL),
            BorderColor::all(theme::BORDER),
            Reveal::new(Vec2::new(0.0, 30.0), 0.4),
            UiTransform::IDENTITY,
        ))
        .with_children(|card| {
            card.spawn(ctx.fonts.bold("Mounted games", 18.0, theme::TEXT_DIM));
            for status in &ctx.content.games {
                let (color, text) = match (&status.state, status.enabled) {
                    (Ok(_), true) => (theme::SUCCESS, status.game.name.to_owned()),
                    (_, false) => (theme::TEXT_DIM, format!("{} (disabled)", status.game.name)),
                    (Err(_), true) => (theme::DANGER, format!("{} (not found)", status.game.name)),
                };
                card.spawn(Node {
                    column_gap: Val::Px(10.0),
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|row| {
                    row.spawn((
                        Node {
                            width: Val::Px(10.0),
                            height: Val::Px(10.0),
                            border_radius: BorderRadius::MAX,
                            ..default()
                        },
                        BackgroundColor(color),
                    ));
                    row.spawn(ctx.fonts.text(&text, 22.0, theme::TEXT));
                });
            }
            if !ctx.content.any_mounted() {
                card.spawn(ctx.fonts.text(
                    "Open \"Mounted games\" to point rustlife at your install.",
                    18.0,
                    theme::DANGER,
                ));
            }
        });
}

/// Why the last game ended badly, with a button to dismiss it.
fn banner(layer: &mut ChildSpawnerCommands, ctx: &Ctx, reason: &str) {
    layer
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(28.0),
                right: Val::Px(28.0),
                max_width: Val::Px(560.0),
                padding: UiRect::all(Val::Px(18.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
                ..default()
            },
            BackgroundColor(theme::PANEL),
            BorderColor::all(theme::DANGER),
            Reveal::new(Vec2::new(0.0, -30.0), 0.2),
            UiTransform::IDENTITY,
        ))
        .with_children(|banner| {
            banner.spawn(ctx.fonts.bold("The last game ended", 22.0, theme::DANGER));
            banner.spawn(ctx.fonts.text(reason, 20.0, theme::TEXT));
            banner
                .spawn(button(ctx.fonts, "Dismiss", ButtonStyle::Ghost))
                .observe(|_: On<Activate>, mut status: ResMut<NetStatus>| {
                    *status = NetStatus::Idle;
                });
        });
}
