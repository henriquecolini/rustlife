//! The rustlife client: a Bevy app that mounts the player's game content and
//! renders a map.
//!
//! It loads a BSP and its props through the `source://` asset source and lets
//! the player fight through it ([`player`], [`input`]): rustlife's movement,
//! weapons and Manipulator, the map's pickups and other players
//! ([`actors`]), with a HUD ([`hud`]) and combat effects ([`effects`]); or
//! fly it in noclip. Every game runs on a server ([`net`]): one the client
//! hosts in-process — open to others with `--listen` — or a remote one it
//! joins with `--connect`. It sounds as the game does ([`audio`]).

#![forbid(unsafe_code)]

use std::sync::Arc;

use bevy::prelude::*;
use rl_assets::{SourceAssetPlugin, SourceLoadersPlugin};
use rl_data::GameData;
use source_fs::Vfs;

use crate::actors::ActorsPlugin;
use crate::animation::ModelAnimationPlugin;
use crate::audio::SoundPlugin;
use crate::camera::{CursorGrab, FlyCamPlugin};
use crate::capture::{Capture, CapturePlugin};
use crate::decals::DecalsPlugin;
use crate::effects::EffectsPlugin;
use crate::fog::FogPlugin;
use crate::grenades::GrenadesPlugin;
use crate::hud::HudPlugin;
use crate::movers::MoversPlugin;
use crate::net::{NetPlugin, NetSettings};
use crate::player::PlayerPlugin;
use crate::props::PropsPlugin;
use crate::ragdolls::RagdollsPlugin;
use crate::script::InputScript;
use crate::settings::{Settings, SettingsPlugin};
use crate::sky::SkyPlugin;
use crate::surfaces::SurfacesPlugin;
use crate::ui::UiPlugin;
use crate::viewmodel::ViewModelPlugin;
use crate::world::WorldPlugin;

pub mod actors;
pub mod animation;
pub mod audio;
pub mod camera;
pub mod capture;
pub mod content;
pub mod debris;
pub mod decals;
pub mod effects;
pub mod explosion;
pub mod fog;
mod gravity_gun;
pub mod grenades;
pub mod hud;
pub mod input;
mod laser;
mod lightning;
pub mod menu;
pub mod movers;
pub mod net;
pub mod particles;
pub mod pcf_particles;
pub mod player;
pub mod props;
pub mod ragdolls;
pub mod script;
mod selector;
pub mod settings;
pub mod sky;
pub mod surfaces;
pub mod ui;
pub mod viewmodel;
mod visual_world;
pub mod world;
pub mod zoom;

/// Top-level client state.
#[derive(States, Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum AppState {
    /// The main menu: choosing what to play.
    #[default]
    MainMenu,
    /// Joining a server, until it says which map to load.
    Connecting,
    /// Playing [`CurrentMap`].
    InGame,
    /// The server moved a running game to another map: for a moment the old
    /// map is unloaded, then [`AppState::InGame`] loads [`CurrentMap`].
    ChangingMap,
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
    /// The mounted games and the player's content settings.
    pub content: content::Content,
    /// A menu screen to open instead of the main menu (`--menu`), for
    /// screenshots.
    pub menu: Option<menu::Screen>,
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
    /// Log the frame rate and frame time every second (a developer aid).
    pub log_fps: bool,
    /// Play no sound this run, whatever the volume settings say. The
    /// settings themselves are left alone, so nothing muted is saved.
    pub mute: bool,
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
        (net::Network::Host { .. }, None) => AppState::MainMenu,
    };
    let mut app = App::new();
    app.insert_resource(GameVfs(vfs.clone()));
    let mut settings = Settings(options.content.config.settings.clone());
    if options.mute {
        settings.audio.master = 0.0;
    }
    let video = settings.video.clone();
    app.insert_resource(options.content);
    app.insert_resource(settings);
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "rustlife".to_owned(),
            resolution: video.resolution.into(),
            mode: settings::window_mode(&video),
            present_mode: settings::present_mode(&video),
            ..default()
        }),
        ..default()
    }));
    app.add_plugins(SourceLoadersPlugin);
    app.add_plugins(UiPlugin);
    app.insert_resource(CurrentMap(options.map.unwrap_or_default()));
    app.insert_state(start);
    app.insert_resource(CursorGrab(options.capture.is_none()));
    app.add_plugins(FlyCamPlugin);
    app.add_plugins((
        WorldPlugin {
            spawn: options.spawn,
        },
        PropsPlugin,
        menu::MenuPlugin {
            start: options.menu,
        },
        SettingsPlugin,
        PlayerPlugin { data: options.data },
        NetPlugin {
            settings: options.net,
        },
        ActorsPlugin,
        HudPlugin,
        EffectsPlugin,
        MoversPlugin,
        SoundPlugin,
        ModelAnimationPlugin,
        ViewModelPlugin,
        SkyPlugin,
        FogPlugin,
    ));
    app.add_plugins((
        debris::DebrisPlugin,
        particles::ParticlesPlugin,
        explosion::ExplosionPlugin,
        pcf_particles::PcfPlugin,
        GrenadesPlugin,
        SurfacesPlugin,
        RagdollsPlugin,
        DecalsPlugin,
    ));
    if let Some(script) = options.script {
        app.insert_resource(script);
    }
    if options.log_fps {
        // The HUD's plugin already measures frame times.
        app.add_plugins(bevy::diagnostic::LogDiagnosticsPlugin::default());
    }
    if let Some(capture) = options.capture {
        app.add_plugins(CapturePlugin(capture));
    }
    app.run()
}
