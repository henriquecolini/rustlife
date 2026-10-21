//! Configuration for rustlife: the player's settings file, the dedicated
//! server's single config file, and the shared content-location settings.
//!
//! Files are RON in the platform's configuration directory
//! (`~/.config/rustlife/` on Linux): `config.ron` for the client and
//! `server.ron` for the dedicated server. Every field is optional and falls
//! back to a sensible default, so an absent file is equivalent to an empty
//! one. Unknown fields are rejected so that typos are reported instead of
//! silently ignored.
//!
//! Neither players nor server admins need command-line arguments. The
//! content options in [`ContentArgs`] (and the `HL2DM_DIR` environment
//! variable) remain as developer overrides, with the precedence *command line
//! > environment > config file > Steam discovery*.

#![forbid(unsafe_code)]

mod client;
mod content;
mod server;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use client::UserConfig;
pub use content::{ContentArgs, ContentConfig, GAME_DIR_ENV, GameConfig};
pub use server::ServerConfig;

/// Errors from loading or saving a configuration file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read, or could not be written.
    #[error("cannot access config file {path}: {source}")]
    Io {
        /// The config file.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid RON or does not match the schema.
    #[error("invalid config file {path}: {source}")]
    Parse {
        /// The config file.
        path: PathBuf,
        /// Parser error with position. Boxed because `SpannedError` is large,
        /// and without boxing it would inflate every `Result<_, ConfigError>`
        /// (see clippy's `result_large_err`).
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    /// The configuration could not be serialised.
    #[error("cannot serialise config: {0}")]
    Serialise(#[from] ron::Error),
}

/// The platform's rustlife configuration directory, if it has one.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rustlife").map(|dirs| dirs.config_dir().to_owned())
}

/// Reads and parses `path`; `Ok(None)` when it does not exist.
fn read_ron<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => ron::from_str(&text)
            .map(Some)
            .map_err(|source| ConfigError::Parse {
                path: path.to_owned(),
                source: Box::new(source),
            }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

/// Writes `text` to `path`, creating the directory, through a temporary file
/// so that a crash cannot leave a half-written config behind.
fn write_text(path: &Path, text: &str) -> Result<(), ConfigError> {
    let io_error = |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let temporary = path.with_extension("ron.tmp");
    fs::write(&temporary, text).map_err(io_error)?;
    fs::rename(&temporary, path).map_err(io_error)
}

/// Serialises `value` as pretty RON.
fn to_ron<T: Serialize>(value: &T) -> Result<String, ConfigError> {
    Ok(ron::ser::to_string_pretty(
        value,
        ron::ser::PrettyConfig::default(),
    )?)
}
