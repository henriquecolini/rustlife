//! `rustlife-server`: a dedicated server, headless.
//!
//! ```text
//! rustlife-server
//! ```
//!
//! It needs no arguments: everything is configured in `server.ron` (in the
//! rustlife configuration directory; a commented default is written on the
//! first run). It mounts the configured games (for the maps' collision and
//! props; it draws nothing), loads a map and serves it over UDP until
//! stopped, ticking at the gameplay data's rate. Clients join from the
//! game's menu.
//!
//! The command-line options are developer aids.

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use rl_config::{ContentArgs, ServerConfig};
use rl_data::GameData;
use rl_level::maps::deathmatch_maps;
use rl_server::{Server, ServerOptions};
use source_fs::Vfs;
use source_fs::content::mount_games;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(version, about = "The rustlife dedicated server")]
struct Cli {
    /// Content-location overrides (the server's own settings are in its
    /// config file).
    #[command(flatten)]
    content: ContentArgs,

    /// Server config file to use instead of the one in the config directory.
    #[arg(long, value_name = "FILE")]
    server_config: Option<PathBuf>,

    /// Serve this map first, instead of the first one configured.
    #[arg(long)]
    map: Option<String>,

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
    let config_path = cli
        .server_config
        .clone()
        .or_else(ServerConfig::default_path)
        .unwrap_or_else(|| PathBuf::from("server.ron"));
    let config = ServerConfig::load_or_create(&config_path)?;
    tracing::info!(config = %config_path.display(), name = config.name, "configuration loaded");

    let vfs = Vfs::new();
    mount_configured_games(&vfs, &config, &cli.content)?;

    let mut data = GameData::load(cli.data_dir.as_deref())?;
    config.apply_to(&mut data);
    let maps = if config.maps.is_empty() {
        deathmatch_maps(&vfs)
    } else {
        config.maps.clone()
    };
    let map = cli
        .map
        .clone()
        .or_else(|| maps.first().cloned())
        .context("no maps to serve: configure `maps` or mount a game that has some")?;
    let level =
        rl_level::load(&vfs, &map, &data.physics).with_context(|| format!("loading {map}"))?;
    let port = config.port;
    let tick = Duration::from_secs_f32(data.simulation.tick_interval());
    let mut server = Server::new(
        &level,
        ServerOptions {
            data,
            games: config.games.clone(),
            listen: Some(SocketAddr::from((config.bind, port))),
            dummies: cli.dummies,
        },
    )?;
    tracing::info!(map, addresses = ?server.addresses(), "serving");

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

/// Mounts every game the config requires, failing if one cannot be mounted,
/// since players would then be refused or play with different content.
fn mount_configured_games(vfs: &Vfs, config: &ServerConfig, args: &ContentArgs) -> Result<()> {
    let mut content = config.content.clone();
    args.apply(&mut content);
    let plans = content.plans(Some(&config.games));
    for id in &config.games {
        if !plans.iter().any(|plan| plan.game.id == id) {
            bail!("game `{id}` in the server config is unknown or disabled");
        }
    }
    for mount in mount_games(vfs, &plans) {
        match mount.outcome {
            Ok(info) => tracing::info!(
                game = mount.game.name,
                dir = %info.game_dir.display(),
                "mounted"
            ),
            Err(error) => bail!("cannot mount {}: {error}", mount.game.name),
        }
    }
    Ok(())
}
