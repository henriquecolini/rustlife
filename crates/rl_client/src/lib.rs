//! The rustlife client: a Bevy app that mounts the player's game content and
//! renders a map.
//!
//! It loads a BSP and its props through the `source://` asset source and lets
//! the player walk it with rustlife's movement ([`player`]), or fly it in
//! noclip. Later milestones add physics, combat, networking and audio on top
//! of the same app.

#![forbid(unsafe_code)]

use std::sync::Arc;

use bevy::prelude::*;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use rl_data::GameData;
use source_fs::Vfs;

use crate::browser::BrowserPlugin;
use crate::camera::{CursorGrab, FlyCamPlugin};
use crate::capture::{Capture, CapturePlugin};
use crate::player::{PlayerPlugin, ScriptedForward};
use crate::props::PropsPlugin;
use crate::world::WorldPlugin;

pub mod browser;
pub mod camera;
pub mod capture;
pub mod player;
pub mod props;
pub mod world;

/// Top-level client state.
#[derive(States, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    /// Choosing a map in the [`browser`].
    #[default]
    Browser,
    /// Viewing [`CurrentMap`].
    InGame,
}

/// The map to load on entering [`AppState::InGame`], without path or extension.
#[derive(Resource, Debug, Clone, Default)]
pub struct CurrentMap(pub String);

/// The mounted game content, shared with systems (e.g. to mount a map's
/// pakfile on load). Cloning is cheap — it is an `Arc`.
#[derive(Resource, Clone)]
pub struct GameVfs(pub Arc<Vfs>);

/// How the client should start.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Map to load straight away (without path or extension); `None` opens
    /// the map browser.
    pub map: Option<String>,
    /// Index of the spawn point to start at; wraps around the map's spawns.
    pub spawn: usize,
    /// Run non-interactively and save one screenshot (see [`capture`]).
    pub capture: Option<Capture>,
    /// Scripted forward movement, for captures of the player moving.
    pub scripted_forward: Option<ScriptedForward>,
    /// The gameplay data to play with.
    pub data: GameData,
}

/// Builds and runs the client over the content mounted in `vfs`.
///
/// With a map set, the client goes straight into it; otherwise it opens the
/// map browser. When capturing, the client runs non-interactively: it does
/// not grab the cursor, saves one screenshot after a warmup, and exits.
///
/// The `source://` asset source is registered before `DefaultPlugins` (as Bevy
/// requires for custom sources) and the loaders after it.
pub fn run(vfs: Arc<Vfs>, options: ClientOptions) -> AppExit {
    let start = if options.map.is_some() {
        AppState::InGame
    } else {
        AppState::Browser
    };
    let mut app = App::new();
    app.insert_resource(GameVfs(vfs.clone()));
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "rustlife".to_owned(),
            ..default()
        }),
        ..default()
    }));
    app.add_plugins(SourceLoadersPlugin);
    app.insert_resource(CurrentMap(options.map.unwrap_or_default()));
    app.insert_state(start);
    app.insert_resource(CursorGrab(options.capture.is_none()));
    app.add_plugins(FlyCamPlugin);
    app.add_plugins((
        WorldPlugin {
            spawn: options.spawn,
        },
        PropsPlugin,
        BrowserPlugin,
        PlayerPlugin { data: options.data },
    ));
    if let Some(script) = options.scripted_forward {
        app.insert_resource(script);
    }
    if let Some(capture) = options.capture {
        app.add_plugins(CapturePlugin(capture));
    }
    app.run()
}
