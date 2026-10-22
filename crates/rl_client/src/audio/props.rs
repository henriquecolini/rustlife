//! Props knocking about: the sounds of impacts and scraping.
//!
//! What a prop sounds like is its surface's: the mounted game's
//! `surfaceproperties.txt` names, per surface, the sounds of a hard and a
//! soft impact and of scraping rough or smooth. The server reports props
//! striking props or the map (`prop_impacts`) and, every few ticks, those
//! that are sliding (`prop_scrapes`); this turns them into sounds:
//!
//! - an **impact** plays the struck prop's surface's hard or soft sound
//!   (a thrown grenade's surface is its own: only the server has its body),
//!   louder for faster and heavier blows, at most once in a short while per
//!   prop so a tumbling crate does not machine-gun;
//! - a **scrape** keeps a looping emitter on the sliding prop, its volume
//!   following its speed, until the reports stop.

use std::collections::HashMap;

use bevy::audio::{AudioSink, SpatialAudioSink, Volume};
use bevy::prelude::*;
use rl_sim::physics::BodyId;
use source_formats::surfaceprops::SurfaceTable;

use super::{Emitter, Repeat, SoundCategory, SoundPlayer};
use crate::net::Game;
use crate::player::Ticked;
use crate::settings::Settings;
use crate::{AppState, GameVfs};

pub(super) struct PropSoundsPlugin;

impl Plugin for PropSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Surfaces>()
            .init_resource::<Scrapes>()
            .add_systems(OnEnter(AppState::InGame), load_surfaces)
            .add_systems(
                Update,
                (hear_props, stop_scrapes)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The game's surfaces and what they sound like.
#[derive(Resource, Default)]
pub(super) struct Surfaces(pub(super) SurfaceTable);

/// The sliding props that have a scraping emitter, and when each was last
/// reported.
#[derive(Resource, Default)]
struct Scrapes {
    emitters: HashMap<BodyId, (Entity, f32)>,
    /// When each prop last made an impact sound.
    last_impact: HashMap<BodyId, f32>,
}

fn load_surfaces(vfs: Res<GameVfs>, mut surfaces: ResMut<Surfaces>) {
    surfaces.0 = vfs
        .0
        .read("scripts/surfaceproperties.txt")
        .ok()
        .flatten()
        .map(|bytes| SurfaceTable::parse(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
}

/// The volume of an impact at `speed` units/s of a prop of `mass` kg.
#[must_use]
pub(crate) fn impact_volume(config: &rl_data::PropSoundsConfig, speed: f32, mass: f32) -> f32 {
    let blow = (speed / config.loud_speed).clamp(0.0, 1.0);
    let weight = (mass / config.loud_mass).clamp(0.0, 1.0);
    (config.min_volume + (1.0 - config.min_volume) * blow * (0.4 + 0.6 * weight)).clamp(0.0, 1.0)
}

fn hear_props(
    mut ticks: MessageReader<Ticked>,
    time: Res<Time>,
    game: Game,
    surfaces: Res<Surfaces>,
    settings: Res<Settings>,
    mut scrapes: ResMut<Scrapes>,
    mut player: SoundPlayer,
    mut emitters: Query<(
        &mut Emitter,
        &mut Transform,
        Option<&mut SpatialAudioSink>,
        Option<&mut AudioSink>,
    )>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    let config = sim.data().sounds.props;
    let now = time.elapsed_secs();
    let mut started = 0;
    for Ticked(events) in ticks.read() {
        for impact in &events.prop_impacts {
            if started >= config.max_per_frame {
                break;
            }
            let heard = scrapes
                .last_impact
                .get(&impact.body)
                .is_some_and(|&at| now - at < config.cooldown);
            if heard {
                continue;
            }
            let sounds = surfaces.0.sounds(sim.surface_of(impact.body));
            let name = if impact.speed > config.hard_speed {
                &sounds.impact_hard
            } else {
                &sounds.impact_soft
            };
            if name.is_empty() {
                continue;
            }
            scrapes.last_impact.insert(impact.body, now);
            started += 1;
            let volume = impact_volume(&config, impact.speed, impact.mass);
            player.play(name, Some(impact.position), volume);
        }
        for scrape in &events.prop_scrapes {
            let sounds = surfaces.0.sounds(sim.surface_of(scrape.body));
            // Smooth surfaces (metal, glass) scrape high; the rest rough.
            let name = if sounds.scrape_rough.is_empty() {
                &sounds.scrape_smooth
            } else {
                &sounds.scrape_rough
            };
            if name.is_empty() {
                continue;
            }
            let volume = (scrape.speed / config.scrape_loud_speed).clamp(0.2, 1.0);
            let position = rl_assets::coords::position(scrape.position.to_array());
            match scrapes.emitters.get_mut(&scrape.body) {
                Some((entity, last)) => {
                    *last = now;
                    if let Ok((mut emitter, mut transform, spatial, plain)) =
                        emitters.get_mut(*entity)
                    {
                        transform.translation = position;
                        emitter.volume = volume;
                        let level =
                            Volume::Linear(volume * SoundCategory::Effects.gain(&settings.audio));
                        if let Some(mut sink) = spatial {
                            sink.set_volume(level);
                        }
                        if let Some(mut sink) = plain {
                            sink.set_volume(level);
                        }
                    }
                }
                None => {
                    if let Some(entity) =
                        player.spawn(name, Some(scrape.position), volume, Repeat::Loop)
                    {
                        scrapes.emitters.insert(scrape.body, (entity, now));
                    }
                }
            }
        }
    }
    scrapes
        .last_impact
        .retain(|_, at| now - *at < 4.0 * config.cooldown.max(0.25));
}

/// Silences the scrapes nobody has reported lately.
fn stop_scrapes(mut commands: Commands, time: Res<Time>, game: Game, mut scrapes: ResMut<Scrapes>) {
    let timeout = game
        .sim()
        .map_or(0.3, |sim| sim.data().sounds.props.scrape_timeout);
    let now = time.elapsed_secs();
    scrapes.emitters.retain(|_, (entity, last)| {
        let keep = now - *last < timeout;
        if !keep {
            commands.entity(*entity).try_despawn();
        }
        keep
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> rl_data::PropSoundsConfig {
        rl_data::GameData::builtin().expect("builtin").sounds.props
    }

    #[test]
    fn faster_and_heavier_blows_are_louder_within_limits() {
        let config = config();
        let soft = impact_volume(&config, 130.0, 5.0);
        let hard = impact_volume(&config, 700.0, 60.0);
        assert!(soft >= config.min_volume && soft < 0.5, "{soft}");
        assert!((hard - 1.0).abs() < 1e-6, "{hard}");
        assert!(impact_volume(&config, 400.0, 60.0) > impact_volume(&config, 400.0, 5.0));
        assert!(impact_volume(&config, 400.0, 20.0) > impact_volume(&config, 200.0, 20.0));
        assert!(impact_volume(&config, 5000.0, 5000.0) <= 1.0);
        assert!(impact_volume(&config, 0.0, 0.0) >= config.min_volume);
    }
}
