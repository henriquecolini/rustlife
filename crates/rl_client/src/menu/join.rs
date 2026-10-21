//! Joining a server by address.

use std::net::{SocketAddr, ToSocketAddrs};

use bevy::prelude::*;

use super::{Ctx, Menu, heading};
use crate::AppState;
use crate::net::{NetSettings, NetStatus, Network};
use crate::settings::Settings;
use crate::ui::{
    Activate, ButtonStyle, Reveal, TextField, ValueChanged, button, panel, text_field, theme,
};

/// The address typed so far.
#[derive(Resource, Debug, Default)]
pub(super) struct JoinDraft {
    address: String,
    error: Option<String>,
}

pub(super) struct JoinPlugin;

impl Plugin for JoinPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<JoinDraft>();
    }
}

/// The address `text` names: `host:port`, with the port defaulting to
/// `default_port`. Host names are looked up.
///
/// # Errors
///
/// Says in words what is wrong with `text`.
pub(super) fn resolve(text: &str, default_port: u16) -> Result<SocketAddr, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Type the server's address first.".to_owned());
    }
    let with_port = if text.contains(':') && !text.ends_with(':') {
        text.to_owned()
    } else {
        format!("{}:{default_port}", text.trim_end_matches(':'))
    };
    with_port
        .to_socket_addrs()
        .map_err(|error| format!("Cannot find \"{text}\": {error}."))?
        .next()
        .ok_or_else(|| format!("\"{text}\" has no address."))
}

pub(super) fn spawn(layer: &mut ChildSpawnerCommands, ctx: &Ctx) {
    let last = ctx
        .settings
        .player
        .recent_servers
        .first()
        .cloned()
        .unwrap_or_default();
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
                panel(Val::Px(640.0), Val::Auto),
                Reveal::new(Vec2::new(0.0, 24.0), 0.0),
                UiTransform::IDENTITY,
            ))
            .with_children(|panel| {
                heading(
                    panel,
                    ctx,
                    "Join a server",
                    "Both of you need the same games mounted.",
                );
                panel.spawn(Node {
                    height: Val::Px(6.0),
                    ..default()
                });
                panel
                    .spawn(text_field(ctx.fonts, "Address", &last, "host:port", 64))
                    .observe(
                        |changed: On<ValueChanged>,
                         fields: Query<&TextField>,
                         mut draft: ResMut<JoinDraft>| {
                            if let Ok(field) = fields.get(changed.entity) {
                                draft.address.clone_from(&field.value);
                                draft.error = None;
                            }
                        },
                    );
                if !ctx.settings.player.recent_servers.is_empty() {
                    panel.spawn(ctx.fonts.bold("Recent", 18.0, theme::TEXT_DIM));
                    for server in &ctx.settings.player.recent_servers {
                        let address = server.clone();
                        panel
                            .spawn(button(ctx.fonts, server, ButtonStyle::Secondary))
                            .observe(move |_: On<Activate>, mut commands: Commands| {
                                let address = address.clone();
                                commands.queue(move |world: &mut World| connect(&address, world));
                            });
                    }
                }
                panel
                    .spawn(Node {
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SpaceBetween,
                        margin: UiRect::top(Val::Px(14.0)),
                        ..default()
                    })
                    .with_children(|buttons| {
                        buttons
                            .spawn(button(ctx.fonts, "Back", ButtonStyle::Ghost))
                            .observe(|_: On<Activate>, mut menu: ResMut<Menu>| {
                                menu.pop();
                            });
                        buttons
                            .spawn(button(ctx.fonts, "Connect", ButtonStyle::Primary))
                            .observe(|_: On<Activate>, mut commands: Commands| {
                                commands.queue(connect_typed);
                            });
                    });
            });
        });
}

/// Joins the server typed into the field, or the latest recent one when
/// nothing was typed (the field shows that one).
fn connect_typed(world: &mut World) {
    let typed = world.resource::<JoinDraft>().address.clone();
    let address = if typed.trim().is_empty() {
        world
            .resource::<Settings>()
            .player
            .recent_servers
            .first()
            .cloned()
            .unwrap_or_default()
    } else {
        typed
    };
    connect(&address, world);
}

/// Joins the server at `text`, or says why not.
fn connect(text: &str, world: &mut World) {
    let port = world.resource::<crate::player::Gameplay>().0.net.port;
    match resolve(text, port) {
        Ok(address) => {
            let name = world.resource::<Settings>().player.name.clone();
            world
                .resource_mut::<Settings>()
                .player
                .remember_server(&address.to_string());
            let mut net = world.resource_mut::<NetSettings>();
            net.network = Network::Join(address);
            net.name = name;
            *world.resource_mut::<NetStatus>() = NetStatus::Connecting(address.to_string());
            world.resource_mut::<Menu>().close();
            world
                .resource_mut::<NextState<AppState>>()
                .set(AppState::Connecting);
        }
        Err(error) => {
            *world.resource_mut::<NetStatus>() = NetStatus::Failed(error);
            world.resource_mut::<Menu>().pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_default_their_port_and_reject_blanks() {
        assert_eq!(
            resolve("127.0.0.1:4000", 27015).unwrap(),
            "127.0.0.1:4000".parse().unwrap()
        );
        assert_eq!(
            resolve("  127.0.0.1  ", 27015).unwrap(),
            "127.0.0.1:27015".parse().unwrap()
        );
        assert_eq!(
            resolve("127.0.0.1:", 27015).unwrap(),
            "127.0.0.1:27015".parse().unwrap()
        );
        assert!(resolve("", 27015).is_err());
        assert!(resolve("   ", 27015).is_err());
        assert!(resolve("127.0.0.1:notaport", 27015).is_err());
    }
}
