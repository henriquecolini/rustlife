//! The scoreboard, shown while the scoreboard key is held and between
//! matches, with the vote for the next map below it.

use bevy::prelude::*;

use super::anchored;
use super::text::{self, Names};
use crate::CurrentMap;
use crate::menu::Menu;
use crate::net::{Connection, Game, Roster};
use crate::settings::Bindings;
use crate::ui::widgets::{Interact, Look};
use crate::ui::{Activate, ButtonStyle, Fonts, Smooth, theme};

/// Players the board has room for.
const ROWS: usize = 20;
/// Maps a vote can offer, at most (the server offers four).
const CARDS: usize = 6;

#[derive(Component)]
pub(super) struct Board;

#[derive(Component)]
pub(super) struct Heading(Part);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Part {
    Result,
    Map,
    VoteTitle,
}

#[derive(Component)]
pub(super) struct Row(usize);

#[derive(Component)]
pub(super) struct Cell(usize, Column);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Column {
    Rank,
    Name,
    Points,
    Kills,
    Deaths,
}

#[derive(Component)]
pub(super) struct VoteSection;

#[derive(Component)]
pub(super) struct VoteCard(usize);

#[derive(Component)]
pub(super) struct CardText(usize, bool);

pub(super) fn spawn(commands: &mut Commands, fonts: &Fonts) {
    commands
        .spawn(anchored(
            JustifyContent::Center,
            AlignItems::Center,
            UiRect::ZERO,
        ))
        .with_children(|root| {
            root.spawn((
                Board,
                Node {
                    width: Val::Px(780.0),
                    max_height: Val::Percent(92.0),
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::all(Val::Px(26.0)),
                    row_gap: Val::Px(6.0),
                    border: UiRect::all(Val::Px(1.0)),
                    border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
                    display: Display::None,
                    ..default()
                },
                BackgroundColor(theme::PANEL),
                BorderColor::all(theme::BORDER),
                BoxShadow::new(
                    Color::srgba(0.0, 0.0, 0.0, 0.5),
                    Val::Px(0.0),
                    Val::Px(18.0),
                    Val::Px(0.0),
                    Val::Px(40.0),
                ),
            ))
            .with_children(|board| {
                board.spawn((
                    Heading(Part::Result),
                    fonts.bold("Scoreboard", 40.0, theme::TEXT),
                ));
                board.spawn((Heading(Part::Map), fonts.text("", 20.0, theme::TEXT_DIM)));
                header(board, fonts);
                for index in 0..ROWS {
                    score_row(board, fonts, index);
                }
                vote_section(board, fonts);
            });
        });
}

const WIDTHS: [(Column, f32); 5] = [
    (Column::Rank, 52.0),
    (Column::Name, 0.0),
    (Column::Points, 96.0),
    (Column::Kills, 82.0),
    (Column::Deaths, 82.0),
];

fn column_node(column: Column, width: f32) -> Node {
    let mut node = Node {
        justify_content: if matches!(column, Column::Name | Column::Rank) {
            JustifyContent::FlexStart
        } else {
            JustifyContent::FlexEnd
        },
        ..default()
    };
    if width > 0.0 {
        node.width = Val::Px(width);
    } else {
        node.flex_grow = 1.0;
    }
    node
}

fn header(board: &mut ChildSpawnerCommands, fonts: &Fonts) {
    board
        .spawn(Node {
            width: Val::Percent(100.0),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(4.0)),
            margin: UiRect::top(Val::Px(10.0)),
            ..default()
        })
        .with_children(|row| {
            for (column, width) in WIDTHS {
                let label = match column {
                    Column::Rank => "#",
                    Column::Name => "PLAYER",
                    Column::Points => "POINTS",
                    Column::Kills => "KILLS",
                    Column::Deaths => "DEATHS",
                };
                row.spawn(column_node(column, width)).with_children(|cell| {
                    cell.spawn(fonts.bold(label, 16.0, theme::TEXT_DIM));
                });
            }
        });
}

