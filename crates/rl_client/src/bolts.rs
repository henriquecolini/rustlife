//! Crossbow bolts left stuck where they embed, as HL2DM's `BoltImpact` leaves
//! them.
//!
//! The server reports a bolt embedding in the map ([`ProjectileEvent`] of
//! kind `Embed`, for a projectile whose effect is
//! [`ProjectileEffect::Bolt`]) and ends it there; the client does the rest,
//! as client.so's `BoltImpact` callback (`StickyBoltCallback`, objdump
//! 0x7840f0) and `CreateCrossbowBolt` (0x783d10) do: the bolt's model, with
//! its plain skin, set 8 units back from where it went in along its flight
//! and facing along it, so its tip is in the wall and its tail sticks out.
//! The game keeps it as a temporary model for 30 seconds; here it stays for
//! the rest of the map, the user's wish, [`MAX_STUCK`] at most, the oldest
//! going first. Where a bolt glances off or embeds, `g_pEffects->Sparks`
//! throws electric sparks (magnitude 1, trail 1, no direction).
//!
//! The surface's own impact effect and mark there come from the bolt's impact
//! trace (a late [`rl_sim::simulation::ShotEvent`], [`crate::effects`]).

use std::collections::VecDeque;

use bevy::prelude::*;
use rl_assets::coords;
use rl_data::{ProjectileEffect, Shot};
use rl_sim::simulation::{ProjectileEvent, ProjectileEventKind, Simulation};

use crate::AppState;
use crate::net::Game;
use crate::player::Ticked;
use crate::props::{ModelHandles, spawn_model};
use crate::trail_sparks::{Burst, Sparks};

/// Most bolts left stuck at once.
pub(crate) const MAX_STUCK: usize = 128;
/// How far back along its flight from where it went in a stuck bolt's model
/// is set, units (`CreateCrossbowBolt`).
const SET_BACK: f32 = 8.0;

/// Leaves bolts stuck where they embed.
pub(crate) struct BoltsPlugin;

impl Plugin for BoltsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StuckBolts>()
            .add_systems(OnEnter(AppState::InGame), forget)
            .add_systems(
                Update,
                stick_bolts
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The stuck bolts' models, oldest first.
#[derive(Resource, Default)]
pub(crate) struct StuckBolts(VecDeque<Entity>);

impl StuckBolts {
    /// Keeps `bolt`, returning the oldest to remove if there are now more
    /// than [`MAX_STUCK`].
    fn keep(&mut self, bolt: Entity) -> Option<Entity> {
        self.0.push_back(bolt);
        (self.0.len() > MAX_STUCK)
            .then(|| self.0.pop_front())
            .flatten()
    }
}

/// A new map starts with none (the old map's are despawned with it).
fn forget(mut stuck: ResMut<StuckBolts>) {
    stuck.0.clear();
}

/// The projectile effect of the projectile an event is about.
pub(crate) fn effect_of(sim: &Simulation, event: &ProjectileEvent) -> Option<ProjectileEffect> {
    match sim
        .arsenal()
        .fire(event.weapon, event.mode)
        .map(|fire| &fire.shot)
    {
        Some(Shot::Projectile(projectile)) => projectile.effect,
        _ => None,
    }
}

/// Where a bolt that went in at `point` flying along `direction` (Source
/// space) is drawn stuck: set back along its flight, facing along it (the
/// model's +X is its tip).
pub(crate) fn stuck_transform(point: Vec3, direction: Vec3) -> Transform {
    let direction = direction.normalize_or(Vec3::X);
    let yaw = direction.y.atan2(direction.x).to_degrees();
    let pitch = -direction.z.clamp(-1.0, 1.0).asin().to_degrees();
    Transform {
        translation: coords::position((point - direction * SET_BACK).to_array()),
        rotation: coords::rotation([pitch, yaw, 0.0]),
        ..default()
    }
}

/// Sparks where bolts glance off or embed, and a stuck bolt where they embed.
fn stick_bolts(
    mut commands: Commands,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    mut stuck: ResMut<StuckBolts>,
    mut sparks: MessageWriter<Sparks>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    for Ticked(events) in ticks.read() {
        for event in &events.projectiles {
            if effect_of(sim, event) != Some(ProjectileEffect::Bolt) {
                continue;
            }
            let at = coords::position(event.position.to_array());
            match event.kind {
                ProjectileEventKind::Bounce | ProjectileEventKind::Embed => {
                    sparks.write(Sparks {
                        at,
                        direction: Vec3::ZERO,
                        burst: Burst::Electric {
                            magnitude: 1,
                            trail: 1,
                        },
                    });
                }
                ProjectileEventKind::Burst => {}
            }
            if event.kind != ProjectileEventKind::Embed {
                continue;
            }
            let Some(Shot::Projectile(config)) = sim
                .arsenal()
                .fire(event.weapon, event.mode)
                .map(|fire| &fire.shot)
            else {
                continue;
            };
            let bolt = spawn_model(
                &mut commands,
                &mut models,
                &asset_server,
                &config.model,
                stuck_transform(event.position, event.normal),
            );
            if let Some(oldest) = stuck.keep(bolt) {
                commands.entity(oldest).try_despawn();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stuck_bolt_sits_back_along_its_flight_facing_along_it() {
        let transform = stuck_transform(Vec3::new(300.0, 0.0, 50.0), Vec3::X);
        let tail = Vec3::from(coords::to_source_position(transform.translation));
        assert!(tail.distance(Vec3::new(292.0, 0.0, 50.0)) < 1e-3, "{tail}");
        // The model's +X (Source) is Bevy's +X: it faces into the wall.
        let facing = transform.rotation * Vec3::X;
        assert!(facing.abs_diff_eq(Vec3::X, 1e-5), "{facing}");
        // Shot down into a floor: the tip points down (Bevy -Y).
        let down = stuck_transform(Vec3::ZERO, Vec3::NEG_Z);
        assert!((down.rotation * Vec3::X).abs_diff_eq(Vec3::NEG_Y, 1e-5));
    }

    #[test]
    fn no_more_than_the_most_are_kept_the_oldest_going_first() {
        let mut stuck = StuckBolts::default();
        let mut world = World::new();
        let bolts: Vec<Entity> = (0..=MAX_STUCK).map(|_| world.spawn_empty().id()).collect();
        for &bolt in &bolts[..MAX_STUCK] {
            assert_eq!(stuck.keep(bolt), None);
        }
        assert_eq!(stuck.keep(bolts[MAX_STUCK]), Some(bolts[0]));
        assert_eq!(stuck.0.len(), MAX_STUCK);
        assert_eq!(stuck.0.front(), Some(&bolts[1]));
    }
}
