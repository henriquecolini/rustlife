//! The player's settings file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{ConfigError, ContentConfig, config_dir, read_ron, to_ron, write_text};

/// The player's complete settings file (`config.ron`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
    /// Where game content comes from.
    pub content: ContentConfig,
}

impl UserConfig {
    /// Default location of the config file, if the platform has a config
    /// directory.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|dir| dir.join("config.ron"))
    }

    /// Loads the config from `path`. A missing file yields the defaults.
    ///
    /// # Errors
    ///
    /// Fails if the file exists but is unreadable or invalid.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Ok(read_ron(path)?.unwrap_or_default())
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

    /// Writes the config to `path`, creating its directory.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        write_text(path, &to_ron(self)?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use source_fs::games::HL2DM;

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
            r#"(content: (
                games: {"hl2dm": (dir: Some("/games/hl2dm"))},
                library_paths: ["/mnt/lib"],
                workshop: (extra_apps: [220]),
            ))"#,
        )
        .unwrap();
        let hl2dm = config.content.game(&HL2DM);
        assert_eq!(hl2dm.dir.as_deref(), Some(Path::new("/games/hl2dm")));
        assert!(hl2dm.enabled, "unspecified fields keep defaults");
        assert_eq!(config.content.library_paths, [PathBuf::from("/mnt/lib")]);
        assert_eq!(config.content.workshop.extra_apps, [220]);
        assert!(config.content.workshop.enabled);
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
    fn saved_configs_load_back_and_replace_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/config.ron");
        let mut config = UserConfig::default();
        config.content.game_mut(&HL2DM).dir = Some("/games/hl2dm".into());
        config.content.library_paths.push("/mnt/lib".into());

        config.save(&path).unwrap();
        assert_eq!(UserConfig::load(&path).unwrap(), config);

        config.content.game_mut(&HL2DM).enabled = false;
        config.save(&path).unwrap();
        assert_eq!(UserConfig::load(&path).unwrap(), config);
        assert!(!path.with_extension("ron.tmp").exists());
    }

    #[test]
    fn default_config_round_trips_through_ron() {
        let text = to_ron(&UserConfig::default()).unwrap();
        assert_eq!(UserConfig::parse(&text).unwrap(), UserConfig::default());
    }
}
