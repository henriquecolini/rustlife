//! The rustlife client: a Bevy app that mounts the player's game content and
//! renders a map.
//!
//! It loads a BSP and its props through the `source://` asset source and lets
//! the player fight through it ([`player`], [`input`]): rustlife's movement,
//! weapons and Manipulator, the map's pickups and other players
//! ([`actors`]), with a HUD ([`hud`]) and combat effects ([`effects`]); or
//! fly it in noclip. Every game runs on a server ([`net`]): one the client
//! hosts in-process — open to others with `--listen` — or a remote one it
//! joins with `--connect`. Audio comes with a later milestone.

#![forbid(unsafe_code)]

use std::sync::Arc;

use bevy::prelude::*;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use rl_data::GameData;
use source_fs::Vfs;

use crate::actors::ActorsPlugin;
use crate::browser::BrowserPlugin;
use crate::camera::{CursorGrab, FlyCamPlugin};
use crate::capture::{Capture, CapturePlugin};
use crate::effects::EffectsPlugin;
use crate::hud::HudPlugin;
use crate::movers::MoversPlugin;
use crate::net::{NetPlugin, NetSettings};
use crate::player::PlayerPlugin;
use crate::props::PropsPlugin;
use crate::script::InputScript;
use crate::world::WorldPlugin;

pub mod actors;
pub mod browser;
pub mod camera;
pub mod capture;
pub mod effects;
pub mod hud;
pub mod input;
pub mod movers;
pub mod net;
pub mod player;
pub mod props;
pub mod script;
pub mod world;

/// Top-level client state.
#[derive(States, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    /// Choosing a map in the [`browser`].
    #[default]
    Browser,
    /// Joining a server, until it says which map to load.
    Connecting,
    /// Playing [`CurrentMap`].
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
    /// Map to host straight away (without path or extension); `None` opens
    /// the map browser (or, when joining, waits for the server's map).
    pub map: Option<String>,
    /// Index of the spawn point to start at, if any; wraps around the
    /// map's spawns.
    pub spawn: Option<usize>,
    /// Run non-interactively and save one screenshot (see [`capture`]).
    pub capture: Option<Capture>,
    /// Scripted input, for captures of the player moving and using the
    /// Manipulator.
    pub script: Option<InputScript>,
    /// The gameplay data to play with (a server's own replaces it).
    pub data: GameData,
    /// Hosting or joining, the player's name, simulated network trouble,
    /// training dummies.
    pub net: NetSettings,
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
    let start = match (&options.net.network, &options.map) {
        (net::Network::Join(_), _) => AppState::Connecting,
        (net::Network::Host { .. }, Some(_)) => AppState::InGame,
        (net::Network::Host { .. }, None) => AppState::Browser,
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
        NetPlugin {
            settings: options.net,
        },
        ActorsPlugin,
        HudPlugin,
        EffectsPlugin,
        MoversPlugin,
    ));
    if let Some(script) = options.script {
        app.insert_resource(script);
    }
    if let Some(capture) = options.capture {
        app.add_plugins(CapturePlugin(capture));
    }
    app.run()
}
