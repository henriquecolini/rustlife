//! Sounds the map keeps playing: its `ambient_generic`s as its logic
//! switches them, doors' sounds while they move, and wall chargers while
//! someone draws from them.
//!
//! Each is an emitter the system owns and despawns to stop:
//!
//! - **Ambient sounds** follow the replicated state of the map's logic.
//!   One that loops plays while its state says it plays; one that does not
//!   plays once each time it is started (and not when a player joins long
//!   after). A sound heard everywhere plays at the listener; the rest fade
//!   out towards the entity's radius (or as their soundscript says).
//! - **Doors and other movers** play their move sound (`noise1`, or a
//!   `func_movelinear`'s `StartSound`) from start to stop, carried with
//!   them, then their stop sound (`noise2`, `StopSound`).
//! - **Stations** play their item's `use` sound for as long as someone
//!   draws from them.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::simulation::{MapEvent, PlayerId};

use super::events::entity_of;
use super::{Repeat, SoundCategory, SoundPlayer};
use crate::AppState;
use crate::net::{Game, LevelEntities};
use crate::player::Ticked;

/// How long after its start, seconds, a one-shot ambient sound is still
/// worth playing to someone who just heard of it.
const LATE_SECS: f32 = 1.0;
/// Seconds a station's sound keeps playing after the last drawing heard.
const CHARGE_LINGER: f32 = 0.2;
/// The share of an ambient sound's radius at which it is still at full
/// volume: rodio's falloff then brings it to about a twentieth at the
/// radius.
const AMBIENT_FULL_SHARE: f32 = 1.0 / 4.5;

/// Plays the map's ongoing sounds.
pub(super) struct WorldSoundsPlugin;

