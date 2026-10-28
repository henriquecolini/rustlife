//! The rustlife client binary: mount the player's game content and play a map.
//!
//! ```text
//! rustlife --map dm_lockdown                    # host a game, alone
//! rustlife --map dm_lockdown --listen           # host it for others too (--port)
//! rustlife --connect 192.168.1.20:27015         # join a server
//! rustlife --connect 127.0.0.1:27015 --net-sim 100ms,10ms,2%
//! ```

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use rl_client::ClientOptions;
use rl_client::capture::Capture;
use rl_client::content::Content;
use rl_client::net::{NetSettings, Network};
use rl_client::script::InputScript;
use rl_config::ContentArgs;
use rl_data::GameData;
use rl_net::conditioner::NetConditions;
use source_fs::Vfs;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "rustlife — an original arena FPS that loads your Source content"
)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a command-line switch of its own"
)]
struct Cli {
    #[command(flatten)]
    content: ContentArgs,

    /// Map to host directly, without path or extension (e.g. `dm_lockdown`).
    /// Omit it to start in the map browser.
    #[arg(long, conflicts_with = "connect")]
    map: Option<String>,

    /// Join the server at this address (e.g. `127.0.0.1:27015`) instead of
    /// hosting.
    #[arg(long, value_name = "ADDRESS")]
    connect: Option<SocketAddr>,

    /// Open the hosted game to other players over UDP.
    #[arg(long, conflicts_with = "connect")]
    listen: bool,

    /// With `--listen`, the UDP port (default: `net.ron`'s).
    #[arg(long, requires = "listen")]
    port: Option<u16>,

    /// Hold primary fire for this many seconds after the map has loaded
    /// (wherever the view points).
    #[arg(long, value_name = "SECS", hide = true)]
    fire_for: Option<f32>,
    /// Hold secondary fire for this many seconds after the map has loaded.
    #[arg(long, value_name = "SECS", hide = true)]
    alt_fire_for: Option<f32>,
    /// Play the particle system of this name (from the game's `.pcf` files)
    /// in front of the view once the map has loaded.
    #[arg(long, value_name = "NAME", hide = true)]
    pcf: Option<String>,
    /// Set off the game's explosion where the view hits the map once it has
    /// loaded.
    #[arg(long, hide = true)]
    explode: bool,

    /// Open this menu screen at startup (`main`, `play`, `join`, `games`,
    /// `settings`, `settings-audio`, `settings-controls`, `settings-player`),
    /// for screenshots.
    #[arg(long, value_name = "SCREEN", hide = true)]
    menu: Option<String>,

    /// The name other players see.
    #[arg(long, default_value = "player")]
    name: String,

    /// Simulate a bad network: added round-trip latency, jitter and loss,
    /// e.g. `100ms,10ms,2%`.
    #[arg(long, value_name = "LATENCY[,JITTER[,LOSS]]")]
    net_sim: Option<NetConditions>,

    /// Spawn point to start at (wraps around the map's spawn count); in a
    /// hosted game, the player is moved there once in.
    #[arg(long)]
    spawn: Option<usize>,

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

    /// With `--grab-prop`, look this many degrees above the horizon once the
    /// prop is in hand, so that it is thrown into the air.
    #[arg(long, value_name = "DEGREES", requires = "grab_prop")]
    throw_pitch: Option<f32>,

    /// Add this many training dummies to a hosted game: players who stand
    /// still where they spawn and respawn when killed.
    #[arg(long, default_value_t = 0, value_name = "N")]
    dummies: usize,

    /// Take this weapon (by id, e.g. `launcher`) in hand from the start,
    /// with plenty of ammo (for scripted captures).
    #[arg(long, value_name = "ID")]
    weapon: Option<String>,

    /// Carry every weapon, fully loaded, with unlimited ammo (for trying
    /// the weapons out).
    #[arg(long)]
    all_weapons: bool,

    /// Once the map has had time to load, go to the nearest other player
    /// and fire at them for this many seconds (for scripted captures); the
    /// shot is taken as firing ends.
    #[arg(long, value_name = "SECS")]
    shoot_for: Option<f32>,

    /// Once the map has had time to load, go and look at the nearest
    /// pickup (for scripted captures).
    #[arg(long)]
    visit_pickup: bool,

    /// Once the map has had time to load, go to the nearest of the map's
    /// doors, buttons or other movers of this class (e.g. `func_button`)
    /// and press use on it (for scripted captures); the shot is taken
    /// `--wait` seconds later.
    #[arg(long, value_name = "CLASS")]
    use_mover: Option<String>,

    /// Stand at this point (Source units) from the start, looking at this
    /// pitch and yaw (degrees), for scripted captures.
    #[arg(long, value_name = "X,Y,Z[,PITCH,YAW]", value_parser = parse_teleport)]
    teleport: Option<([f32; 3], (f32, f32))>,

    /// Seconds to wait before a scripted capture's shot, after whatever
    /// else it does (e.g. for a door to open).
    #[arg(long, value_name = "SECS", default_value_t = 0.0)]
    wait: f32,

    /// Log the frame rate and frame time every second.
    #[arg(long)]
    log_fps: bool,

