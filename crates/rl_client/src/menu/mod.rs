//! The menus: the main menu before a game and the pause menu during one.
//!
//! A single [`Menu`] resource holds the stack of screens being shown. The
//! main menu ([`AppState::MainMenu`]) always has one; in a game the stack is
//! empty until `Esc` opens the pause menu, and the game keeps running
//! behind it, as it must in a multiplayer game: the player just stops
//! sending input until they resume.
//!
//! Each screen is a function that spawns widgets from [`crate::ui`] into the
//! menu's layer; changing the stack rebuilds the layer behind a short fade.

mod backdrop;
mod games;
mod join;
mod loading;
mod main_screen;
mod pause;
mod play;
mod settings_screen;

use bevy::app::AppExit;
use bevy::input::mouse::{AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use rl_level::maps::MapEntry;

use crate::camera::CursorGrab;
use crate::content::Content;
use crate::net::{Game, NetSettings, NetStatus};
use crate::selector::WeaponSelector;
use crate::ui::{Curtain, Fonts, theme};
use crate::{AppState, GameVfs};

pub use settings_screen::SettingsTab;

/// One screen of the menus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// The main menu.
    Main,
    /// Choosing a map to host.
    Play,
    /// Joining a server.
    Join,
    /// The mounted games.
    Games,
    /// The settings, on this tab.
    Settings(SettingsTab),
    /// The in-game pause menu.
    Pause,
}

impl Screen {
    /// The screen called `name` on the command line (`--menu`), if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "main" => Self::Main,
            "play" => Self::Play,
            "join" => Self::Join,
            "games" => Self::Games,
            "pause" => Self::Pause,
            other => Self::Settings(SettingsTab::parse(
                other.strip_prefix("settings")?.trim_start_matches('-'),
            )?),
        })
    }
}

/// The screens being shown, the last on top. Empty means no menu (playing).
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct Menu {
    stack: Vec<Screen>,
}

impl Menu {
    /// A menu showing `screen`.
    #[must_use]
    pub fn showing(screen: Screen) -> Self {
        Self {
            stack: vec![screen],
        }
    }

    /// The screen on top.
    #[must_use]
    pub fn top(&self) -> Option<&Screen> {
        self.stack.last()
    }

    /// Whether a menu is up.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Opens `screen` above the current one.
    pub fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
    }

    /// Goes back one screen; returns whether there was one to leave.
    pub fn pop(&mut self) -> bool {
        self.stack.pop().is_some()
    }

    /// Replaces the top screen (a tab change, say) without growing the
    /// stack.
    pub fn replace(&mut self, screen: Screen) {
        match self.stack.last_mut() {
            Some(top) => *top = screen,
            None => self.stack.push(screen),
        }
    }

    /// Closes every screen.
    pub fn close(&mut self) {
        self.stack.clear();
    }

    /// How deep the stack is.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

/// Marks the entity holding everything the menu draws; rebuilt whenever the
/// screen changes.
#[derive(Component)]
struct MenuLayer;

/// A list the mouse wheel scrolls.
#[derive(Component)]
pub(crate) struct Scrollable;

/// What a screen needs to know to build itself.
pub(crate) struct Ctx<'a> {
    pub fonts: &'a Fonts,
    pub settings: &'a rl_config::Settings,
    pub content: &'a Content,
    pub maps: &'a [MapEntry],
    pub status: &'a NetStatus,
    pub in_game: bool,
    pub version: &'a str,
}

/// The menu systems.
pub struct MenuPlugin {
    /// A screen to open at startup instead of the main menu (`--menu`).
    pub start: Option<Screen>,
}

/// The screen asked for at startup.
#[derive(Resource)]
struct StartScreen(Option<Screen>);

impl Plugin for MenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Menu>()
            .insert_resource(StartScreen(self.start.clone()))
            .add_plugins((
                play::PlayPlugin,
                join::JoinPlugin,
                games::GamesPlugin,
                settings_screen::SettingsPlugin,
                loading::LoadingPlugin,
            ))
            .add_systems(OnEnter(AppState::MainMenu), open_main_menu)
            .add_systems(OnExit(AppState::MainMenu), close_menu)
            .add_systems(
                OnEnter(AppState::InGame),
                (close_menu, open_pause_for_capture).chain(),
            )
            .add_systems(
                Update,
                (
                    escape,
                    rebuild_layer,
                    sync_cursor,
                    scroll_lists,
                    backdrop::drift,
                )
                    .chain(),
            );
    }
}

