//! User configuration for rustlife.
//!
//! Settings live in a RON file in the platform's configuration directory
//! (`~/.config/rustlife/config.ron` on Linux). Every field is optional and
//! falls back to a sensible default, so an absent file is equivalent to an
//! empty one. Unknown fields are rejected so that typos are reported instead of
//! silently ignored.
//!
//! Content-location settings can additionally be overridden from the command
//! line ([`ContentArgs`]) and the `HL2DM_DIR` environment variable, with the
//! precedence *command line > environment > config file > Steam discovery*.
//!
//! ```ron
//! (
//!     content: (
//!         game_dir: Some("/mnt/games/SteamLibrary/steamapps/common/Half-Life 2 Deathmatch"),
//!         workshop: (extra_apps: [220], disabled_items: [123456789]),
//!     ),
//! )
//! ```

#![forbid(unsafe_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use source_fs::content::{ContentOptions, WorkshopOptions};

/// Environment variable that overrides the game install folder.
pub const GAME_DIR_ENV: &str = "HL2DM_DIR";

/// The complete user configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    /// Where game content comes from.
    pub content: ContentConfig,
}

/// Content-location settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentConfig {
    /// Explicit Half-Life 2: Deathmatch install folder; discovered through
    /// Steam when absent.
    pub game_dir: Option<PathBuf>,
    /// Which workshop content to mount.
    pub workshop: WorkshopOptions,
}

/// Errors from loading the configuration file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file exists but could not be read.
    #[error("cannot read config file {path}: {source}")]
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
}

impl UserConfig {
    /// Default location of the config file, if the platform has a config
    /// directory.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "rustlife")
            .map(|dirs| dirs.config_dir().join("config.ron"))
    }

    /// Loads the config from `path`. A missing file yields the defaults.
    ///
    /// # Errors
    ///
    /// Fails if the file exists but is unreadable or invalid.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|source| ConfigError::Parse {
                path: path.to_owned(),
                source: Box::new(source),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Loads the config from [`UserConfig::default_path`], or the defaults if
    /// the platform has no config directory.
    ///
    /// # Errors
    ///
    /// See [`UserConfig::load`].
    pub fn load_default() -> Result<Self, ConfigError> {
        match Self::default_path() {
            Some(path) => Self::load(&path),
            None => Ok(Self::default()),
        }
    }

    /// Parses config text.
    ///
    /// # Errors
    ///
    /// Returns the RON error, with position, on invalid input.
    pub fn parse(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(text)
    }
}

/// Command-line options shared by every rustlife binary that mounts content.
#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub struct ContentArgs {
    /// Half-Life 2: Deathmatch install folder (the one containing `hl2mp/`).
    /// Overrides the `HL2DM_DIR` environment variable and the config file.
    #[arg(long, value_name = "DIR")]
    pub game_dir: Option<PathBuf>,

    /// Also mount workshop items of this Steam app id (repeatable), e.g. 220
    /// for Half-Life 2.
    #[arg(long = "workshop-app", value_name = "APP_ID")]
    pub workshop_apps: Vec<u32>,

    /// Do not mount any workshop content.
    #[arg(long)]
    pub no_workshop: bool,

    /// Use this config file instead of the default location.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
}

impl ContentArgs {
    /// Loads the config file selected by `--config` (or the default one).
    ///
    /// # Errors
    ///
    /// See [`UserConfig::load`].
    pub fn load_config(&self) -> Result<UserConfig, ConfigError> {
        match &self.config {
            Some(path) => UserConfig::load(path),
            None => UserConfig::load_default(),
        }
    }

    /// Combines these arguments, the environment and `config` into
    /// [`ContentOptions`], applying the documented precedence.
    #[must_use]
    pub fn resolve(&self, config: &ContentConfig) -> ContentOptions {
        self.resolve_with_env(config, std::env::var_os(GAME_DIR_ENV).map(PathBuf::from))
    }

