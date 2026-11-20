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
use crate::effect_material::EffectMaterialPlugin;
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
mod bolts;
pub mod camera;
pub mod capture;
mod combine_ball;
pub mod content;
pub mod debris;
pub mod decals;
pub mod effect_material;
pub mod effects;
pub mod explosion;
pub mod fog;
mod gravity_gun;
pub mod grenades;
pub mod hud;
mod impact_debris;
pub mod input;
mod laser;
mod lightning;
mod map_decals;
pub mod menu;
pub mod movers;
pub mod net;
pub mod particles;
pub mod pcf_particles;
mod physcannon_impact;
pub mod player;
pub mod player_anim;
pub mod prepare;
pub mod props;
pub mod ragdolls;
mod screen_fade;
pub mod script;
mod selector;
pub mod settings;
pub mod sky;
pub mod surfaces;
mod trail_sparks;
pub mod ui;
mod view_punch;
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
    /// Fetching [`CurrentMap`]'s files before playing it, when the game's
    /// content arrives asynchronously (a browser). Never entered otherwise.
    Preparing,
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
    /// Play no sound this run, whatever the volume settings say
    /// (`--silent`). The settings themselves are left alone, so a silent
    /// run never saves its silence to the config.
    pub silent: bool,
    /// The page element (a CSS selector such as `#game`) to draw into, in a
    /// browser; `None` elsewhere.
    pub canvas: Option<String>,
    /// The game's files arrive asynchronously (a browser), so a map's files
    /// are fetched before it is entered.
    pub async_content: bool,
    /// Draw at this many pixels per logical pixel instead of the display's
    /// own ratio: a phone's screen has three, which costs the GPU nine times
    /// a 1x screen's pixels. `None` keeps the display's.
    pub scale_factor: Option<f32>,
}

impl ClientOptions {
    /// Options to start at the menu with `content` and `data`, with nothing
    /// scripted: what an interactive run needs, for callers that do not build
    /// them from a command line.
    #[must_use]
    pub fn interactive(content: content::Content, data: GameData, net: NetSettings) -> Self {
        Self {
            content,
            menu: None,
            map: None,
            spawn: None,
            capture: None,
            script: None,
            data,
            net,
            log_fps: false,
            silent: false,
            canvas: None,
            async_content: false,
            scale_factor: None,
        }
    }
}

/// The game's window, as the video settings and the platform ask.
fn primary_window(options: &ClientOptions, video: &rl_config::VideoSettings) -> Window {
    let resolution: bevy::window::WindowResolution = video.resolution.into();
    Window {
        title: "rustlife".to_owned(),
        resolution: match options.scale_factor {
            Some(factor) => resolution.with_scale_factor_override(factor),
            None => resolution,
        },
        mode: settings::window_mode(video),
        present_mode: settings::present_mode(video),
        // In a browser the game fills its canvas, which follows the page,
        // and keeps keys such as Tab and the arrows from scrolling it.
        fit_canvas_to_parent: options.canvas.is_some(),
        canvas: options.canvas.clone(),
        prevent_default_event_handling: true,
        ..default()
    }
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
        (net::Network::Host { .. }, Some(_)) => {
            prepare::state_for_game(prepare::AsyncContent(options.async_content))
        }
        (net::Network::Host { .. }, None) => AppState::MainMenu,
    };
    let mut app = App::new();
    app.insert_resource(GameVfs(vfs.clone()));
    let settings = Settings(options.content.config.settings.clone());
    let video = settings.video.clone();
    let window = primary_window(&options, &video);
    app.insert_resource(options.content);
    app.insert_resource(settings);
    app.add_plugins(SourceAssetPlugin::new(vfs));
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(window),
        ..default()
    }));
    app.add_plugins(SourceLoadersPlugin);
    app.add_plugins(prepare::PreparePlugin {
        content: prepare::AsyncContent(options.async_content),
    });
    app.add_plugins((UiPlugin, EffectMaterialPlugin));
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
        SoundPlugin {
            silent: options.silent,
        },
        ModelAnimationPlugin,
        ViewModelPlugin,
        SkyPlugin,
        FogPlugin,
    ));
    app.add_plugins((
        debris::DebrisPlugin,
        particles::ParticlesPlugin,
        explosion::ExplosionPlugin,
        trail_sparks::TrailSparksPlugin,
        impact_debris::ImpactDebrisPlugin,
        physcannon_impact::PhyscannonImpactPlugin,
        combine_ball::CombineBallPlugin,
        bolts::BoltsPlugin,
        screen_fade::ScreenFadePlugin,
        view_punch::ViewPunchPlugin,
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
