//! rustlife's own gameplay data: schema, validation and loading.
//!
//! Everything tunable about how the game plays lives in RON files under
//! `data/` rather than in code. This crate defines the schema of each file
//! (with `deny_unknown_fields`, so a typo is an error rather than a silently
//! ignored key), validates the values, and loads them.
//!
//! The repository's `data/` directory is compiled into the crate, so the game
//! always has a complete, valid set ([`GameData::builtin`]). A data directory
//! given at run time ([`GameData::load`]) overrides it file by file: any file
//! it contains replaces the built-in one, and missing files fall back to the
//! built-in copies.
//!
//! As of M3 the data set is:
//!
//! | File | Type | Contents |
//! |------|------|----------|
//! | `movement.ron` | [`MovementConfig`] | Player movement physics. |
//! | `simulation.ron` | [`SimulationConfig`] | Tick rate. |

#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

pub mod movement;
pub mod simulation;
mod validate;

pub use movement::{
    AirConfig, DuckConfig, GroundConfig, HullConfig, HullsConfig, JumpConfig, LadderConfig,
    MovementConfig, WaterConfig,
};
pub use simulation::SimulationConfig;
pub use validate::Problem;

/// File name of the movement data.
pub const MOVEMENT_FILE: &str = "movement.ron";
/// File name of the simulation timing data.
pub const SIMULATION_FILE: &str = "simulation.ron";

/// The built-in copy of each data file, from the repository's `data/`.
const BUILTIN: [(&str, &str); 2] = [
    (MOVEMENT_FILE, include_str!("../../../data/movement.ron")),
    (
        SIMULATION_FILE,
        include_str!("../../../data/simulation.ron"),
    ),
];

/// Errors from loading gameplay data. Each names the file it concerns.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    /// The file exists but could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The data file.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid RON or does not match the schema.
    #[error("{file}: {source}")]
    Parse {
        /// Where the file came from (a path, or `builtin:<name>`).
        file: String,
        /// Parser error with line and column; boxed because it is large.
        #[source]
        source: Box<ron::error::SpannedError>,
    },
    /// The file parsed, but some values make no sense.
    #[error("{file}: {}", problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid {
        /// Where the file came from (a path, or `builtin:<name>`).
        file: String,
        /// Every problem found.
        problems: Vec<Problem>,
    },
}

/// The complete, validated gameplay data set.
#[derive(Debug, Clone, PartialEq)]
pub struct GameData {
    /// Player movement.
    pub movement: MovementConfig,
    /// Simulation timing.
    pub simulation: SimulationConfig,
}

impl GameData {
    /// The data compiled into the game.
    ///
    /// # Errors
    ///
    /// Only if the repository's own data is broken, which the crate's tests
    /// rule out.
    pub fn builtin() -> Result<Self, DataError> {
        Self::load(None)
    }

    /// Loads the data, taking each file from `dir` when it exists there and
    /// from the built-in set otherwise.
    ///
    /// # Errors
    ///
    /// Fails on the first file that cannot be read, parsed or validated.
    pub fn load(dir: Option<&Path>) -> Result<Self, DataError> {
        Ok(Self {
            movement: load_file(dir, MOVEMENT_FILE, MovementConfig::validate)?,
            simulation: load_file(dir, SIMULATION_FILE, SimulationConfig::validate)?,
        })
    }
}

/// Loads, parses and validates one data file.
fn load_file<T: DeserializeOwned>(
    dir: Option<&Path>,
    name: &str,
    validate: fn(&T, &mut validate::Validator),
) -> Result<T, DataError> {
    let (file, text) = match dir.map(|dir| dir.join(name)) {
        Some(path) if path.exists() => {
            let text = std::fs::read_to_string(&path).map_err(|source| DataError::Io {
                path: path.clone(),
                source,
            })?;
            (path.display().to_string(), text)
        }
        _ => (format!("builtin:{name}"), builtin_text(name).to_owned()),
    };
    parse(&file, &text, validate)
}

/// Parses and validates `text`, attributing errors to `file`.
fn parse<T: DeserializeOwned>(
    file: &str,
    text: &str,
    validate: fn(&T, &mut validate::Validator),
) -> Result<T, DataError> {
    let value: T = ron::from_str(text).map_err(|source| DataError::Parse {
        file: file.to_owned(),
        source: Box::new(source),
    })?;
    let mut check = validate::Validator::default();
    validate(&value, &mut check);
    let problems = check.finish();
    if problems.is_empty() {
        Ok(value)
    } else {
        Err(DataError::Invalid {
            file: file.to_owned(),
            problems,
        })
    }
}

/// The built-in text of data file `name` (empty for an unknown name, which
/// then fails to parse with a clear error).
fn builtin_text(name: &str) -> &'static str {
    BUILTIN
        .iter()
        .find(|(file, _)| *file == name)
        .map_or("", |(_, text)| text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_data_is_valid() {
        let data = GameData::builtin().expect("the repository's data/ must be valid");
        assert_eq!(data.simulation.tick_rate, 66);
        assert!(data.movement.jump.launch_speed(data.movement.gravity) > 0.0);
    }

    #[test]
    fn a_data_directory_overrides_files_it_contains() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(SIMULATION_FILE),
            "Simulation(tick_rate: 100)",
        )
        .expect("write");
        let data = GameData::load(Some(dir.path())).expect("load");
        assert_eq!(data.simulation.tick_rate, 100);
        assert_eq!(
            data.movement,
            GameData::builtin().expect("builtin").movement
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error = parse::<SimulationConfig>(
            "test",
            "Simulation(tick_rate: 66, tickrate: 1)",
            SimulationConfig::validate,
        )
        .expect_err("typo");
        assert!(matches!(error, DataError::Parse { .. }), "{error}");
    }

    #[test]
    fn invalid_values_report_every_problem() {
        let mut movement = GameData::builtin().expect("builtin").movement;
        movement.gravity = -1.0;
        movement.hulls.ducked.maxs[2] = 100.0;
        movement.hulls.standing.mins[2] = 1.0;
        let text = ron::to_string(&movement).expect("serialize");
        let Err(DataError::Invalid { problems, .. }) =
            parse::<MovementConfig>("test", &text, MovementConfig::validate)
        else {
            panic!("invalid movement data accepted");
        };
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.contains(&"gravity"), "{fields:?}");
        assert!(fields.contains(&"hulls.standing.mins"), "{fields:?}");
        assert!(fields.contains(&"hulls.ducked.maxs"), "{fields:?}");
    }

    #[test]
    fn tick_interval_is_the_reciprocal_rate() {
        let simulation = SimulationConfig { tick_rate: 50 };
        assert!((simulation.tick_interval() - 0.02).abs() < 1e-7);
        let mut check = validate::Validator::default();
        SimulationConfig { tick_rate: 0 }.validate(&mut check);
        assert_eq!(check.finish().len(), 1);
    }
}
