//! Models the simulation moves: pickups, projectiles and other players.
//!
//! Each is drawn with its model through the props pipeline
//! (`props::spawn_model`: lit like any prop, but with no physics
//! body of its own) and placed from the simulation:
//!
//! - **pickups** where the map put them, hidden while taken;
//! - **projectiles** along their flight, turned to face where they fly,
//!   appearing and vanishing with them;
//! - **other players** at their feet, turned to their view's yaw, hidden
//!   while dead. Models show their reference pose until animation arrives
//!   (M7).
//!
//! Things that move are interpolated between the last two ticks, like the
//! camera and the props.

use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::simulation::PlayerId;

use crate::AppState;
use crate::player::{Gameplay, LocalPlayer, Sim, SimulationTick};
use crate::props::{ModelHandles, Moving, spawn_model};

/// Draws the simulation's pickups, projectiles and other players.
pub struct ActorsPlugin;

impl Plugin for ActorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ActorsSpawned>()
            .add_systems(OnEnter(AppState::InGame), reset)
            .add_systems(
                Update,
                (spawn_actors, show_pickups, place_players, place_projectiles)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                FixedUpdate,
                (record_players, record_projectiles)
                    .after(SimulationTick)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Whether the pickups and players of the current simulation have models.
#[derive(Resource, Default)]
struct ActorsSpawned(bool);

/// The model of a pickup, by index into the simulation's pickups.
#[derive(Component)]
struct PickupView(usize);

/// The model of another player, and where they stood after the last two
/// ticks: feet (Source space) and yaw (degrees).
#[derive(Component)]
struct PlayerView {
    id: PlayerId,
    previous: (Vec3, f32),
    current: (Vec3, f32),
    alive: bool,
}

/// The model of a projectile, and where it was after the last two ticks
/// (Source space).
#[derive(Component)]
struct ProjectileView {
    id: u32,
    previous: Vec3,
    current: Vec3,
    velocity: Vec3,
}

fn reset(mut spawned: ResMut<ActorsSpawned>) {
    spawned.0 = false;
}

/// Gives every pickup and every other player a model once the simulation
/// exists.
fn spawn_actors(
    mut commands: Commands,
    mut spawned: ResMut<ActorsSpawned>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    sim: Option<Res<Sim>>,
    local: Option<Res<LocalPlayer>>,
    gameplay: Res<Gameplay>,
) {
    let (Some(sim), Some(local)) = (sim, local) else {
        return;
    };
    if spawned.0 {
        return;
    }
    spawned.0 = true;
    for (index, pickup) in sim.0.pickups().iter().enumerate() {
        let transform = Transform {
            translation: coords::position(pickup.origin.to_array()),
            rotation: coords::rotation(pickup.angles),
            ..default()
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            &pickup.model,
            transform,
        );
        commands.entity(entity).insert(PickupView(index));
    }
    let player_models = &gameplay.0.player.models;
    for (id, player) in sim.0.players().filter(|&(id, _)| id != local.id) {
        let Some(model) = player_models.get(id.index() % player_models.len().max(1)) else {
            continue;
        };
        let at = (player.state.origin, player.look.1);
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            model,
            player_transform(at),
        );
        commands.entity(entity).insert((
            PlayerView {
                id,
                previous: at,
                current: at,
                alive: player.alive(),
            },
            Moving,
        ));
    }
}

/// The transform of a player model standing at `feet` facing `yaw`.
fn player_transform((feet, yaw): (Vec3, f32)) -> Transform {
    Transform {
        translation: coords::position(feet.to_array()),
        rotation: coords::rotation([0.0, yaw, 0.0]),
        ..default()
    }
}

/// Hides taken pickups.
fn show_pickups(sim: Option<Res<Sim>>, mut pickups: Query<(&PickupView, &mut Visibility)>) {
    let Some(sim) = sim else {
        return;
    };
    for (view, mut visibility) in &mut pickups {
        let shown = sim
            .0
            .pickups()
            .get(view.0)
            .is_some_and(rl_sim::pickups::Pickup::available);
        let wanted = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        visibility.set_if_neq(wanted);
    }
}

