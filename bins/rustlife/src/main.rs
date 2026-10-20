//! The rustlife client binary: mount the player's game content and play a map.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rl_client::ClientOptions;
use rl_client::capture::Capture;
use rl_client::script::InputScript;
use rl_config::ContentArgs;
use rl_data::GameData;
use source_fs::content::mount_game;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "rustlife — an original arena FPS that loads your Source content"
)]
struct Cli {
    #[command(flatten)]
    content: ContentArgs,

    /// Map to load directly, without path or extension (e.g. `dm_lockdown`).
    /// Omit it to start in the map browser.
    #[arg(long)]
    map: Option<String>,

    /// Spawn point to start at (wraps around the map's spawn count).
    #[arg(long, default_value_t = 0)]
    spawn: usize,

    /// Directory of gameplay data files overriding the built-in ones, file
    /// by file (e.g. a tweaked `movement.ron`).
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Hold forward for this many seconds once the map has had time to load;
    /// with `--screenshot`, the shot is taken just before letting go (for
    /// scripted captures of movement).
    #[arg(long, value_name = "SECS")]
    walk_forward: Option<f32>,

    /// Once the map has had time to load, go to the grabbable prop nearest
    /// the player and pick it up with the Manipulator (for scripted
    /// captures); with `--screenshot`, the shot shows it held.
    #[arg(long)]
    grab_prop: bool,

    /// With `--grab-prop`, throw the prop this many seconds after going for
    /// it; the shot is taken a moment after the throw.
    #[arg(long, value_name = "SECS", requires = "grab_prop")]
    throw_after: Option<f32>,

    /// Render non-interactively, save one screenshot to this PNG path, and exit.
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,
}

/// Seconds a non-interactive run waits for the map and its textures to load.
const LOAD_SECS: f32 = 3.0;
/// Seconds a scripted grab takes to settle the prop in front of the player.
const GRAB_SECS: f32 = 1.5;
/// Seconds after a scripted throw before the shot, with the prop in flight.
const THROW_SHOT_SECS: f32 = 0.2;

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Logging is set up by Bevy's `LogPlugin`; `RUST_LOG` still applies.
    let config = cli.content.load_config()?;
    let data = GameData::load(cli.data_dir.as_deref())?;
    let game = mount_game(&cli.content.resolve(&config.content))?;
    let grab_at = cli.grab_prop.then_some(LOAD_SECS);
    let fire_at = cli.throw_after.map(|secs| LOAD_SECS + secs);
    let walk = cli.walk_forward.map(|secs| (LOAD_SECS, secs));
    let script =
        (walk.is_some() || grab_at.is_some()).then(|| InputScript::new(walk, grab_at, fire_at));
    let shot_secs = match (fire_at, grab_at, cli.walk_forward) {
        (Some(fire), _, _) => fire + THROW_SHOT_SECS,
        (None, Some(grab), _) => grab + GRAB_SECS,
        (None, None, walk) => LOAD_SECS + walk.map_or(0.0, |secs| (secs - 0.2).max(0.0)),
    };
    let capture = cli.screenshot.map(|path| Capture {
        path,
        warmup_secs: shot_secs,
    });
    rl_client::run(
        Arc::new(game.vfs),
        ClientOptions {
            map: cli.map,
            spawn: cli.spawn,
            capture,
            script,
            data,
        },
    );
    Ok(())
}
