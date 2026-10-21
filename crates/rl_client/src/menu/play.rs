//! Choosing a map to host.

use bevy::prelude::*;

use super::{Ctx, Menu, Scrollable, heading, start_hosted};
use crate::content::Content;
use crate::ui::{
    Activate, ButtonStyle, Disabled, Reveal, Toggle, ValueChanged, button, theme, toggle,
};

/// What the player has picked so far.
#[derive(Resource, Debug, Default)]
pub(super) struct PlayDraft {
    map: Option<String>,
    listen: bool,
}

/// A map card, and the map it stands for.
#[derive(Component)]
struct MapCard(String);

pub(super) struct PlayPlugin;

impl Plugin for PlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayDraft>()
            .add_systems(Update, show_selection);
    }
}

/// A colour pair for a map's card, the same every time for the same name.
pub(super) fn card_colors(name: &str) -> (Color, Color) {
    let hash = name.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    let hue = (hash % 360) as f32;
    (
        Color::hsl(hue, 0.35, 0.20),
        Color::hsl((hue + 40.0) % 360.0, 0.45, 0.32),
    )
}

pub(super) fn spawn(layer: &mut ChildSpawnerCommands, ctx: &Ctx) {
    layer
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            padding: UiRect::axes(Val::Percent(6.0), Val::Px(48.0)),
            row_gap: Val::Px(18.0),
            ..default()
        })
        .with_children(|page| {
            page.spawn(Node {
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                ..default()
            })
            .with_children(|top| {
                top.spawn(Node {
                    flex_direction: FlexDirection::Column,
                    ..default()
                })
                .with_children(|titles| {
                    heading(
                        titles,
                        ctx,
                        "Play",
                        "Pick a map. You host the match; friends can join if you open it.",
                    );
                });
                top.spawn(button(ctx.fonts, "Back", ButtonStyle::Ghost))
                    .observe(|_: On<Activate>, mut menu: ResMut<Menu>| {
                        menu.pop();
                    });
            });

            page.spawn((
                Scrollable,
                Node {
                    flex_grow: 1.0,
                    flex_basis: Val::Px(0.0),
                    flex_wrap: FlexWrap::Wrap,
                    align_content: AlignContent::FlexStart,
                    overflow: Overflow::scroll_y(),
                    column_gap: Val::Px(16.0),
                    row_gap: Val::Px(16.0),
                    padding: UiRect::all(Val::Px(6.0)),
                    ..default()
                },
            ))
            .with_children(|grid| {
                for (index, map) in ctx.maps.iter().enumerate() {
                    spawn_card(grid, ctx, &map.name, &map.origin, index);
                }
                if ctx.maps.is_empty() {
                    grid.spawn(ctx.fonts.text(
                        "No maps found. Mount a game first.",
                        24.0,
                        theme::DANGER,
                    ));
                }
            });

            page.spawn(Node {
                justify_content: JustifyContent::SpaceBetween,
                align_items: AlignItems::Center,
                column_gap: Val::Px(24.0),
                ..default()
            })
            .with_children(|bottom| {
                bottom
                    .spawn((
                        Node {
                            width: Val::Px(420.0),
                            ..default()
                        },
                        children![toggle(ctx.fonts, "Open to other players", false)],
                    ))
                    .observe(on_listen_changed);
                let mut start = bottom.spawn(button(ctx.fonts, "Start game", ButtonStyle::Primary));
                if ctx.maps.is_empty() {
                    start.insert(Disabled);
                }
                start.observe(start_game);
            });
        });
}

fn spawn_card(grid: &mut ChildSpawnerCommands, ctx: &Ctx, name: &str, origin: &str, index: usize) {
    let (dark, light) = card_colors(name);
    grid.spawn((
        MapCard(name.to_owned()),
        Node {
            width: Val::Px(250.0),
            height: Val::Px(140.0),
            padding: UiRect::all(Val::Px(11.0)),
            border: UiRect::all(Val::Px(3.0)),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::End,
            border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
            ..default()
        },
        BackgroundGradient::from(LinearGradient::new(
            LinearGradient::TO_BOTTOM_RIGHT,
            vec![
                ColorStop::new(light, Val::Percent(0.0)),
                ColorStop::new(dark, Val::Percent(100.0)),
            ],
        )),
        BorderColor::all(Color::NONE),
        UiTransform::IDENTITY,
        crate::ui::Smooth::new(Color::NONE),
        crate::ui::widgets::Look {
            idle: Color::NONE,
            hover: Color::srgba(1.0, 1.0, 1.0, 0.07),
            pressed: Color::srgba(0.0, 0.0, 0.0, 0.1),
        },
        crate::ui::widgets::Interact::default(),
        crate::ui::Focusable,
        Reveal::new(Vec2::new(0.0, 24.0), 0.05 + 0.025 * (index % 12) as f32),
        children![
            ctx.fonts.bold(name, 26.0, theme::TEXT),
            ctx.fonts.text(origin, 18.0, theme::TEXT.with_alpha(0.7)),
        ],
    ))
    .observe({
        let name = name.to_owned();
        move |_: On<Activate>, mut draft: ResMut<PlayDraft>| {
            draft.map = Some(name.clone());
        }
    });
}

fn on_listen_changed(
    changed: On<ValueChanged>,
    toggles: Query<&Toggle>,
    mut draft: ResMut<PlayDraft>,
    children: Query<&Children>,
) {
    // The event comes from the toggle inside the wrapper this is observing.
    let toggle = toggles.get(changed.entity).ok().or_else(|| {
        children
            .get(changed.entity)
            .ok()
            .and_then(|kids| kids.iter().find_map(|kid| toggles.get(kid).ok()))
    });
    if let Some(toggle) = toggle {
        draft.listen = toggle.0;
    }
}

fn start_game(_: On<Activate>, mut commands: Commands) {
    commands.queue(start_game_now);
}

fn start_game_now(world: &mut World) {
    let Some(map) = pick_map(world) else {
        return;
    };
    let settings = world.resource::<crate::settings::Settings>().clone();
    let port = world.resource::<crate::player::Gameplay>().0.net.port;
    let listen = world.resource::<PlayDraft>().listen;
    start_hosted(&map, listen, &settings, port, world);
}

/// The chosen map, or the first deathmatch map when none was picked.
fn pick_map(world: &mut World) -> Option<String> {
    if let Some(map) = world.resource::<PlayDraft>().map.clone() {
        return Some(map);
    }
    let vfs = world.resource::<crate::GameVfs>().0.clone();
    let _ = world.resource::<Content>();
    let maps = rl_level::maps::deathmatch_maps(&vfs);
    maps.into_iter().next().or_else(|| {
        rl_level::maps::list_maps(&vfs)
            .into_iter()
            .next()
            .map(|entry| entry.name)
    })
}

/// Borders the chosen card in the accent colour. With nothing chosen the
/// first card stands for the map `Start game` would pick.
fn show_selection(draft: Res<PlayDraft>, mut cards: Query<(&MapCard, &mut BorderColor)>) {
    let first = cards
        .iter()
        .map(|(card, _)| card.0.as_str())
        .min()
        .map(str::to_owned);
    let chosen = draft.map.clone().or(first);
    for (card, mut border) in &mut cards {
        let wanted = if chosen.as_deref() == Some(card.0.as_str()) {
            BorderColor::all(theme::ACCENT)
        } else {
            BorderColor::all(Color::NONE)
        };
        if *border != wanted {
            *border = wanted;
        }
    }
}
