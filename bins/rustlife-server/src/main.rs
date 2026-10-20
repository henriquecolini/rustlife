//! `rustlife-server`: a dedicated server, headless.
//!
//! ```text
//! rustlife-server --game-dir <HL2DM> --map dm_overwatch [--port 27015] [--dummies 2]
//! ```
//!
//! It mounts the game content (for the map's collision and props; it draws
//! nothing), loads the level and serves it over UDP until stopped, ticking
//! at the gameplay data's rate. Clients join with
//! `rustlife --connect <address>:<port>`.

#![forbid(unsafe_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use rl_config::ContentArgs;
use rl_data::GameData;
use rl_server::{Server, ServerOptions};
use source_fs::content::mount_game;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about = "The rustlife dedicated server")]
struct Cli {
    #[command(flatten)]
    content: ContentArgs,

    /// Map to serve, without path or extension (e.g. `dm_overwatch`).
    #[arg(long)]
    map: String,

    /// UDP port to listen on (default: `net.ron`'s).
    #[arg(long)]
    port: Option<u16>,

    /// Address to listen on.
    #[arg(long, default_value_t = Ipv4Addr::UNSPECIFIED)]
    bind: Ipv4Addr,

    /// Directory of gameplay data files overriding the built-in ones, file
    /// by file; clients are sent the result.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Add this many training dummies: players who stand still and respawn.
    #[arg(long, default_value_t = 0, value_name = "N")]
    dummies: usize,

    /// Stop after this many seconds (for scripted tests).
    #[arg(long, value_name = "SECS")]
    run_for: Option<f32>,
}

/// How often the server logs who is playing and how long ticks take.
const STATUS_INTERVAL: Duration = Duration::from_secs(30);

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let config = cli.content.load_config()?;
    let data = GameData::load(cli.data_dir.as_deref())?;
    let game = mount_game(&cli.content.resolve(&config.content))?;
    let level = rl_level::load(&game.vfs, &cli.map, &data.physics)
        .with_context(|| format!("loading {}", cli.map))?;
    let port = cli.port.unwrap_or(data.net.port);
    let tick = Duration::from_secs_f32(data.simulation.tick_interval());
    let mut server = Server::new(
        &level,
        ServerOptions {
            data,
            listen: Some(SocketAddr::from((cli.bind, port))),
            dummies: cli.dummies,
        },
    )?;
    tracing::info!(map = cli.map, addresses = ?server.addresses(), "serving");

    let start = Instant::now();
    let stop = cli.run_for.map(Duration::from_secs_f32);
    let mut status = start;
    let mut busiest = Duration::ZERO;
    while stop.is_none_or(|stop| start.elapsed() < stop) {
        let began = Instant::now();
        server.update(start.elapsed());
        let busy = began.elapsed();
        busiest = busiest.max(busy);
        if status.elapsed() >= STATUS_INTERVAL {
            status = Instant::now();
            let players: Vec<&str> = server.players().map(|(_, name)| name).collect();
            tracing::info!(
                ?players,
                busiest_ms = busiest.as_secs_f32() * 1000.0,
                "status"
            );
            busiest = Duration::ZERO;
        }
        // Sleeping half a tick keeps ticks punctual without spinning.
        std::thread::sleep(tick.saturating_sub(busy) / 2);
    }
    server.shut_down();
    // Let the disconnections go out.
    server.update(start.elapsed());
    Ok(())
}
