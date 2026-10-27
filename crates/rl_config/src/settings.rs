//! The player's settings: video, audio, controls and who they are.
//!
//! These are plain data with defaults and validation (`sanitized`), kept
//! free of any engine types so the config crate stays small: key bindings
//! are stored by name (`"KeyW"`, `"Mouse Left"`) and mapped to real inputs
//! by the client.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// All of the player's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Window, rendering quality and field of view.
    pub video: VideoSettings,
    /// Volumes.
    pub audio: AudioSettings,
    /// Mouse and key bindings.
    pub controls: ControlSettings,
    /// Who the player is.
    pub player: PlayerSettings,
}

impl Settings {
    /// These settings with every value brought into its valid range, as
    /// after hand-editing the file.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.video.fov = finite_or(self.video.fov, 90.0).clamp(60.0, 120.0);
        self.video.resolution = (
            self.video.resolution.0.clamp(640, 16384),
            self.video.resolution.1.clamp(480, 16384),
        );
        let volume = |value: f32| finite_or(value, 1.0).clamp(0.0, 1.0);
        self.audio.master = volume(self.audio.master);
        self.audio.effects = volume(self.audio.effects);
        self.audio.ambient = volume(self.audio.ambient);
        self.controls.sensitivity = finite_or(self.controls.sensitivity, 1.0).clamp(0.05, 10.0);
        self.player.name = crate::clean_player_name(&self.player.name);
        self
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// How the window is shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowMode {
    /// A normal window.
    #[default]
    Windowed,
    /// A borderless window covering the screen.
    Borderless,
    /// Exclusive fullscreen.
    Fullscreen,
}

/// A quality level for textures and shadows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Quality {
    /// The cheapest.
    Low,
    /// A balance for modest hardware.
    Medium,
    /// The default.
    #[default]
    High,
    /// The best looking.
    Ultra,
}

impl Quality {
    /// Every level, worst first.
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];

    /// The level's name for menus.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Ultra => "Ultra",
        }
    }
}

/// Multisample anti-aliasing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Antialiasing {
    /// None.
    Off,
    /// Two samples per pixel.
    X2,
    /// Four samples per pixel.
    #[default]
    X4,
}

impl Antialiasing {
    /// Every setting, cheapest first.
    pub const ALL: [Self; 3] = [Self::Off, Self::X2, Self::X4];

    /// The setting's name for menus.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::X2 => "2x",
            Self::X4 => "4x",
        }
    }
}

/// Window, rendering quality and field of view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoSettings {
    /// Windowed, borderless or fullscreen.
    pub window_mode: WindowMode,
    /// The window's size in pixels when windowed.
    pub resolution: (u32, u32),
    /// Wait for the display's refresh, avoiding tearing.
    pub vsync: bool,
    /// Horizontal field of view in degrees.
    pub fov: f32,
    /// Sharpness of textures; lower levels drop their largest mip levels.
    pub textures: Quality,
    /// Resolution and range of shadows.
    pub shadows: Quality,
    /// Smoothing of jagged edges.
    pub antialiasing: Antialiasing,
    /// Show the frame rate.
    pub show_fps: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            window_mode: WindowMode::Windowed,
            resolution: (1600, 900),
            vsync: true,
            fov: 90.0,
            textures: Quality::High,
            shadows: Quality::High,
            antialiasing: Antialiasing::X4,
            show_fps: false,
        }
    }
}

/// Volumes, each from 0 to 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSettings {
    /// Everything.
    pub master: f32,
    /// Weapons, footsteps, props and the like.
    pub effects: f32,
    /// The soundscape of the place the player is in.
    pub ambient: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master: 0.8,
            effects: 1.0,
            ambient: 1.0,
        }
    }
}

/// A rebindable game action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    /// Stable id, used in the config file.
    pub id: &'static str,
    /// Name for menus.
    pub label: &'static str,
    /// The input bound by default (see [`ControlSettings::bindings`]).
    pub default: &'static str,
}

/// Every rebindable action, in the order menus list them.
pub const ACTIONS: &[Action] = &[
    Action {
        id: "forward",
        label: "Move forward",
        default: "KeyW",
    },
    Action {
        id: "back",
        label: "Move back",
        default: "KeyS",
    },
    Action {
        id: "left",
        label: "Move left",
        default: "KeyA",
    },
    Action {
        id: "right",
        label: "Move right",
        default: "KeyD",
    },
    Action {
        id: "jump",
        label: "Jump",
        default: "Space",
    },
    Action {
        id: "duck",
        label: "Crouch",
        default: "ControlLeft",
    },
    Action {
        id: "sprint",
        label: "Sprint",
        default: "ShiftLeft",
    },
    Action {
        id: "walk",
        label: "Walk",
        default: "AltLeft",
    },
    Action {
        id: "use",
        label: "Use",
        default: "KeyE",
    },
    Action {
        id: "attack",
        label: "Fire",
        default: "Mouse Left",
    },
    Action {
        id: "attack2",
        label: "Alternate fire",
        default: "Mouse Right",
    },
    Action {
        id: "reload",
        label: "Reload",
        default: "KeyR",
    },
    Action {
        id: "last_weapon",
        label: "Last weapon",
        default: "KeyQ",
    },
    Action {
        id: "scoreboard",
        label: "Scoreboard",
        default: "Tab",
    },
];