fn score_row(board: &mut ChildSpawnerCommands, fonts: &Fonts, index: usize) {
    board
        .spawn((
            Row(index),
            Node {
                width: Val::Percent(100.0),
                padding: UiRect::axes(Val::Px(12.0), Val::Px(5.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                display: Display::None,
                ..default()
            },
            BackgroundColor(Color::NONE),
        ))
        .with_children(|row| {
            for (column, width) in WIDTHS {
                row.spawn(column_node(column, width)).with_children(|cell| {
                    cell.spawn((Cell(index, column), fonts.bold("", 24.0, theme::TEXT)));
                });
            }
        });
}

fn vote_section(board: &mut ChildSpawnerCommands, fonts: &Fonts) {
    board
        .spawn((
            VoteSection,
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                margin: UiRect::top(Val::Px(18.0)),
                display: Display::None,
                ..default()
            },
        ))
        .with_children(|section| {
            section.spawn((
                Heading(Part::VoteTitle),
                fonts.bold("Vote for the next map", 24.0, theme::TEXT),
            ));
            section
                .spawn(Node {
                    column_gap: Val::Px(12.0),
                    flex_wrap: FlexWrap::Wrap,
                    row_gap: Val::Px(12.0),
                    ..default()
                })
                .with_children(|cards| {
                    for index in 0..CARDS {
                        let look = Look::of(ButtonStyle::Secondary);
                        cards
                            .spawn((
                                VoteCard(index),
                                Node {
                                    width: Val::Px(172.0),
                                    flex_direction: FlexDirection::Column,
                                    padding: UiRect::all(Val::Px(12.0)),
                                    border: UiRect::all(Val::Px(2.0)),
                                    border_radius: BorderRadius::all(Val::Px(8.0)),
                                    display: Display::None,
                                    ..default()
                                },
                                BackgroundColor(look.idle),
                                BorderColor::all(Color::NONE),
                                UiTransform::IDENTITY,
                                Smooth::new(look.idle),
                                look,
                                Interact::default(),
                            ))
                            .with_children(|card| {
                                card.spawn((
                                    CardText(index, false),
                                    fonts.bold("", 21.0, theme::TEXT),
                                ));
                                card.spawn((
                                    CardText(index, true),
                                    fonts.text("", 18.0, theme::TEXT_DIM),
                                ));
                            })
                            .observe(
                                move |_: On<Activate>, connection: Option<ResMut<Connection>>| {
                                    if let Some(mut connection) = connection {
                                        connection.0.cast_vote(index as u16);
                                    }
                                },
                            );
                    }
                });
        });
}

/// Whether the board is up: between matches, or while the key is held.
fn shown(
    keys: &ButtonInput<KeyCode>,
    bindings: &Bindings,
    menu: &Menu,
    game: &Game,
) -> Option<bool> {
    let over = game.sim()?.game().intermission();
    let held = bindings.pressed("scoreboard", keys, None);
    Some(over || (held && !menu.is_open()))
}

/// Shows or hides the board.
pub(super) fn show(
    keys: Res<ButtonInput<KeyCode>>,
    bindings: Res<Bindings>,
    menu: Res<Menu>,
    game: Game,
    mut board: Query<&mut Node, With<Board>>,
) {
    let Some(shown) = shown(&keys, &bindings, &menu, &game) else {
        return;
    };
    for mut node in &mut board {
        let display = if shown { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
    }
}

/// The board's headings.
pub(super) fn headings(
    game: Game,
    roster: Res<Roster>,
    map: Res<CurrentMap>,
    mut headings: Query<(&Heading, &mut Text)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let names = Names {
        local,
        roster: &roster.0,
    };
    let over = sim.game().intermission();
    let message = text::message(sim, &names, None);
    for (heading, mut text) in &mut headings {
        let line = match (heading.0, over) {
            (Part::Result, true) => message.title.clone(),
            (Part::Result, false) => "Scoreboard".to_owned(),
            (Part::Map, true) => format!("{}   ·   {}", map.0, message.subtitle),
            (Part::Map, false) => map.0.clone(),
            (Part::VoteTitle, _) => "Vote for the next map (click, or press its number)".to_owned(),
        };
        if text.0 != line {
            text.0 = line;
        }
    }
}

/// The score rows.
pub(super) fn rows(
    game: Game,
    roster: Res<Roster>,
    mut rows: Query<(&Row, &mut Node, &mut BackgroundColor)>,
    mut cells: Query<(&Cell, &mut Text, &mut TextColor)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), game.local()) else {
        return;
    };
    let names = Names {
        local,
        roster: &roster.0,
    };
    let table = text::score_rows(sim, &names);
    for (Row(index), mut node, mut background) in &mut rows {
        match table.get(*index) {
            Some(row) => {
                node.display = Display::Flex;
                background.0 = if row.local {
                    theme::ACCENT.with_alpha(0.07)
                } else if index % 2 == 0 {
                    Color::srgba(1.0, 1.0, 1.0, 0.012)
                } else {
                    Color::NONE
                };
            }
            None => node.display = Display::None,
        }
    }
    for (Cell(index, column), mut text, mut color) in &mut cells {
        let Some(row) = table.get(*index) else {
            continue;
        };
        let line = match column {
            Column::Rank => row.rank.to_string(),
            Column::Name => row.name.clone(),
            Column::Points => row.score.points.to_string(),
            Column::Kills => row.score.kills.to_string(),
            Column::Deaths => row.score.deaths.to_string(),
        };
        if text.0 != line {
            text.0 = line;
        }
        color.0 = match (row.local, column) {
            (true, _) => theme::ACCENT,
            (_, Column::Points | Column::Name) => theme::TEXT,
            (_, Column::Rank | Column::Kills | Column::Deaths) => theme::TEXT_DIM,
        };
    }
}

/// The vote cards.
pub(super) fn vote(
    game: Game,
    mut section: Query<&mut Node, With<VoteSection>>,
    mut cards: Query<(&VoteCard, &mut Node, &mut BorderColor), Without<VoteSection>>,
    mut card_texts: Query<(&CardText, &mut Text)>,
) {
    let over = game.sim().is_some_and(|sim| sim.game().intermission());
    let vote = game.vote().filter(|_| over);
    for mut node in &mut section {
        let display = if vote.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
    for (VoteCard(index), mut node, mut border) in &mut cards {
        let Some(vote) = vote.filter(|vote| *index < vote.options.len()) else {
            node.display = Display::None;
            continue;
        };
        node.display = Display::Flex;
        *border = BorderColor::all(if vote.chosen == Some(*index as u16) {
            theme::ACCENT
        } else {
            Color::NONE
        });
    }
    for (CardText(index, is_votes), mut text) in &mut card_texts {
        let Some(vote) = vote.filter(|vote| *index < vote.options.len()) else {
            continue;
        };
        let line = if *is_votes {
            let votes = vote.tally.get(*index).copied().unwrap_or(0);
            format!(
                "[{}]   {votes} vote{}",
                index + 1,
                if votes == 1 { "" } else { "s" }
            )
        } else {
            vote.options[*index].clone()
        };
        if text.0 != line {
            text.0 = line;
        }
    }
}
