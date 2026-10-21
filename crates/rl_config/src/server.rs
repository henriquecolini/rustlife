//! The dedicated server's single config file.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use rl_data::GameData;
use serde::{Deserialize, Serialize};

use crate::{ConfigError, ContentConfig, config_dir, read_ron, to_ron, write_text};

/// The text written when no `server.ron` exists yet. It is the defaults, with
/// comments; a test keeps it in step with [`ServerConfig::default`].
const TEMPLATE: &str = r#"// rustlife dedicated server configuration. Every field is optional.
(
    // Name shown to players.
    name: "rustlife server",
    // Games whose content players must have to join. The server mounts them.
    games: ["hl2dm"],
    // Maps that can be played and voted for, without path or extension.
    // Empty means every map found in the mounted games.
    maps: [],
    // UDP port and address to listen on.
    port: 27015,
    bind: "0.0.0.0",
    // Most players at once.
    max_players: 16,
    // Players needed before a match starts; until then the server waits.
    min_players: 2,
    // Countdown before a match, once there are enough players, in seconds.
    warmup_seconds: 10.0,
    // How long a match lasts, in seconds.
    match_seconds: 600.0,
    // How long the scoreboard and the vote for the next map stay up after a
    // match, in seconds.
    vote_seconds: 20.0,
    // How long a dead player waits before respawning, in seconds.
    respawn_seconds: 1.5,
    // Where the games are installed (see the game's own config for the
    // available options; usually found automatically).
    content: (),
)
"#;

/// The dedicated server's configuration (`server.ron`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Name shown to players.
    pub name: String,
    /// Ids of the games whose content players must share with the server.
    pub games: Vec<String>,
    /// Maps in the rotation and the vote, without path or extension; empty
    /// means every map the mounted games provide.
    pub maps: Vec<String>,
    /// UDP port to listen on.
    pub port: u16,
    /// Address to listen on.
    pub bind: Ipv4Addr,
    /// Most players at once.
    pub max_players: u32,
    /// Players needed before a match starts.
    pub min_players: u32,
    /// Countdown before a match, once there are enough players, in seconds.
    pub warmup_seconds: f32,
    /// How long a match lasts, in seconds.
    pub match_seconds: f32,
    /// How long the scoreboard and the vote for the next map stay up after a
    /// match, in seconds.
    pub vote_seconds: f32,
    /// How long a dead player waits before respawning, in seconds.
    pub respawn_seconds: f32,
    /// Where the games are installed.
    pub content: ContentConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "rustlife server".to_owned(),
            games: vec!["hl2dm".to_owned()],
            maps: Vec::new(),
            port: 27015,
            bind: Ipv4Addr::UNSPECIFIED,
            max_players: 16,
            min_players: 2,
            warmup_seconds: 10.0,
            match_seconds: 600.0,
            vote_seconds: 20.0,
            respawn_seconds: 1.5,
            content: ContentConfig::default(),
        }
    }
}

impl ServerConfig {
    /// Default location of `server.ron`.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join("server.ron"))
    }

    /// Parses config text (the template is valid input).
    ///
    /// # Errors
    ///
    /// Returns the RON error, with position, on invalid input.
    pub fn parse(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::from_str(text)
    }

    /// Loads the config at `path`. When the file does not exist yet, a
    /// commented default is written there for the admin to edit, and the
    /// defaults are returned.
    ///
    /// # Errors
    ///
    /// Fails if the file is unreadable or invalid, or the default cannot be
    /// written.
    pub fn load_or_create(path: &Path) -> Result<Self, ConfigError> {
        if let Some(config) = read_ron(path)? {
            return Ok(config);
        }
        write_text(path, TEMPLATE)?;
        Ok(Self::default())
    }

    /// Writes the config to `path`, creating its directory.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        write_text(path, &to_ron(self)?)
    }

    /// Applies the operational settings to `data`, the gameplay data the
    /// server runs and sends to clients, so that what clients receive is what
    /// is enforced.
    pub fn apply_to(&self, data: &mut GameData) {
        data.net.max_players = self.max_players;
        data.rules.min_players = self.min_players;
        data.rules.warmup = self.warmup_seconds;
        data.rules.time_limit = self.match_seconds;
        data.rules.intermission = self.vote_seconds;
        data.rules.respawn.delay = self.respawn_seconds;
        // Players are respawned for them no sooner than they may ask.
        data.rules.respawn.force = data.rules.respawn.force.max(self.respawn_seconds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_the_default_config() {
        assert_eq!(
            ServerConfig::parse(TEMPLATE).unwrap(),
            ServerConfig::default()
        );
    }

    #[test]
    fn a_missing_file_is_created_from_the_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg/server.ron");
        let config = ServerConfig::load_or_create(&path).unwrap();
        assert_eq!(config, ServerConfig::default());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);
        assert_eq!(ServerConfig::load_or_create(&path).unwrap(), config);
    }

    #[test]
    fn edited_files_are_honoured_and_typos_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.ron");
        std::fs::write(&path, r#"(max_players: 4, maps: ["dm_lockdown"])"#).unwrap();
        let config = ServerConfig::load_or_create(&path).unwrap();
        assert_eq!(config.max_players, 4);
        assert_eq!(config.maps, ["dm_lockdown"]);
        assert_eq!(config.port, 27015);

        std::fs::write(&path, "(maxplayers: 4)").unwrap();
        let error = ServerConfig::load_or_create(&path).unwrap_err();
        assert!(error.to_string().contains("maxplayers"), "{error}");
    }

    #[test]
    fn operational_settings_override_the_gameplay_data() {
        let mut data = GameData::builtin().unwrap();
        let config = ServerConfig {
            max_players: 3,
            min_players: 3,
            warmup_seconds: 7.0,
            match_seconds: 90.0,
            vote_seconds: 12.0,
            respawn_seconds: 4.0,
            ..ServerConfig::default()
        };
        config.apply_to(&mut data);
        assert_eq!(data.net.max_players, 3);
        assert!((data.rules.time_limit - 90.0).abs() < f32::EPSILON);
        assert!((data.rules.respawn.delay - 4.0).abs() < f32::EPSILON);
        assert_eq!(data.rules.min_players, 3);
        assert!((data.rules.warmup - 7.0).abs() < f32::EPSILON);
        assert!((data.rules.intermission - 12.0).abs() < f32::EPSILON);
    }
}