impl Plugin for WorldSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Playing>()
            .add_systems(OnEnter(AppState::InGame), forget)
            .add_systems(OnExit(AppState::InGame), forget)
            .add_systems(
                Update,
                (ambient_sounds, mover_sounds, follow_movers, station_sounds)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The emitters this module owns.
#[derive(Resource, Default)]
struct Playing {
    /// Each ambient sound's emitter and the start it plays.
    ambient: Vec<Option<(Entity, u64)>>,
    /// Moving movers' sounds, by BSP model.
    movers: HashMap<u32, Entity>,
    /// Stations' sounds by (player, pickup), and when last heard.
    stations: HashMap<(PlayerId, usize), (Entity, f32)>,
}

/// A sound carried with the mover of a BSP model.
#[derive(Component)]
struct OnMover(u32);

/// Forgets the emitters of the map left (they are despawned with it).
fn forget(mut playing: ResMut<Playing>) {
    *playing = Playing::default();
}

/// Starts and stops the map's ambient sounds as its logic says.
fn ambient_sounds(
    mut commands: Commands,
    game: Game,
    mut playing: ResMut<Playing>,
    mut player: SoundPlayer,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let sounds = sim.logic().sounds();
    if playing.ambient.len() != sounds.len() {
        playing.ambient.resize(sounds.len(), None);
    }
    let now = sim.tick_count();
    let late = (LATE_SECS / sim.dt()) as u64;
    for (index, sound) in sounds.iter().enumerate() {
        let wanted = sound.state.started;
        let current = playing.ambient[index];
        if current.map(|(_, started)| started) == wanted && current.is_some() == wanted.is_some() {
            continue;
        }
        if let Some((entity, _)) = current {
            commands.entity(entity).try_despawn();
        }
        playing.ambient[index] = None;
        let Some(started) = wanted else {
            continue;
        };
        // A one-shot started long ago (before this client heard of it) is
        // over.
        if !sound.looped && now.saturating_sub(started) > late {
            playing.ambient[index] = Some((Entity::PLACEHOLDER, started));
            continue;
        }
        let Some(chosen) = player.choose(&sound.sound) else {
            playing.ambient[index] = Some((Entity::PLACEHOLDER, started));
            continue;
        };
        let at = (!sound.everywhere).then_some(sound.position);
        // A soundscript gives its own level; a bare file fades by the
        // entity's radius.
        let spatial = if player.is_scripted(&sound.sound) {
            super::spatial_scale(chosen.soundlevel)
        } else {
            let full_volume_metres = (sound.radius * AMBIENT_FULL_SHARE).max(1.0) * 0.0254;
            Some(1.0 / full_volume_metres)
        };
        let repeat = if sound.looped {
            Repeat::AsRecorded
        } else {
            Repeat::Once
        };
        let mut chosen = chosen;
        chosen.speed *= sound.state.pitch;
        let entity = player.spawn_chosen(&chosen, at, sound.state.volume, spatial, repeat);
        if let Some(entity) = entity {
            player.categorize(entity, SoundCategory::Ambient);
        }
        playing.ambient[index] = Some((entity.unwrap_or(Entity::PLACEHOLDER), started));
    }
}

/// Starts and stops movers' sounds as they start and stop moving.
fn mover_sounds(
    mut commands: Commands,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    entities: Res<LevelEntities>,
    mut playing: ResMut<Playing>,
    mut player: SoundPlayer,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    for Ticked(events) in ticks.read() {
        for event in &events.map {
            let (model, started) = match *event {
                MapEvent::MoverStarted { model } => (model, true),
                MapEvent::MoverStopped { model } => (model, false),
                _ => continue,
            };
            let Some(entity) = entity_of(&entities.0, model) else {
                continue;
            };
            let center = sim
                .world()
                .mover_bounds(model as usize)
                .map(|(mins, maxs)| (mins + maxs) * 0.5);
            let key = |keys: [&str; 2]| {
                keys.iter()
                    .find_map(|key| entity.property(key))
                    .filter(|name| !name.is_empty() && *name != "0")
                    .map(str::to_owned)
            };
            if let Some(old) = playing.movers.remove(&model) {
                commands.entity(old).try_despawn();
            }
            if started {
                if let Some(name) = key(["noise1", "StartSound"])
                    && let Some(emitter) = player.spawn(&name, center, 1.0, Repeat::AsRecorded)
                {
                    commands.entity(emitter).insert(OnMover(model));
                    playing.movers.insert(model, emitter);
                }
            } else if let Some(name) = key(["noise2", "StopSound"]) {
                player.play(&name, center, 1.0);
            }
        }
    }
}

/// Carries movers' sounds with them.
fn follow_movers(game: Game, mut emitters: Query<(&OnMover, &mut Transform)>) {
    let Some(sim) = game.sim() else {
        return;
    };
    for (on, mut transform) in &mut emitters {
        if let Some((mins, maxs)) = sim.world().mover_bounds(on.0 as usize) {
            transform.translation = coords::position(((mins + maxs) * 0.5).to_array());
        }
    }
}

/// Plays stations' sounds while players draw from them.
fn station_sounds(
    mut commands: Commands,
    time: Res<Time>,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    mut playing: ResMut<Playing>,
    mut player: SoundPlayer,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    let now = time.elapsed_secs();
    for Ticked(events) in ticks.read() {
        for &(who, index) in &events.charging {
            if let Some((_, seen)) = playing.stations.get_mut(&(who, index)) {
                *seen = now;
                continue;
            }
            let Some(pickup) = sim.pickups().get(index) else {
                continue;
            };
            let name = sim
                .data()
                .items
                .get(&pickup.id)
                .and_then(|item| item.sounds.get("use"))
                .cloned();
            if let Some(name) = name
                && let Some(emitter) = player.spawn(&name, Some(pickup.origin), 1.0, Repeat::Loop)
            {
                playing.stations.insert((who, index), (emitter, now));
            }
        }
    }
    playing.stations.retain(|_, &mut (emitter, seen)| {
        let lingering = now - seen <= CHARGE_LINGER;
        if !lingering {
            commands.entity(emitter).try_despawn();
        }
        lingering
    });
}