    /// Play no sound, whatever the volume settings say; the settings are
    /// not changed. Always on for screenshots and scripted captures, which
    /// are run unattended.
    #[arg(long, visible_alias = "mute")]
    silent: bool,

    /// Render non-interactively, save one screenshot to this PNG path, and exit.
    #[arg(long, value_name = "PNG")]
    screenshot: Option<PathBuf>,
}

/// Seconds a non-interactive run waits for the map and its textures to load.
const LOAD_SECS: f32 = 3.0;
/// Seconds a scripted grab takes to settle the prop in front of the player.
const GRAB_SECS: f32 = 1.5;
/// Seconds a scripted visit to a pickup waits for its model to light.
const VISIT_SECS: f32 = 0.5;
/// Seconds after a scripted throw before the shot, with the prop in flight
/// (props are drawn a tenth of a second in the past).
const THROW_SHOT_SECS: f32 = 0.35;

/// `X,Y,Z` or `X,Y,Z,PITCH,YAW`.
fn parse_teleport(text: &str) -> Result<([f32; 3], (f32, f32)), String> {
    let numbers = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<f32>, String>>()?;
    match numbers[..] {
        [x, y, z] => Ok(([x, y, z], (0.0, 0.0))),
        [x, y, z, pitch, yaw] => Ok(([x, y, z], (pitch, yaw))),
        _ => Err("expected X,Y,Z or X,Y,Z,PITCH,YAW".to_owned()),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Logging is set up by Bevy's `LogPlugin`; `RUST_LOG` still applies.
    let mut config = cli.content.load_config()?;
    cli.content.apply(&mut config.content);
    let mut data = GameData::load(cli.data_dir.as_deref())?;
    let vfs = Arc::new(Vfs::new());
    let content = Content::mount(config, cli.content.config_path(), &vfs);
    let grab_at = cli.grab_prop.then_some(LOAD_SECS);
    let fire_at = cli.throw_after.map(|secs| LOAD_SECS + secs);
    let walk = cli.walk_forward.map(|secs| (LOAD_SECS, secs));
    let shoot = cli.shoot_for.map(|secs| (LOAD_SECS, secs));
    let held_fire = cli.fire_for.map(|secs| (LOAD_SECS, secs));
    let held_alt = cli.alt_fire_for.map(|secs| (LOAD_SECS, secs));
    let visit = cli.visit_pickup.then_some(LOAD_SECS);
    let use_mover = cli.use_mover.map(|class| (LOAD_SECS, class));
    let scripted = walk.is_some()
        || grab_at.is_some()
        || shoot.is_some()
        || held_fire.is_some()
        || held_alt.is_some()
        || visit.is_some()
        || use_mover.is_some()
        || cli.teleport.is_some()
        || cli.pcf.is_some()
        || cli.explode
        || cli.weapon.is_some()
        || cli.all_weapons;
    let script = scripted.then(|| {
        InputScript::new(walk, grab_at, fire_at)
            .with_weapon(cli.weapon)
            .with_all_weapons(cli.all_weapons)
            .with_shooting(shoot)
            .with_fire_held(held_fire)
            .with_alt_fire_held(held_alt)
            .with_visit(visit)
            .with_use(use_mover.clone())
            .with_teleport(cli.teleport)
            .with_throw_pitch(cli.throw_pitch)
            .with_pcf(cli.pcf)
            .with_explosion(cli.explode)
    });
    let shot_secs = match (fire_at, grab_at, shoot) {
        (Some(fire), _, _) => fire + THROW_SHOT_SECS,
        (None, Some(grab), _) => grab + GRAB_SECS,
        (None, None, Some((start, secs))) => start + secs,
        (None, None, None) if held_fire.is_some() => {
            held_fire.map_or(LOAD_SECS, |(start, secs)| start + secs)
        }
        (None, None, None) if visit.is_some() => LOAD_SECS + VISIT_SECS,
        (None, None, None) if use_mover.is_some() => LOAD_SECS + VISIT_SECS,
        (None, None, None) => {
            LOAD_SECS + cli.walk_forward.map_or(0.0, |secs| (secs - 0.2).max(0.0))
        }
    } + cli.wait;
    if scripted || cli.screenshot.is_some() {
        // Captures and scripts act at fixed times: no countdown to wait out.
        data.rules.warmup = 0.0;
    }
    let silent = cli.silent || scripted || cli.screenshot.is_some();
    let capture = cli.screenshot.map(|path| Capture {
        path,
        warmup_secs: shot_secs,
    });
    let network = match cli.connect {
        Some(address) => Network::Join(address),
        None => Network::Host {
            port: cli.listen.then(|| cli.port.unwrap_or(data.net.port)),
        },
    };
    let net = NetSettings {
        network,
        name: cli.name,
        conditions: cli.net_sim.unwrap_or(NetConditions::NONE),
        dummies: cli.dummies,
    };
    rl_client::run(
        vfs,
        ClientOptions {
            content,
            menu: cli.menu.as_deref().and_then(rl_client::menu::Screen::parse),
            map: cli.map,
            spawn: cli.spawn,
            capture,
            script,
            data,
            net,
            log_fps: cli.log_fps,
            silent,
        },
    );
    Ok(())
}
