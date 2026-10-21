//! The screens shown while connecting to a server or moving to the next
//! map.

use bevy::prelude::*;

use crate::net::NetStatus;
use crate::ui::{Fonts, theme};
use crate::{AppState, CurrentMap};

pub(super) struct LoadingPlugin;

impl Plugin for LoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::Connecting), connecting)
            .add_systems(OnEnter(AppState::ChangingMap), changing_map)
            .add_systems(Update, spin);
    }
}

/// A mark that turns while something loads.
#[derive(Component)]
struct Spinner;

fn connecting(commands: Commands, fonts: Res<Fonts>, status: Res<NetStatus>) {
    let line = match &*status {
        NetStatus::Connecting(server) => format!("Connecting to {server}"),
        _ => "Connecting".to_owned(),
    };
    screen(commands, &fonts, AppState::Connecting, &line);
}

fn changing_map(commands: Commands, fonts: Res<Fonts>, map: Res<CurrentMap>) {
    screen(
        commands,
        &fonts,
        AppState::ChangingMap,
        &format!("Loading {}", map.0),
    );
}

/// A screen with a spinner and one line of text, until `state` is left.
fn screen(mut commands: Commands, fonts: &Fonts, state: AppState, line: &str) {
    commands.spawn((
        DespawnOnExit(state),
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: Val::Px(28.0),
            ..default()
        },
        BackgroundColor(theme::BACKGROUND),
        GlobalZIndex(100),
        children![
            (
                Spinner,
                Node {
                    width: Val::Px(46.0),
                    height: Val::Px(46.0),
                    border: UiRect::all(Val::Px(5.0)),
                    border_radius: BorderRadius::all(Val::Px(8.0)),
                    ..default()
                },
                BorderColor {
                    top: theme::ACCENT,
                    right: theme::BORDER,
                    bottom: theme::BORDER,
                    left: theme::BORDER,
                },
                UiTransform::IDENTITY,
            ),
            fonts.bold(line, 34.0, theme::TEXT),
        ],
    ));
}

fn spin(time: Res<Time<Real>>, mut spinners: Query<&mut UiTransform, With<Spinner>>) {
    for mut transform in &mut spinners {
        transform.rotation = Rot2::radians(time.elapsed_secs() * 4.0);
    }
}
