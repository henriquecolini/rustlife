//! Which games to mount and where to find them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{ConfigError, UserConfig};
use source_fs::content::{ContentOptions, GamePlan, WorkshopOptions};
use source_fs::games::{GameDescriptor, HL2DM, KNOWN_GAMES};
use source_fs::steam::SteamLibraries;

/// Environment variable that overrides the Half-Life 2: Deathmatch install
/// folder (a developer aid; players use the mounted-games screen).
pub const GAME_DIR_ENV: &str = "HL2DM_DIR";

/// Content settings shared by the client and the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentConfig {
    /// Per-game settings, keyed by [`GameDescriptor::id`]. A game that is
    /// absent is enabled and discovered through Steam.
    pub games: BTreeMap<String, GameConfig>,
    /// Extra Steam library folders to search, for installs that Steam does
    /// not list.
    pub library_paths: Vec<PathBuf>,
    /// Which workshop content to mount.
    pub workshop: WorkshopOptions,
}

/// Settings for one mountable game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GameConfig {
    /// Whether the game's content is mounted.
    pub enabled: bool,
    /// Explicit install folder (the one containing the game's mod directory,
    /// e.g. `hl2mp/`); discovered through Steam when absent.
    pub dir: Option<PathBuf>,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dir: None,
        }
    }
}

impl ContentConfig {
    /// The settings of `game`, which are the defaults when never configured.
    #[must_use]
    pub fn game(&self, game: &GameDescriptor) -> GameConfig {
        self.games.get(game.id).cloned().unwrap_or_default()
    }

    /// Mutable settings of `game`, created with the defaults on first use.
    pub fn game_mut(&mut self, game: &GameDescriptor) -> &mut GameConfig {
        self.games.entry(game.id.to_owned()).or_default()
    }

    /// The mount plan for every enabled game among `wanted` (all known games
    /// when `None`), in [`KNOWN_GAMES`] order.
    ///
    /// Steam is searched once for all of them, including the configured
    /// [`ContentConfig::library_paths`].
    #[must_use]
    pub fn plans(&self, wanted: Option<&[String]>) -> Vec<GamePlan> {
        let mut steam = SteamLibraries::discover();
        steam.add_libraries(self.library_paths.iter().cloned());
        KNOWN_GAMES
            .iter()
            .filter(|game| wanted.is_none_or(|ids| ids.iter().any(|id| id == game.id)))
            .filter_map(|game| {
                let settings = self.game(game);
                settings.enabled.then(|| GamePlan {
                    game: *game,
                    options: ContentOptions {
                        game_dir: settings.dir,
                        workshop: self.workshop.clone(),
                        steam: Some(steam.clone()),
                        ..ContentOptions::for_game(game)
                    },
                })
            })
            .collect()
    }
}

/// Command-line options shared by every rustlife binary that mounts content.
///
/// They are developer overrides: players and server admins configure content
/// through the game's menu and the config files instead.
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
    /// Loads the player's config file selected by `--config` (or the default
    /// one).
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

    /// The file the player's settings are saved to: `--config`, else the
    /// default location.
    #[must_use]
    pub fn config_path(&self) -> Option<PathBuf> {
        self.config.clone().or_else(UserConfig::default_path)
    }

    /// Applies these arguments and the environment on top of `config`, with
    /// the precedence *command line > environment > config file*.
    pub fn apply(&self, config: &mut ContentConfig) {
        self.apply_with_env(config, std::env::var_os(GAME_DIR_ENV).map(PathBuf::from));
    }

    /// [`ContentArgs::apply`] with the environment value passed explicitly,
    /// which keeps the precedence logic testable without mutating process
    /// state.
    pub fn apply_with_env(&self, config: &mut ContentConfig, env_dir: Option<PathBuf>) {
        let dir = self
            .game_dir
            .clone()
            .or_else(|| env_dir.filter(|dir| !dir.as_os_str().is_empty()));
        if let Some(dir) = dir {
            config.game_mut(&HL2DM).dir = Some(dir);
        }
        for app in &self.workshop_apps {
            if !config.workshop.extra_apps.contains(app) {
                config.workshop.extra_apps.push(*app);
            }
        }
        if self.no_workshop {
            config.workshop.enabled = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_cli_then_env_then_config() {
        let configured = || {
            let mut config = ContentConfig::default();
            config.game_mut(&HL2DM).dir = Some("/config".into());
            config
        };
        let cli = ContentArgs {
            game_dir: Some("/cli".into()),
            ..ContentArgs::default()
        };
        let none = ContentArgs::default();

        let dir = |args: &ContentArgs, env: Option<&str>| {
            let mut config = configured();
            args.apply_with_env(&mut config, env.map(PathBuf::from));
            config.game(&HL2DM).dir.unwrap()
        };
        assert_eq!(dir(&cli, Some("/env")), PathBuf::from("/cli"));
        assert_eq!(dir(&none, Some("/env")), PathBuf::from("/env"));
        assert_eq!(dir(&none, Some("")), PathBuf::from("/config"));
        assert_eq!(dir(&none, None), PathBuf::from("/config"));

        let mut empty = ContentConfig::default();
        none.apply_with_env(&mut empty, None);
        assert_eq!(empty, ContentConfig::default());
    }

    #[test]
    fn cli_workshop_flags_merge_with_config() {
        let mut config = ContentConfig {
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
        args.apply_with_env(&mut config, None);
        assert_eq!(config.workshop.extra_apps, [220, 4000]);

        let off = ContentArgs {
            no_workshop: true,
            ..ContentArgs::default()
        };
        off.apply_with_env(&mut config, None);
        assert!(!config.workshop.enabled);
    }

    #[test]
    fn plans_cover_enabled_games_only() {
        let mut config = ContentConfig::default();
        config.game_mut(&HL2DM).dir = Some("/games/hl2dm".into());
        let plans = config.plans(None);
        assert_eq!(plans.len(), KNOWN_GAMES.len());
        assert_eq!(plans[0].game, HL2DM);
        assert_eq!(
            plans[0].options.game_dir.as_deref(),
            Some(std::path::Path::new("/games/hl2dm"))
        );

        config.game_mut(&HL2DM).enabled = false;
        assert!(config.plans(None).is_empty());
        config.game_mut(&HL2DM).enabled = true;
        assert!(config.plans(Some(&["other".to_owned()])).is_empty());
        assert_eq!(config.plans(Some(&["hl2dm".to_owned()])).len(), 1);
    }
}