/// Reads every other player's position back after a tick.
fn record_players(sim: Option<Res<Sim>>, mut players: Query<&mut PlayerView>) {
    let Some(sim) = sim else {
        return;
    };
    for mut view in &mut players {
        if let Some(player) = sim.0.player(view.id) {
            let now = (player.state.origin, player.look.1);
            // Do not glide across the map from where they died.
            view.previous = if view.alive == player.alive() {
                view.current
            } else {
                now
            };
            view.current = now;
            view.alive = player.alive();
        }
    }
}

/// Places other players between their last two ticks; hides the dead.
fn place_players(
    time: Res<Time<Fixed>>,
    mut players: Query<(&PlayerView, &mut Transform, &mut Visibility)>,
) {
    let alpha = time.overstep_fraction();
    for (view, mut transform, mut visibility) in &mut players {
        let feet = view.previous.0.lerp(view.current.0, alpha);
        *transform = player_transform((feet, view.current.1));
        visibility.set_if_neq(if view.alive {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// Follows the simulation's projectiles after a tick: new ones get a model,
/// gone ones lose theirs.
fn record_projectiles(
    mut commands: Commands,
    sim: Option<Res<Sim>>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    mut views: Query<(Entity, &mut ProjectileView)>,
) {
    let Some(sim) = sim else {
        return;
    };
    let projectiles = sim.0.projectiles();
    for (entity, mut view) in &mut views {
        match projectiles
            .iter()
            .find(|projectile| projectile.id == view.id)
        {
            Some(projectile) => {
                view.previous = view.current;
                view.current = projectile.position;
                view.velocity = projectile.velocity;
            }
            None => commands.entity(entity).despawn(),
        }
    }
    let arsenal = sim.0.arsenal();
    for projectile in projectiles {
        if views.iter().any(|(_, view)| view.id == projectile.id) {
            continue;
        }
        let Some(rl_data::Shot::Projectile(config)) = arsenal
            .fire(projectile.weapon, projectile.mode)
            .map(|fire| &fire.shot)
        else {
            continue;
        };
        let transform = projectile_transform(projectile.position, projectile.velocity);
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            &config.model,
            transform,
        );
        commands.entity(entity).insert((
            ProjectileView {
                id: projectile.id,
                previous: projectile.position,
                current: projectile.position,
                velocity: projectile.velocity,
            },
            Moving,
        ));
    }
}

/// The transform of a projectile at `position` flying along `velocity`
/// (Source space): models face Source +X.
fn projectile_transform(position: Vec3, velocity: Vec3) -> Transform {
    let direction = velocity.normalize_or(Vec3::X);
    let yaw = direction.y.atan2(direction.x).to_degrees();
    let pitch = -direction.z.asin().to_degrees();
    Transform {
        translation: coords::position(position.to_array()),
        rotation: coords::rotation([pitch, yaw, 0.0]),
        ..default()
    }
}

/// Places projectiles between their last two ticks.
fn place_projectiles(
    time: Res<Time<Fixed>>,
    mut projectiles: Query<(&ProjectileView, &mut Transform)>,
) {
    let alpha = time.overstep_fraction();
    for (view, mut transform) in &mut projectiles {
        let position = view.previous.lerp(view.current, alpha);
        *transform = projectile_transform(position, view.velocity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projectiles_face_where_they_fly() {
        // Flying along Source +Y (Bevy -Z): the model's +X turns to face it.
        let transform = projectile_transform(Vec3::ZERO, Vec3::new(0.0, 500.0, 0.0));
        let facing = transform.rotation * Vec3::X;
        assert!(facing.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{facing}");
        // Climbing: the nose tips up (Bevy +Y).
        let transform = projectile_transform(Vec3::ZERO, Vec3::new(1.0, 0.0, 1.0));
        let facing = transform.rotation * Vec3::X;
        assert!(facing.y > 0.6, "{facing}");
    }
}