fn open_main_menu(mut menu: ResMut<Menu>, mut start: ResMut<StartScreen>) {
    *menu = Menu::showing(Screen::Main);
    if let Some(screen) = start.0.take()
        && screen != Screen::Main
    {
        menu.push(screen);
    }
}

fn close_menu(mut menu: ResMut<Menu>) {
    menu.close();
}

/// `--menu pause` with a map: the pause menu opens as the game starts.
fn open_pause_for_capture(mut menu: ResMut<Menu>, mut start: ResMut<StartScreen>) {
    if start.0 == Some(Screen::Pause) {
        start.0 = None;
        menu.push(Screen::Pause);
    }
}

/// `Esc` backs out of a screen; in a game with no menu up it closes the
/// weapon list if that is up (HL2's `cancelselect`), else opens the pause
/// menu. It is left alone while a key is being captured for a binding,
/// which uses it to cancel.
fn escape(
    keys: Res<ButtonInput<KeyCode>>,
    state: Res<State<AppState>>,
    capturing: Res<settings_screen::Rebinding>,
    selector: Option<ResMut<WeaponSelector>>,
    mut menu: ResMut<Menu>,
) {
    if !keys.just_pressed(KeyCode::Escape) || capturing.0.is_some() {
        return;
    }
    match state.get() {
        AppState::InGame => {
            if menu.is_open() {
                menu.pop();
            } else if !selector.is_some_and(|mut selector| selector.cancel()) {
                menu.push(Screen::Pause);
            }
        }
        AppState::MainMenu if menu.depth() > 1 => {
            menu.pop();
        }
        _ => {}
    }
}

/// Rebuilds the menu's layer when the screen on top, or the app state,
/// changed.
#[allow(clippy::too_many_arguments)]
fn rebuild_layer(
    mut commands: Commands,
    menu: Res<Menu>,
    state: Res<State<AppState>>,
    fonts: Option<Res<Fonts>>,
    settings: Res<crate::settings::Settings>,
    content: Res<Content>,
    vfs: Res<GameVfs>,
    status: Res<NetStatus>,
    layers: Query<Entity, With<MenuLayer>>,
    mut shown: Local<Option<(Option<Screen>, AppState)>>,
    mut forced: Local<u32>,
) {
    let Some(fonts) = fonts else {
        return;
    };
    let now = (menu.top().cloned(), state.get().clone());
    let rebuild = *shown != Some(now.clone()) || *forced != content_revision(&content, &status);
    if !rebuild {
        return;
    }
    *forced = content_revision(&content, &status);
    let previous = shown.replace(now.clone());
    for layer in &layers {
        commands.entity(layer).despawn();
    }
    let (Some(screen), in_game) = (now.0, *state.get() == AppState::InGame) else {
        return;
    };
    if !matches!(state.get(), AppState::MainMenu | AppState::InGame) {
        return;
    }
    let maps = rl_level::maps::list_maps(&vfs.0);
    let ctx = Ctx {
        fonts: &fonts,
        settings: &settings.0,
        content: &content,
        maps: &maps,
        status: &status,
        in_game,
        version: env!("CARGO_PKG_VERSION"),
    };
    let first = previous.is_none_or(|(old, _)| old.is_none());
    spawn_layer(&mut commands, &ctx, &screen, first);
}

/// A number that changes when what screens show changes without the screen
/// changing: the mounted games, the last connection failure.
fn content_revision(content: &Content, status: &NetStatus) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for game in &content.games {
        game.game.id.hash(&mut hasher);
        game.enabled.hash(&mut hasher);
        format!("{:?}", game.state).hash(&mut hasher);
    }
    format!("{status:?}").hash(&mut hasher);
    hasher.finish() as u32
}

fn spawn_layer(commands: &mut Commands, ctx: &Ctx, screen: &Screen, first: bool) {
    let mut layer = commands.spawn((
        MenuLayer,
        Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            ..default()
        },
        GlobalZIndex(100),
        if ctx.in_game {
            BackgroundColor(theme::SCRIM)
        } else {
            BackgroundColor(theme::BACKGROUND)
        },
    ));
    layer.with_children(|layer| {
        if !ctx.in_game {
            backdrop::spawn(layer);
        }
        match screen {
            Screen::Main => main_screen::spawn(layer, ctx),
            Screen::Play => play::spawn(layer, ctx),
            Screen::Join => join::spawn(layer, ctx),
            Screen::Games => games::spawn(layer, ctx),
            Screen::Settings(tab) => settings_screen::spawn(layer, ctx, *tab),
            Screen::Pause => pause::spawn(layer, ctx),
        }
    });
    let curtain = if first && !ctx.in_game {
        Curtain::new(theme::BACKGROUND)
    } else {
        Curtain::new(Color::srgba(0.02, 0.03, 0.05, 0.55))
    };
    commands.spawn(curtain.bundle());
}

