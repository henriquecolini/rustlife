//! `rustlife-inspect`: explore the content rustlife mounts from your game
//! install.
//!
//! ```text
//! rustlife-inspect mounts                       # search path order
//! rustlife-inspect maps                         # maps and where they come from
//! rustlife-inspect ls 'materials/concrete/*.vmt'
//! rustlife-inspect cat materials/concrete/concretefloor001a.vmt
//! rustlife-inspect extract maps/dm_lockdown.bsp --out /tmp/dm_lockdown.bsp
//! ```
//!
//! Extracted files are for local debugging only; they remain Valve's content
//! and must not be redistributed or committed.

// This is a command-line tool: printing to stdout is its purpose.
#![allow(clippy::print_stdout)]
#![forbid(unsafe_code)]

use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rl_config::ContentArgs;
use source_fs::content::{MountedGame, mount_game};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Inspect the Source content rustlife mounts from your game install"
)]
struct Cli {
    #[command(flatten)]
    content: ContentArgs,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List mounted search paths in priority order.
    Mounts,
    /// List installed workshop items that were mounted.
    Workshop,
    /// List playable maps and their origin.
    Maps,
    /// List files matching a glob (`*` within a folder, `**` across folders).
    Ls {
        /// Pattern such as `materials/concrete/*.vtf`.
        pattern: String,
    },
    /// Write a file's contents to standard output.
    Cat {
        /// Virtual path of the file.
        path: String,
    },
    /// Copy a file out of the virtual filesystem.
    Extract {
        /// Virtual path of the file.
        path: String,
        /// Destination; defaults to the file name in the current directory.
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let config = cli.content.load_config()?;
    let game = mount_game(&cli.content.resolve(&config.content))?;
    run(&cli.command, &game)
}

fn run(command: &Command, game: &MountedGame) -> Result<()> {
    let vfs = &game.vfs;
    let mut out = io::stdout().lock();
    match command {
        Command::Mounts => {
            writeln!(
                out,
                "{} ({})",
                game.gameinfo.name,
                game.gameinfo.gameinfo_dir.display()
            )?;
            for (priority, mount) in vfs.mount_list().iter().enumerate() {
                writeln!(
                    out,
                    "{priority:>3}  {:<28} {}",
                    mount.origin.to_string(),
                    mount.description
                )?;
            }
        }
        Command::Workshop => {
            if game.workshop_items.is_empty() {
                writeln!(out, "no workshop items mounted")?;
            }
            for item in &game.workshop_items {
                writeln!(
                    out,
                    "{:>12}  app {:<6} {}",
                    item.item_id,
                    item.app_id,
                    item.path.display()
                )?;
            }
        }
        Command::Maps => {
            for map in vfs.list("maps/*.bsp") {
                let name = map
                    .path
                    .trim_start_matches("maps/")
                    .trim_end_matches(".bsp");
                writeln!(out, "{name:<32} {}", map.origin)?;
            }
        }
        Command::Ls { pattern } => {
            let files = vfs.list(pattern);
            for file in &files {
                writeln!(out, "{:<72} {}", file.path, file.origin)?;
            }
            writeln!(out, "{} file(s)", files.len())?;
        }
        Command::Cat { path } => {
            let data = read(game, path)?;
            out.write_all(&data)?;
        }
        Command::Extract {
            path,
            out: destination,
        } => {
            let data = read(game, path)?;
            let destination = match destination {
                Some(destination) => destination.clone(),
                None => PathBuf::from(path.rsplit(['/', '\\']).next().unwrap_or(path)),
            };
            std::fs::write(&destination, &data)
                .with_context(|| format!("writing {}", destination.display()))?;
            writeln!(
                out,
                "wrote {} bytes to {}",
                data.len(),
                destination.display()
            )?;
        }
    }
    Ok(())
}

fn read(game: &MountedGame, path: &str) -> Result<Vec<u8>> {
    match game.vfs.read(path)? {
        Some(data) => Ok(data),
        None => bail!("{path} not found in any mounted search path"),
    }
}
