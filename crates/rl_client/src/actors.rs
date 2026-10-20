//! Models the game moves: pickups, projectiles and other players.
//!
//! Each is drawn with its model through the props pipeline
//! (`props::spawn_model`: lit like any prop, but with no physics
//! body of its own) and placed from the game:
//!
//! - **pickups** where the map put them, hidden while taken;
//! - **projectiles** along their flight, turned to face where they fly,
//!   appearing and vanishing with them;
//! - **other players** at their feet, turned to their view's yaw, hidden
//!   while dead; models come and go as players join and leave. Models show
//!   their reference pose until animation arrives (M7).
//!
//! Projectiles and other players are drawn where [`Drawn`] places them:
//! interpolated between two of the server's snapshots, a little in the
//! past, every frame.

use bevy::prelude::*;
use rl_assets::coords;
use rl_sim::simulation::PlayerId;

use crate::AppState;
use crate::net::{Drawn, Game};
use crate::player::Gameplay;
use crate::props::{ModelHandles, Moving, spawn_model};

/// Draws the game's pickups, projectiles and other players.
pub struct ActorsPlugin;

impl Plugin for ActorsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PickupsSpawned>()
            .add_systems(OnEnter(AppState::InGame), reset)
            .add_systems(
                Update,
                (spawn_pickups, show_pickups, draw_players, draw_projectiles)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Whether the pickups of the current game have models.
#[derive(Resource, Default)]
struct PickupsSpawned(bool);

/// The model of a pickup, by index into the simulation's pickups.
#[derive(Component)]
struct PickupView(usize);

/// The model of another player.
#[derive(Component)]
struct PlayerView(PlayerId);

/// The model of a projectile, by its id.
#[derive(Component)]
struct ProjectileView(u32);

fn reset(mut spawned: ResMut<PickupsSpawned>) {
    spawned.0 = false;
}

/// Gives every pickup a model once the game's simulation exists.
fn spawn_pickups(
    mut commands: Commands,
    mut spawned: ResMut<PickupsSpawned>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    game: Game,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    if spawned.0 {
        return;
    }
    spawned.0 = true;
    for (index, pickup) in sim.pickups().iter().enumerate() {
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
}

/// Hides taken pickups.
fn show_pickups(game: Game, mut pickups: Query<(&PickupView, &mut Visibility)>) {
    let Some(sim) = game.sim() else {
        return;
    };
    for (view, mut visibility) in &mut pickups {
        let shown = sim
            .pickups()
            .get(view.0)
            .is_some_and(rl_sim::pickups::Pickup::available);
        visibility.set_if_neq(if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

/// The transform of a player model standing at `feet` facing `yaw`.
fn player_transform(feet: Vec3, yaw: f32) -> Transform {
    Transform {
        translation: coords::position(feet.to_array()),
        rotation: coords::rotation([0.0, yaw, 0.0]),
        ..default()
    }
}

/// Places every other player where they are drawn this frame, hiding the
/// dead; gives newcomers a model and takes it from those who left.
fn draw_players(
    mut commands: Commands,
    drawn: Res<Drawn>,
    game: Game,
    gameplay: Res<Gameplay>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    mut views: Query<(Entity, &PlayerView, &mut Transform, &mut Visibility)>,
) {
    let Some(world) = &drawn.0 else {
        return;
    };
    let local = game.local();
    let mut shown = Vec::new();
    for (entity, view, mut transform, mut visibility) in &mut views {
        match world.players.get(&view.0).filter(|_| Some(view.0) != local) {
            Some(pose) => {
                *transform = player_transform(pose.feet, pose.yaw);
                visibility.set_if_neq(if pose.alive {
                    Visibility::Inherited
                } else {
                    Visibility::Hidden
                });
                shown.push(view.0);
            }
            None => commands.entity(entity).despawn(),
        }
    }
    let player_models = &gameplay.0.player.models;
    for (&id, pose) in &world.players {
        if Some(id) == local || shown.contains(&id) {
            continue;
        }
        let Some(model) = player_models.get(id.index() % player_models.len().max(1)) else {
            continue;
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            model,
            player_transform(pose.feet, pose.yaw),
        );
        commands.entity(entity).insert((PlayerView(id), Moving));
    }
}

/// Places every projectile where it is drawn this frame: new ones get a
/// model, gone ones lose theirs.
fn draw_projectiles(
    mut commands: Commands,
    drawn: Res<Drawn>,
    game: Game,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
    mut views: Query<(Entity, &ProjectileView, &mut Transform)>,
) {
    let (Some(world), Some(sim)) = (&drawn.0, game.sim()) else {
        return;
    };
    let mut shown = Vec::new();
    for (entity, view, mut transform) in &mut views {
        match world
            .projectiles
            .iter()
            .find(|projectile| projectile.id == view.0)
        {
            Some(projectile) => {
                *transform = projectile_transform(projectile.position, projectile.velocity);
                shown.push(view.0);
            }
            None => commands.entity(entity).despawn(),
        }
    }
    let arsenal = sim.arsenal();
    for projectile in &world.projectiles {
        if shown.contains(&projectile.id) {
            continue;
        }
        let Some(rl_data::Shot::Projectile(config)) = arsenal
            .fire(projectile.weapon, projectile.mode)
            .map(|fire| &fire.shot)
        else {
            continue;
        };
        let entity = spawn_model(
            &mut commands,
            &mut models,
            &asset_server,
            &config.model,
            projectile_transform(projectile.position, projectile.velocity),
        );
        commands
            .entity(entity)
            .insert((ProjectileView(projectile.id), Moving));
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

    #[test]
    fn players_stand_at_their_feet_facing_their_yaw() {
        let transform = player_transform(Vec3::new(10.0, 0.0, 0.0), 90.0);
        assert!(transform.translation.x > 0.0);
        let facing = transform.rotation * Vec3::X;
        assert!(facing.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{facing}");
    }
}
