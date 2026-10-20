//! The rustlife client binary: mount the player's game content and view a map.

#![forbid(unsafe_code)]

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rl_config::ContentArgs;
use source_fs::content::mount_game;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "rustlife — an original arena FPS that loads your Source content"
)]
struct Cli {
    #[command(flatten)]
    content: ContentArgs,

    /// Map to load, without path or extension (e.g. `dm_lockdown`).
    #[arg(long, default_value = "dm_lockdown")]
    map: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Logging is set up by Bevy's `LogPlugin`; `RUST_LOG` still applies.
    let config = cli.content.load_config()?;
    let game = mount_game(&cli.content.resolve(&config.content))?;
    rl_client::run(Arc::new(game.vfs), cli.map);
    Ok(())
}
