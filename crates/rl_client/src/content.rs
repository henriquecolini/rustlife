//! The games the player has mounted.
//!
//! [`Content`] is the single place that knows which Source games are mounted
//! into the shared [`Vfs`], where they came from and why one failed. The menus
//! read it, change the player's [`UserConfig`] through it and ask it to mount
//! again; nothing else touches mounting.

use std::path::PathBuf;

use bevy::prelude::*;
use rl_config::UserConfig;
use source_fs::Vfs;
use source_fs::content::mount_games;
use source_fs::games::{GameDescriptor, KNOWN_GAMES};

/// Whether one known game is mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameStatus {
    /// The game.
    pub game: GameDescriptor,
    /// Whether the player enabled it.
    pub enabled: bool,
    /// Where it is mounted from, or why it could not be.
    pub state: Result<PathBuf, String>,
}

impl GameStatus {
    /// Whether the game's content is available.
    #[must_use]
    pub fn mounted(&self) -> bool {
        self.enabled && self.state.is_ok()
    }
}

/// The player's content configuration and what mounting it produced.
#[derive(Resource, Debug, Clone)]
pub struct Content {
    /// The settings being edited; [`Content::save`] persists them.
    pub config: UserConfig,
    /// Where [`Content::save`] writes, if the platform has a config directory.
    pub path: Option<PathBuf>,
    /// One entry per known game.
    pub games: Vec<GameStatus>,
}

impl Content {
    /// Mounts every enabled game of `config` into `vfs`, replacing what was
    /// mounted before. Games that cannot be found are reported, not fatal:
    /// the menus let the player locate them.
    #[must_use]
    pub fn mount(config: UserConfig, path: Option<PathBuf>, vfs: &Vfs) -> Self {
        let mut content = Self {
            config,
            path,
            games: Vec::new(),
        };
        content.remount(vfs);
        content
    }

    /// Mounts the games again after the configuration changed.
    pub fn remount(&mut self, vfs: &Vfs) {
        vfs.clear();
        let mounts = mount_games(vfs, &self.config.content.plans(None));
        self.games = KNOWN_GAMES
            .iter()
            .map(|game| {
                let mount = mounts.iter().find(|mount| mount.game.id == game.id);
                GameStatus {
                    game: *game,
                    enabled: mount.is_some(),
                    state: match mount {
                        Some(mount) => mount
                            .outcome
                            .as_ref()
                            .map(|info| info.game_dir.clone())
                            .map_err(ToString::to_string),
                        None => Err("disabled".to_owned()),
                    },
                }
            })
            .collect();
    }

    /// The ids of the games whose content is available, which is what a
    /// server compares against its own.
    #[must_use]
    pub fn mounted_ids(&self) -> Vec<String> {
        self.games
            .iter()
            .filter(|status| status.mounted())
            .map(|status| status.game.id.to_owned())
            .collect()
    }

    /// Whether any game is mounted, i.e. whether there is anything to play.
    #[must_use]
    pub fn any_mounted(&self) -> bool {
        self.games.iter().any(GameStatus::mounted)
    }

    /// Writes the configuration to disk.
    ///
    /// # Errors
    ///
    /// Fails when there is no config directory or the file cannot be written.
    pub fn save(&self) -> Result<(), String> {
        let path = self
            .path
            .as_ref()
            .ok_or("this platform has no configuration directory")?;
        self.config.save(path).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use source_fs::games::HL2DM;
    use source_fs::testing::{SAMPLE_GAMEINFO, SteamFixture};

    use super::*;

    fn installed_game() -> (tempfile::TempDir, UserConfig) {
        let steam = tempfile::tempdir().unwrap();
        let fixture = SteamFixture::new(steam.path());
        let game = fixture.install_app(steam.path(), 320, "Sample").unwrap();
        let mod_dir = game.join("hl2mp");
        std::fs::create_dir_all(&mod_dir).unwrap();
        std::fs::write(mod_dir.join("gameinfo.txt"), SAMPLE_GAMEINFO).unwrap();
        let mut config = UserConfig::default();
        config.content.game_mut(&HL2DM).dir = Some(game);
        (steam, config)
    }

    #[test]
    fn an_unfindable_game_is_reported_not_fatal() {
        let mut config = UserConfig::default();
        config.content.game_mut(&HL2DM).dir = Some("/definitely/not/here".into());
        let content = Content::mount(config, None, &Vfs::new());
        assert!(!content.any_mounted());
        assert!(content.mounted_ids().is_empty());
        assert!(content.games[0].state.is_err());
    }

    #[test]
    fn disabled_games_are_not_mounted() {
        let (_steam, mut config) = installed_game();
        config.content.game_mut(&HL2DM).enabled = false;
        let content = Content::mount(config, None, &Vfs::new());
        assert!(!content.any_mounted());
        assert!(!content.games[0].enabled);
    }

    #[test]
    fn remounting_follows_the_configuration() {
        let (_steam, config) = installed_game();
        let vfs = Vfs::new();
        let mut content = Content::mount(config, None, &vfs);
        assert_eq!(content.mounted_ids(), ["hl2dm"]);
        assert!(!vfs.mount_list().is_empty());

        content.config.content.game_mut(&HL2DM).enabled = false;
        content.remount(&vfs);
        assert!(content.mounted_ids().is_empty());
        assert!(vfs.mount_list().is_empty());
    }

    #[test]
    fn saving_needs_a_path() {
        let content = Content::mount(UserConfig::default(), None, &Vfs::new());
        assert!(content.save().is_err());
    }
}