/// In a game the mouse is captured unless a menu is up; in the main menu
/// it is free.
fn sync_cursor(
    menu: Res<Menu>,
    state: Res<State<AppState>>,
    grab: Option<Res<CursorGrab>>,
    game: Game,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let Ok(mut options) = cursor.single_mut() else {
        return;
    };
    // Between matches nobody moves, and the players vote with the mouse.
    let between_matches = game.sim().is_some_and(|sim| sim.game().intermission());
    let want_grab = *state.get() == AppState::InGame
        && !menu.is_open()
        && !between_matches
        && grab.is_none_or(|grab| grab.0);
    let grabbed = options.grab_mode != CursorGrabMode::None;
    if want_grab != grabbed {
        options.grab_mode = if want_grab {
            CursorGrabMode::Locked
        } else {
            CursorGrabMode::None
        };
        options.visible = !want_grab;
    }
}

const LINE_HEIGHT: f32 = 42.0;

fn scroll_lists(
    scroll: Res<AccumulatedMouseScroll>,
    mut lists: Query<&mut ScrollPosition, With<Scrollable>>,
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

/// A screen's title and optional subtitle.
pub(crate) fn heading(parent: &mut ChildSpawnerCommands, ctx: &Ctx, title: &str, subtitle: &str) {
    parent.spawn(ctx.fonts.bold(title, 44.0, theme::TEXT));
    if !subtitle.is_empty() {
        parent.spawn(ctx.fonts.text(subtitle, 20.0, theme::TEXT_DIM));
    }
}

/// Starts a game on `map`, hosted by this client.
pub(crate) fn start_hosted(
    map: &str,
    listen: bool,
    bots: usize,
    settings: &crate::settings::Settings,
    data_port: u16,
    world: &mut World,
) {
    map.clone_into(&mut world.resource_mut::<crate::CurrentMap>().0);
    let mut net = world.resource_mut::<NetSettings>();
    net.network = crate::net::Network::Host {
        port: listen.then_some(data_port),
    };
    net.name.clone_from(&settings.player.name);
    net.dummies = bots;
    world
        .resource_mut::<NetStatus>()
        .clone_from(&NetStatus::Idle);
    world.resource_mut::<Menu>().close();
    world
        .resource_mut::<NextState<AppState>>()
        .set(AppState::InGame);
}

/// Quits to the desktop.
pub(crate) fn quit(mut exit: MessageWriter<AppExit>) {
    exit.write(AppExit::Success);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stack_opens_backs_out_and_closes() {
        let mut menu = Menu::default();
        assert!(!menu.is_open());
        assert_eq!(menu.top(), None);
        menu.push(Screen::Pause);
        menu.push(Screen::Settings(SettingsTab::Video));
        assert_eq!(menu.depth(), 2);
        menu.replace(Screen::Settings(SettingsTab::Audio));
        assert_eq!(menu.top(), Some(&Screen::Settings(SettingsTab::Audio)));
        assert_eq!(menu.depth(), 2);
        assert!(menu.pop());
        assert_eq!(menu.top(), Some(&Screen::Pause));
        menu.close();
        assert!(!menu.is_open());
        assert!(!menu.pop());
        menu.replace(Screen::Main);
        assert_eq!(menu, Menu::showing(Screen::Main));
    }

    #[test]
    fn screens_parse_from_their_command_line_names() {
        assert_eq!(Screen::parse("main"), Some(Screen::Main));
        assert_eq!(Screen::parse("play"), Some(Screen::Play));
        assert_eq!(Screen::parse("games"), Some(Screen::Games));
        assert_eq!(
            Screen::parse("settings-audio"),
            Some(Screen::Settings(SettingsTab::Audio))
        );
        assert_eq!(
            Screen::parse("settings"),
            Some(Screen::Settings(SettingsTab::Video))
        );
        assert_eq!(Screen::parse("settings-nope"), None);
        assert_eq!(Screen::parse("nope"), None);
    }
}
