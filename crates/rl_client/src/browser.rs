//! The map browser: every playable map in the mounted content, labelled with
//! where it comes from (base game, Workshop item, `custom/`, downloads), and a
//! click to host a game on it. It says why the last game ended, if it ended
//! badly. While joining a server, a plain screen says so instead.

use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::picking::Pickable;
use bevy::picking::events::{PointerClick, PointerOut, PointerOver};
use bevy::prelude::*;
use rl_level::maps::{MapEntry, list_maps};

use crate::net::{NetSettings, NetStatus, Network};
use crate::{AppState, CurrentMap, GameVfs};

/// Shows the map browser while in [`AppState::Browser`].
pub struct BrowserPlugin;

impl Plugin for BrowserPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Browser), spawn_browser)
            .add_systems(OnEnter(AppState::Connecting), spawn_connecting)
            .add_systems(Update, scroll_list.run_if(in_state(AppState::Browser)));
    }
}

const BACKGROUND: Color = Color::srgb(0.07, 0.07, 0.09);
const ROW_IDLE: Color = Color::srgb(0.13, 0.13, 0.16);
const ROW_HOVER: Color = Color::srgb(0.22, 0.24, 0.30);
const TEXT: Color = Color::srgb(0.92, 0.92, 0.94);
const TEXT_DIM: Color = Color::srgb(0.55, 0.57, 0.62);
const TEXT_WARN: Color = Color::srgb(0.95, 0.55, 0.35);
/// Pixels scrolled per mouse-wheel line.
const LINE_HEIGHT: f32 = 40.0;

/// The scrollable list of map rows.
#[derive(Component)]
struct MapList;

/// The screen shown while joining a server.
fn spawn_connecting(mut commands: Commands, status: Res<NetStatus>) {
    let line = match &*status {
        NetStatus::Connecting(server) => format!("Connecting to {server}..."),
        _ => "Connecting...".to_owned(),
    };
    commands
        .spawn((
            DespawnOnExit(AppState::Connecting),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BACKGROUND),
        ))
        .with_children(|root| {
            root.spawn(label(&line, 28.0, TEXT));
        });
}

fn spawn_browser(mut commands: Commands, vfs: Res<GameVfs>, status: Res<NetStatus>) {
    let maps = list_maps(&vfs.0);
    info!("map browser: {} maps", maps.len());

    commands
        .spawn((
            DespawnOnExit(AppState::Browser),
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(40.0)),
                row_gap: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(BACKGROUND),
        ))
        .with_children(|root| {
            root.spawn(label("rustlife", 40.0, TEXT));
            root.spawn(label(
                "Choose a map. In a map: M returns here, Esc frees the mouse.",
                16.0,
                TEXT_DIM,
            ));
            if let NetStatus::Failed(reason) = &*status {
                root.spawn(label(
                    &format!("The last game ended: {reason}"),
                    16.0,
                    TEXT_WARN,
                ));
            }
            root.spawn((
                MapList,
                Node {
                    width: Val::Px(640.0),
                    max_height: Val::Percent(75.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    margin: UiRect::top(Val::Px(16.0)),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
            ))
            .with_children(|list| {
                if maps.is_empty() {
                    list.spawn(label(
                        "No maps found in the mounted content.",
                        18.0,
                        TEXT_DIM,
                    ));
                }
                for map in maps {
                    spawn_row(list, map);
                }
            });
        });
}

/// One clickable row: map name on the left, origin on the right.
fn spawn_row(list: &mut ChildSpawnerCommands, map: MapEntry) {
    let name = map.name.clone();
    list.spawn((
        Node {
            width: Val::Percent(100.0),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            padding: UiRect::axes(Val::Px(16.0), Val::Px(10.0)),
            ..default()
        },
        BackgroundColor(ROW_IDLE),
    ))
    .with_children(|row| {
        // Labels ignore the pointer so hover and clicks land on the row itself.
        row.spawn((label(&map.name, 22.0, TEXT), Pickable::IGNORE));
        row.spawn((label(&map.origin, 16.0, TEXT_DIM), Pickable::IGNORE));
    })
    .observe(
        move |_: On<PointerClick>,
              mut current: ResMut<CurrentMap>,
              mut settings: ResMut<NetSettings>,
              mut next: ResMut<NextState<AppState>>| {
            current.0.clone_from(&name);
            // A map chosen here is hosted, even after joining a server.
            if matches!(settings.network, Network::Join(_)) {
                settings.network = Network::Host { port: None };
            }
            next.set(AppState::InGame);
        },
    )
    .observe(
        |over: On<PointerOver>, mut colors: Query<&mut BackgroundColor>| {
            if let Ok(mut color) = colors.get_mut(over.entity) {
                color.0 = ROW_HOVER;
            }
        },
    )
    .observe(
        |out: On<PointerOut>, mut colors: Query<&mut BackgroundColor>| {
            if let Ok(mut color) = colors.get_mut(out.entity) {
                color.0 = ROW_IDLE;
            }
        },
    );
}

fn label(text: &str, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(text),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(color),
    )
}

/// Scrolls the map list with the mouse wheel.
fn scroll_list(
    scroll: Res<AccumulatedMouseScroll>,
    mut lists: Query<&mut ScrollPosition, With<MapList>>,
) {
    if scroll.delta.y == 0.0 {
        return;
    }
    let pixels = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y * LINE_HEIGHT,
        MouseScrollUnit::Pixel => scroll.delta.y,
    };
    for mut position in &mut lists {
        position.0.y = (position.0.y - pixels).max(0.0);
    }
}
