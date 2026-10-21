//! The pause menu. The game does not stop (it is multiplayer): the player
//! just stops sending input while it is up.

use bevy::prelude::*;

use super::{Ctx, Menu, Screen, SettingsTab, heading, quit};
use crate::AppState;
use crate::net::NetStatus;
use crate::ui::{Activate, ButtonStyle, Reveal, button, panel};

pub(super) fn spawn(layer: &mut ChildSpawnerCommands, ctx: &Ctx) {
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
                panel(Val::Px(460.0), Val::Auto),
                Reveal::new(Vec2::new(0.0, 24.0), 0.0),
                UiTransform::IDENTITY,
            ))
            .with_children(|panel| {
                heading(panel, ctx, "Paused", "The match goes on without you.");
                panel.spawn(Node {
                    height: Val::Px(8.0),
                    ..default()
                });
                panel
                    .spawn(button(ctx.fonts, "Resume", ButtonStyle::Primary))
                    .observe(|_: On<Activate>, mut menu: ResMut<Menu>| menu.close());
                panel
                    .spawn(button(ctx.fonts, "Settings", ButtonStyle::Secondary))
                    .observe(|_: On<Activate>, mut menu: ResMut<Menu>| {
                        menu.push(Screen::Settings(SettingsTab::Video));
                    });
                panel
                    .spawn(button(ctx.fonts, "Leave match", ButtonStyle::Danger))
                    .observe(
                        |_: On<Activate>,
                         mut menu: ResMut<Menu>,
                         mut next: ResMut<NextState<AppState>>,
                         mut status: ResMut<NetStatus>| {
                            menu.close();
                            *status = NetStatus::Idle;
                            next.set(AppState::MainMenu);
                        },
                    );
                panel
                    .spawn(button(ctx.fonts, "Quit to desktop", ButtonStyle::Ghost))
                    .observe(|_: On<Activate>, exit: MessageWriter<AppExit>| quit(exit));
            });
        });
}
