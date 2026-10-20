//! The rustlife client binary: mount the player's game content and play a map.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rl_client::ClientOptions;
use rl_client::capture::Capture;
use rl_client::player::ScriptedForward;
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

    /// Render non-interactively, save one screenshot to this PNG path, and exit.
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,
}

/// Seconds a non-interactive run waits for the map and its textures to load.
const LOAD_SECS: f32 = 3.0;

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Logging is set up by Bevy's `LogPlugin`; `RUST_LOG` still applies.
    let config = cli.content.load_config()?;
    let data = GameData::load(cli.data_dir.as_deref())?;
    let game = mount_game(&cli.content.resolve(&config.content))?;
    let scripted_forward = cli.walk_forward.map(|duration_secs| ScriptedForward {
        start_secs: LOAD_SECS,
        duration_secs,
    });
    let capture = cli.screenshot.map(|path| Capture {
        path,
        warmup_secs: LOAD_SECS + cli.walk_forward.map_or(0.0, |secs| (secs - 0.2).max(0.0)),
    });
    rl_client::run(
        Arc::new(game.vfs),
        ClientOptions {
            map: cli.map,
            spawn: cli.spawn,
            capture,
            scripted_forward,
            data,
        },
    );
    Ok(())
}
