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
use rl_sim::movement::PlayerState;
use rl_sim::simulation::PlayerId;

use super::SoundPlayer;
use crate::AppState;
use crate::net::{Drawn, Game};
use crate::player::{LocalPlayer, Ticked};

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
    let walkers: Vec<(PlayerId, PlayerState, Option<Vec3>)> = sim
        .players()
        .filter(|(_, other)| other.alive())
        .map(|(id, other)| {
            if Some(id) == local_id
                && let Some(local) = &local
            {
                return (id, local.current, None);
            }
            let feet = drawn
                .0
                .as_ref()
                .and_then(|world| world.players.get(&id))
                .map_or(other.state.origin, |pose| pose.feet);
            (id, other.state, Some(feet))
        })
        .collect();
    gait.players
        .retain(|id, _| walkers.iter().any(|(walker, ..)| walker == id));
    for (id, state, at) in walkers {
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
        let name = if stride.left {
            &config.left
        } else {
            &config.right
        };
        stride.left = !stride.left;
        player.play(name, at, volume);
    }
}

/// A step's sound when the local player lands (others' landings are not
/// told to clients).
fn landings(
    mut ticks: MessageReader<Ticked>,
    game: Game,
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
            player.play(&config.left, None, config.run_volume);
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