    /// [`ContentArgs::resolve`] with the environment value passed explicitly,
    /// which keeps the precedence logic testable without mutating process
    /// state.
    #[must_use]
    pub fn resolve_with_env(
        &self,
        config: &ContentConfig,
        env_dir: Option<PathBuf>,
    ) -> ContentOptions {
        let game_dir = self
            .game_dir
            .clone()
            .or_else(|| env_dir.filter(|dir| !dir.as_os_str().is_empty()))
            .or_else(|| config.game_dir.clone());

        let mut workshop = config.workshop.clone();
        for app in &self.workshop_apps {
            if !workshop.extra_apps.contains(app) {
                workshop.extra_apps.push(*app);
            }
        }
        if self.no_workshop {
            workshop.enabled = false;
        }

        ContentOptions {
            game_dir,
            workshop,
            ..ContentOptions::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_missing_files_give_defaults() {
        assert_eq!(UserConfig::parse("()").unwrap(), UserConfig::default());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            UserConfig::load(&dir.path().join("absent.ron")).unwrap(),
            UserConfig::default()
        );
    }

    #[test]
    fn parses_full_content_section() {
        let config = UserConfig::parse(
            r#"(content: (game_dir: Some("/games/hl2dm"), workshop: (extra_apps: [220])))"#,
        )
        .unwrap();
        assert_eq!(
            config.content.game_dir.as_deref(),
            Some(Path::new("/games/hl2dm"))
        );
        assert_eq!(config.content.workshop.extra_apps, [220]);
        assert!(
            config.content.workshop.enabled,
            "unspecified fields keep defaults"
        );
    }

    #[test]
    fn rejects_unknown_fields_with_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.ron");
        fs::write(&path, "(content: (gamedir: None))").unwrap();
        let error = UserConfig::load(&path).unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }));
        assert!(error.to_string().contains("gamedir"), "{error}");
    }

    #[test]
    fn precedence_is_cli_then_env_then_config() {
        let config = ContentConfig {
            game_dir: Some("/config".into()),
            ..ContentConfig::default()
        };
        let cli = ContentArgs {
            game_dir: Some("/cli".into()),
            ..ContentArgs::default()
        };
        let none = ContentArgs::default();

        let dir = |options: ContentOptions| options.game_dir.unwrap();
        assert_eq!(
            dir(cli.resolve_with_env(&config, Some("/env".into()))),
            Path::new("/cli")
        );
        assert_eq!(
            dir(none.resolve_with_env(&config, Some("/env".into()))),
            Path::new("/env")
        );
        assert_eq!(
            dir(none.resolve_with_env(&config, Some(PathBuf::new()))),
            Path::new("/config")
        );
        assert_eq!(
            dir(none.resolve_with_env(&config, None)),
            Path::new("/config")
        );
        assert_eq!(
            none.resolve_with_env(&ContentConfig::default(), None)
                .game_dir,
            None
        );
    }

    #[test]
    fn cli_workshop_flags_merge_with_config() {
        let config = ContentConfig {
            workshop: WorkshopOptions {
                extra_apps: vec![220],
                ..WorkshopOptions::default()
            },
            ..ContentConfig::default()
        };
        let args = ContentArgs {
            workshop_apps: vec![220, 4000],
            ..ContentArgs::default()
        };
        assert_eq!(
            args.resolve_with_env(&config, None).workshop.extra_apps,
            [220, 4000]
        );

        let off = ContentArgs {
            no_workshop: true,
            ..ContentArgs::default()
        };
        assert!(!off.resolve_with_env(&config, None).workshop.enabled);
    }

    #[test]
    fn default_config_round_trips_through_ron() {
        let text =
            ron::ser::to_string_pretty(&UserConfig::default(), ron::ser::PrettyConfig::default())
                .unwrap();
        assert_eq!(UserConfig::parse(&text).unwrap(), UserConfig::default());
    }
}