/// Mouse and key bindings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ControlSettings {
    /// Mouse sensitivity as a multiple of the default.
    pub sensitivity: f32,
    /// Look up by pushing the mouse down.
    pub invert_y: bool,
    /// Inputs by action id (see [`ACTIONS`]): a key code name such as
    /// `"KeyW"`, or `"Mouse Left"` / `"Mouse Right"` / `"Mouse Middle"`.
    /// Actions left out use their default.
    pub bindings: BTreeMap<String, String>,
}

impl Default for ControlSettings {
    fn default() -> Self {
        Self {
            sensitivity: 1.0,
            invert_y: false,
            bindings: BTreeMap::new(),
        }
    }
}

impl ControlSettings {
    /// The input bound to action `id`: the player's choice, else the
    /// default. Empty for an unknown action.
    #[must_use]
    pub fn binding(&self, id: &str) -> &str {
        self.bindings
            .get(id)
            .map(String::as_str)
            .or_else(|| {
                ACTIONS
                    .iter()
                    .find(|action| action.id == id)
                    .map(|a| a.default)
            })
            .unwrap_or_default()
    }

    /// Binds `input` to action `id`. Another action using the same input
    /// gets this action's previous one, so no input is ever bound twice.
    pub fn rebind(&mut self, id: &str, input: &str) {
        let previous = self.binding(id).to_owned();
        for action in ACTIONS {
            if action.id != id && self.binding(action.id) == input {
                self.bindings.insert(action.id.to_owned(), previous.clone());
            }
        }
        self.bindings.insert(id.to_owned(), input.to_owned());
    }

    /// Puts every binding back to its default.
    pub fn reset_bindings(&mut self) {
        self.bindings.clear();
    }
}

/// Who the player is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerSettings {
    /// The name other players see.
    pub name: String,
    /// Servers joined before, most recent first, as `host:port`.
    pub recent_servers: Vec<String>,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            name: "player".to_owned(),
            recent_servers: Vec::new(),
        }
    }
}

impl PlayerSettings {
    /// Most servers remembered.
    pub const MAX_RECENT: usize = 8;

    /// Remembers a server as the most recent, once.
    pub fn remember_server(&mut self, address: &str) {
        self.recent_servers.retain(|known| known != address);
        self.recent_servers.insert(0, address.to_owned());
        self.recent_servers.truncate(Self::MAX_RECENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_default_and_rebind_without_duplicates() {
        let mut controls = ControlSettings::default();
        assert_eq!(controls.binding("forward"), "KeyW");
        assert_eq!(controls.binding("nonsense"), "");

        controls.rebind("jump", "KeyW");
        assert_eq!(controls.binding("jump"), "KeyW");
        assert_eq!(
            controls.binding("forward"),
            "Space",
            "forward gets jump's old key"
        );
        let mut inputs: Vec<&str> = ACTIONS.iter().map(|a| controls.binding(a.id)).collect();
        inputs.sort_unstable();
        inputs.dedup();
        assert_eq!(inputs.len(), ACTIONS.len());

        controls.reset_bindings();
        assert_eq!(controls.binding("jump"), "Space");
    }

    #[test]
    fn default_bindings_are_distinct() {
        let mut inputs: Vec<&str> = ACTIONS.iter().map(|action| action.default).collect();
        inputs.sort_unstable();
        inputs.dedup();
        assert_eq!(inputs.len(), ACTIONS.len());
    }

    #[test]
    fn hand_edited_values_are_brought_into_range() {
        let mut settings = Settings::default();
        settings.video.fov = 500.0;
        settings.video.resolution = (1, 99999);
        settings.audio.master = f32::NAN;
        settings.audio.effects = -2.0;
        settings.controls.sensitivity = 0.0;
        settings.player.name = "  \n ".to_owned();
        let settings = settings.sanitized();
        assert_eq!(settings.video.fov, 120.0);
        assert_eq!(settings.video.resolution, (640, 16384));
        assert_eq!(settings.audio.master, 1.0);
        assert_eq!(settings.audio.effects, 0.0);
        assert_eq!(settings.controls.sensitivity, 0.05);
        assert_eq!(settings.player.name, "player");
    }

    #[test]
    fn recent_servers_are_unique_most_recent_first_and_capped() {
        let mut player = PlayerSettings::default();
        for index in 0..12 {
            player.remember_server(&format!("10.0.0.{index}:27015"));
        }
        player.remember_server("10.0.0.5:27015");
        assert_eq!(player.recent_servers.len(), PlayerSettings::MAX_RECENT);
        assert_eq!(player.recent_servers[0], "10.0.0.5:27015");
        assert_eq!(
            player
                .recent_servers
                .iter()
                .filter(|server| *server == "10.0.0.5:27015")
                .count(),
            1
        );
    }

    #[test]
    fn partial_files_use_defaults_and_round_trip() {
        let settings: Settings =
            ron::from_str(r#"(video: (fov: 100.0), controls: (bindings: {"jump": "KeyX"}))"#)
                .unwrap();
        assert_eq!(settings.video.fov, 100.0);
        assert_eq!(settings.video.textures, Quality::High);
        assert_eq!(settings.controls.binding("jump"), "KeyX");
        let text =
            ron::ser::to_string_pretty(&settings, ron::ser::PrettyConfig::default()).unwrap();
        assert_eq!(ron::from_str::<Settings>(&text).unwrap(), settings);
    }
}
