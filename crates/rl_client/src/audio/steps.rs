//! Footsteps: every player on the ground and moving steps every so often,
//! left and right in turn, quieter when walking, silent when creeping
//! (`sounds.ron`'s `footsteps`). A landing sounds a step at once.
//!
//! The local player steps by their predicted movement and is heard at the
//! listener; everyone else by the replica's view of them, where they are
//! drawn.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_data::FootstepsConfig;
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::collision::{CollisionQuery, Hull};
use rl_sim::movement::PlayerState;
use rl_sim::scene::Scene;
use rl_sim::simulation::{PlayerId, Simulation};

use super::SoundPlayer;
use super::props::Surfaces;
use crate::AppState;
use crate::net::{Drawn, Game};
use crate::player::{LocalPlayer, Ticked};
use crate::surfaces::MapSurfaces;

/// Plays footsteps.
pub(super) struct StepsPlugin;

impl Plugin for StepsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Gait>()
            .add_systems(OnEnter(AppState::InGame), |mut gait: ResMut<Gait>| {
                *gait = Gait::default();
            })
            .add_systems(
                Update,
                (footsteps, landings).run_if(in_state(AppState::InGame)),
            );
    }
}

/// Each player's step clock and which foot is next.
#[derive(Resource, Default)]
struct Gait {
    players: HashMap<PlayerId, Stride>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Stride {
    /// Seconds until the next step.
    until: f32,
    /// Whether the next step is the left foot.
    left: bool,
}

/// How loud a step is for a player moving at `speed` on the ground, and the
/// time to the next one; `None` while too slow to be heard.
fn step(config: &FootstepsConfig, speed: f32, ducked: bool) -> Option<(f32, f32)> {
    if speed < config.run_speed * 0.25 {
        return None;
    }
    if ducked || speed < config.run_speed {
        Some((config.walk_volume, config.walk_interval))
    } else {
        Some((config.run_volume, config.run_interval))
    }
}

/// Steps for everyone on the ground.
fn footsteps(
    time: Res<Time>,
    game: Game,
    drawn: Res<Drawn>,
    surfaces: Res<Surfaces>,
    map_surfaces: Res<MapSurfaces>,
    local: Option<Res<LocalPlayer>>,
    mut gait: ResMut<Gait>,
    mut player: SoundPlayer,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let config = &sim.data().sounds.footsteps;
    let dt = time.delta_secs();
    let local_id = local.as_ref().map(|local| local.id);
    let walkers: Vec<(PlayerId, PlayerState, Vec3, Option<Vec3>)> = sim
        .players()
        .filter(|(_, other)| other.alive())
        .map(|(id, other)| {
            if Some(id) == local_id
                && let Some(local) = &local
            {
                return (id, local.current, local.current.origin, None);
            }
            let feet = drawn
                .0
                .as_ref()
                .and_then(|world| world.players.get(&id))
                .map_or(other.state.origin, |pose| pose.feet);
            (id, other.state, feet, Some(feet))
        })
        .collect();
    gait.players
        .retain(|id, _| walkers.iter().any(|(walker, ..)| walker == id));
    for (id, state, feet, at) in walkers {
        let stride = gait.players.entry(id).or_default();
        let moving = state
            .on_ground
            .then(|| step(config, state.speed_2d(), state.ducked))
            .flatten();
        let Some((volume, interval)) = moving else {
            stride.until = 0.0;
            continue;
        };
        stride.until -= dt;
        if stride.until > 0.0 {
            continue;
        }
        stride.until = interval;
        let surface = ground_surface(sim, &map_surfaces, feet);
        let sounds = surfaces.0.sounds(&surface);
        let foot = if stride.left {
            &sounds.step_left
        } else {
            &sounds.step_right
        };
        let name = if foot.is_empty() {
            if stride.left {
                &config.left
            } else {
                &config.right
            }
        } else {
            foot
        };
        stride.left = !stride.left;
        player.play(name, at, volume);
    }
}

/// The surface property of the floor under `feet`: the prop stood on, else
/// the map material, else the game's default.
fn ground_surface(sim: &Simulation, surfaces: &MapSurfaces, feet: Vec3) -> String {
    let scene = Scene {
        world: sim.world(),
        physics: sim.physics(),
        ignore: None,
    };
    let trace = scene.trace_hull(
        feet + Vec3::Z * 2.0,
        feet - Vec3::Z * 4.0,
        &Hull::POINT,
        MASK_PLAYER_SOLID,
    );
    if !trace.hit() {
        return "default".to_owned();
    }
    if let Some(body) = trace.body {
        return sim.physics().surface(body).to_owned();
    }
    surfaces
        .get(trace.material)
        .map_or("default", |info| info.prop.as_str())
        .to_owned()
}

/// A step's sound when the local player lands (others' landings are not
/// told to clients).
fn landings(
    mut ticks: MessageReader<Ticked>,
    game: Game,
    surfaces: Res<Surfaces>,
    map_surfaces: Res<MapSurfaces>,
    local: Option<Res<LocalPlayer>>,
    mut player: SoundPlayer,
) {
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        ticks.clear();
        return;
    };
    let config = &sim.data().sounds.footsteps;
    for Ticked(events) in ticks.read() {
        let landed = events
            .moves
            .iter()
            .any(|(id, moved)| *id == local.id && moved.landed.is_some());
        if landed {
            let surface = ground_surface(sim, &map_surfaces, local.current.origin);
            let sound = surfaces.0.sounds(&surface);
            let name = if sound.step_left.is_empty() {
                &config.left
            } else {
                &sound.step_left
            };
            player.play(name, None, config.run_volume);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_quieten_when_walking_and_stop_when_creeping() {
        let config = FootstepsConfig {
            left: "L".to_owned(),
            right: "R".to_owned(),
            run_interval: 0.3,
            walk_interval: 0.4,
            run_speed: 150.0,
            run_volume: 0.5,
            walk_volume: 0.2,
        };
        assert_eq!(step(&config, 250.0, false), Some((0.5, 0.3)));
        assert_eq!(step(&config, 100.0, false), Some((0.2, 0.4)));
        assert_eq!(step(&config, 250.0, true), Some((0.2, 0.4)), "crouched");
        assert_eq!(step(&config, 20.0, false), None);
    }
}
