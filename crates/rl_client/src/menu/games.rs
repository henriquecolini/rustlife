//! The mounted games: which Source games provide maps and weapons, and
//! where they are.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bevy::prelude::*;

use super::{Ctx, Menu, Scrollable, heading};
use crate::GameVfs;
use crate::content::{Content, GameStatus};
use crate::ui::{
    Activate, ButtonStyle, Reveal, TextField, Toggle, ValueChanged, button, panel, text_field,
    theme, toggle,
};

/// Text typed on this screen, kept until it is applied.
#[derive(Resource, Debug, Default)]
pub(super) struct GamesDraft {
    dirs: BTreeMap<String, String>,
    library: String,
}

pub(super) struct GamesPlugin;

impl Plugin for GamesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GamesDraft>();
    }
}

/// Marks a game's install-folder field with the game it belongs to.
#[derive(Component)]
struct DirField(&'static str);

/// Marks a game's enable toggle.
#[derive(Component)]
struct EnableToggle(&'static str);

/// Marks the field for an extra Steam library folder.
#[derive(Component)]
struct LibraryField;

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
                panel(Val::Px(820.0), Val::Percent(88.0)),
                Reveal::new(Vec2::new(0.0, 24.0), 0.0),
                UiTransform::IDENTITY,
            ))
            .with_children(|panel| {
                heading(
                    panel,
                    ctx,
                    "Mounted games",
                    "Their maps, models and sounds become available, as in Garry's Mod.",
                );
                panel
                    .spawn((
                        Scrollable,
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(14.0),
                            overflow: Overflow::scroll_y(),
                            flex_shrink: 1.0,
                            ..default()
                        },
                    ))
                    .with_children(|list| {
                        for status in &ctx.content.games {
                            game_card(list, ctx, status);
                        }
                        list.spawn(ctx.fonts.bold("Extra Steam library", 18.0, theme::TEXT_DIM));
                        list.spawn((
                            text_field(
                                ctx.fonts,
                                "Folder",
                                "",
                                "a folder that contains steamapps/",
                                256,
                            ),
                            LibraryField,
                        ))
                        .observe(on_text_changed);
                    });
                panel
                    .spawn(Node {
                        justify_content: JustifyContent::SpaceBetween,
                        margin: UiRect::top(Val::Px(8.0)),
                        ..default()
                    })
                    .with_children(|buttons| {
                        buttons
                            .spawn(button(ctx.fonts, "Back", ButtonStyle::Ghost))
                            .observe(|_: On<Activate>, mut menu: ResMut<Menu>| {
                                menu.pop();
                            });
                        buttons
                            .spawn(button(ctx.fonts, "Apply and rescan", ButtonStyle::Primary))
                            .observe(|_: On<Activate>, mut commands: Commands| {
                                commands.queue(apply);
                            });
                    });
            });
        });
}

fn game_card(list: &mut ChildSpawnerCommands, ctx: &Ctx, status: &GameStatus) {
    let configured = ctx.content.config.content.game(&status.game);
    let (line, color) = match (&status.state, status.enabled) {
        (Ok(dir), true) => (format!("Mounted from {}", dir.display()), theme::SUCCESS),
        (_, false) => ("Disabled".to_owned(), theme::TEXT_DIM),
        (Err(why), true) => (why.clone(), theme::DANGER),
    };
    list.spawn((
        Node {
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(Val::Px(16.0)),
            row_gap: Val::Px(8.0),
            border: UiRect::all(Val::Px(1.0)),
            border_radius: BorderRadius::all(Val::Px(theme::RADIUS)),
            ..default()
        },
        BackgroundColor(theme::CONTROL.with_alpha(0.5)),
        BorderColor::all(theme::BORDER),
    ))
    .with_children(|card| {
        card.spawn(ctx.fonts.bold(status.game.name, 26.0, theme::TEXT));
        card.spawn(ctx.fonts.text(&line, 18.0, color));
        let mut enable = card.spawn((
            toggle(ctx.fonts, "Mount this game", configured.enabled),
            EnableToggle(status.game.id),
        ));
        enable.observe(on_enable_changed);
        let dir = configured
            .dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        card.spawn((
            text_field(
                ctx.fonts,
                "Install folder",
                &dir,
                "found automatically",
                256,
            ),
            DirField(status.game.id),
        ))
        .observe(on_text_changed);
    });
}

fn on_enable_changed(
    changed: On<ValueChanged>,
    toggles: Query<(&Toggle, &EnableToggle)>,
    mut content: ResMut<Content>,
) {
    let Ok((toggle, enable)) = toggles.get(changed.entity) else {
        return;
    };
    if let Some(game) = source_fs::games::find_game(enable.0) {
        content.config.content.game_mut(game).enabled = toggle.0;
    }
}

fn on_text_changed(
    changed: On<ValueChanged>,
    fields: Query<(&TextField, Option<&DirField>, Has<LibraryField>)>,
    mut draft: ResMut<GamesDraft>,
) {
    let Ok((field, dir, is_library)) = fields.get(changed.entity) else {
        return;
    };
    if let Some(DirField(game)) = dir {
        draft.dirs.insert((*game).to_owned(), field.value.clone());
    } else if is_library {
        draft.library.clone_from(&field.value);
    }
}

/// Applies the typed folders, mounts again and saves.
fn apply(world: &mut World) {
    let draft = std::mem::take(&mut *world.resource_mut::<GamesDraft>());
    let vfs = world.resource::<GameVfs>().0.clone();
    let mut content = world.resource_mut::<Content>();
    for (id, text) in draft.dirs {
        if let Some(game) = source_fs::games::find_game(&id) {
            let text = text.trim();
            content.config.content.game_mut(game).dir =
                (!text.is_empty()).then(|| PathBuf::from(text));
        }
    }
    let library = draft.library.trim();
    if !library.is_empty() {
        let library = PathBuf::from(library);
        if !content.config.content.library_paths.contains(&library) {
            content.config.content.library_paths.push(library);
        }
    }
    content.remount(&vfs);
    if let Err(error) = content.save() {
        warn!("cannot save the content settings: {error}");
    }
}
