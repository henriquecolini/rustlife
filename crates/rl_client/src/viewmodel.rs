//! The view model: the local player's weapon in first person.
//!
//! The weapon in hand's `view_model` (`models/weapons/v_smg1.mdl`, hands
//! and all) is drawn by a second camera that follows the main one: it sees
//! only the view model's render layer, draws over the world with a depth
//! buffer of its own (so the weapon never sinks into walls) and a narrower
//! field of view, as Source draws view models. The model's origin sits at
//! the eyes, its +X along the view, placed there each frame.
//!
//! It plays the model's activities as the local player acts, from their
//! predicted events: `ACT_VM_DRAW` when the weapon comes up,
//! `ACT_VM_PRIMARYATTACK` / `ACT_VM_SECONDARYATTACK` (a swing:
//! `ACT_VM_HITCENTER` / `ACT_VM_MISSCENTER`) when it fires, `ACT_VM_RELOAD`
//! when it reloads, and `ACT_VM_IDLE` once any of those is over. It is lit
//! like a moving prop, where the player's eyes are.

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::prelude::*;
use rl_data::Shot;
use rl_sim::combat::hitscan::Struck;
use rl_sim::weapons::{Mode, WeaponId};

use crate::AppState;
use crate::animation::{Animated, SequencePlayer};
use crate::camera::FlyCam;
use crate::net::Game;
use crate::player::{LocalPlayer, Ticked, ViewMode};
use crate::props::{LightAt, ModelHandles, Moving, spawn_model};

/// The render layer only the view model camera sees.
const VIEW_LAYER: usize = 1;
/// The view model camera's vertical field of view, degrees: Source's
/// `viewmodel_fov` of 54° across a 4:3 screen.
const VIEW_FOV_DEGREES: f32 = 42.0;
/// The view model camera's near plane, metres: weapons sit just in front
/// of the eyes.
const VIEW_NEAR: f32 = 0.01;

/// Draws and animates the view model.
pub struct ViewModelPlugin;

impl Plugin for ViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, add_view_camera).add_systems(
            Update,
            (
                swap_view_model,
                layer_meshes,
                animate_view_model,
                follow_camera,
                light_view_model,
            )
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// The camera that draws the view model.
#[derive(Component)]
struct ViewCamera;

/// The view model of a weapon.
#[derive(Component)]
struct ViewModel {
    weapon: WeaponId,
    /// Whether its draw sequence has been started.
    drawn: bool,
}

/// Gives the main camera a child camera for the view model.
fn add_view_camera(
    mut commands: Commands,
    cameras: Query<Entity, With<FlyCam>>,
    existing: Query<(), With<ViewCamera>>,
) {
    if !existing.is_empty() {
        return;
    }
    let Ok(main) = cameras.single() else {
        return;
    };
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            fov: VIEW_FOV_DEGREES.to_radians(),
            near: VIEW_NEAR,
            ..default()
        }),
        // As the main camera: the look of Source's renderer.
        Tonemapping::None,
        DebandDither::Disabled,
        RenderLayers::layer(VIEW_LAYER),
        ViewCamera,
        ChildOf(main),
    ));
}

/// The weapon in the local player's hand, and its view model, if they are
/// alive and walking (not flying in noclip).
fn wanted_view_model(
    game: &Game,
    local: Option<&LocalPlayer>,
    mode: Option<&ViewMode>,
) -> Option<(WeaponId, String)> {
    if mode.is_some_and(|mode| *mode == ViewMode::Noclip) {
        return None;
    }
    let sim = game.sim()?;
    let player = sim.player(local?.id).filter(|player| player.alive())?;
    let weapon = player.inventory.active()?;
    let model = sim.arsenal().weapon(weapon)?.view_model.clone()?;
    Some((weapon, model))
}

/// Spawns the view model of the weapon in hand, replacing that of the last
/// one; removes it while there is none to show.
fn swap_view_model(
    mut commands: Commands,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    mode: Option<Res<ViewMode>>,
    camera: Query<Entity, With<ViewCamera>>,
    current: Query<(Entity, &ViewModel)>,
    mut models: ResMut<ModelHandles>,
    asset_server: Res<AssetServer>,
) {
    let wanted = wanted_view_model(&game, local.as_deref(), mode.as_deref());
    let mut keep = false;
    for (entity, view) in &current {
        if wanted
            .as_ref()
            .is_some_and(|(weapon, _)| *weapon == view.weapon)
        {
            keep = true;
        } else {
            commands.entity(entity).despawn();
        }
    }
    let (Some((weapon, model)), Ok(_), false) = (wanted, camera.single(), keep) else {
        return;
    };
    debug!(model, "view model");
    let placement = Transform::from_rotation(facing());
    let entity = spawn_model(&mut commands, &mut models, &asset_server, &model, placement);
    commands.entity(entity).insert((
        ViewModel {
            weapon,
            drawn: false,
        },
        Moving,
        LightAt(Vec3::ZERO),
        RenderLayers::layer(VIEW_LAYER),
    ));
}

/// Keeps the view model at the eyes, turned with the view. It is a root
/// entity placed each frame rather than the camera's child: cameras carry
/// no visibility of their own for a child to inherit.
fn follow_camera(
    camera: Query<&Transform, (With<FlyCam>, Without<ViewModel>)>,
    mut views: Query<&mut Transform, With<ViewModel>>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    for mut transform in &mut views {
        *transform = Transform {
            translation: camera.translation,
            rotation: camera.rotation * facing(),
            scale: Vec3::ONE,
        };
    }
}

/// Meshes just spawned.
type NewMesh = (Added<Mesh3d>, Without<NoFrustumCulling>);

/// Keeps the view model's meshes (which take its layer from their root,
/// see `world`) from being culled: a view model's meshes
/// rest well above the eyes (Source models them on a standing skeleton)
/// and only its animation brings them into view, while culling goes by
/// the rest pose's bounds.
fn layer_meshes(
    mut commands: Commands,
    meshes: Query<(Entity, &ChildOf), NewMesh>,
    views: Query<(), With<ViewModel>>,
) {
    for (entity, parent) in &meshes {
        if views.contains(parent.parent()) {
            commands.entity(entity).insert(NoFrustumCulling);
        }
    }
}

/// The activity a fire mode's shot plays.
fn fire_activity(shot: &Shot, mode: Mode, struck: Option<Struck>) -> &'static str {
    match (shot, mode) {
        (Shot::Melee(_), _) => match struck {
            Some(Struck::Nothing) | None => "ACT_VM_MISSCENTER",
            Some(_) => "ACT_VM_HITCENTER",
        },
        (_, Mode::Primary) => "ACT_VM_PRIMARYATTACK",
        (_, Mode::Secondary) => "ACT_VM_SECONDARYATTACK",
    }
}

/// Plays the view model's activities as the local player acts.
fn animate_view_model(
    mut ticks: MessageReader<Ticked>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    mut views: Query<(&mut ViewModel, &Animated, &mut SequencePlayer)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        ticks.clear();
        return;
    };
    let Ok((mut view, animated, mut player)) = views.single_mut() else {
        ticks.clear();
        return;
    };
    let mut wanted: Option<&str> = None;
    if !view.drawn {
        view.drawn = true;
        wanted = Some("ACT_VM_DRAW");
    }
    for Ticked(events) in ticks.read() {
        let mine = |id| id == local.id;
        if events.reloads.iter().any(|&(id, _)| mine(id)) {
            wanted = Some("ACT_VM_RELOAD");
        }
        for &(id, weapon, mode) in &events.fired {
            if !mine(id) || weapon != view.weapon {
                continue;
            }
            let Some(fire) = sim.arsenal().fire(weapon, mode) else {
                continue;
            };
            let struck = events
                .shots
                .iter()
                .find(|shot| mine(shot.player))
                .map(|shot| shot.trace.struck);
            wanted = Some(fire_activity(&fire.shot, mode, struck));
        }
        if events.tools.iter().any(|&(id, _)| mine(id)) && wanted.is_none() {
            wanted = Some("ACT_VM_PRIMARYATTACK");
        }
    }
    let sequences = &animated.animation.sequences;
    if let Some(activity) = wanted
        && let Some(sequence) = animated.activity(activity)
    {
        player.restart(sequence, 1.0);
        return;
    }
    let idle = player.current().is_none() || player.finished(sequences);
    if idle && let Some(sequence) = animated.activity("ACT_VM_IDLE") {
        player.play(sequence, 0.0, 1.0);
    }
}

/// Keeps the view model lit where the player's eyes are.
fn light_view_model(
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut views: Query<&mut LightAt, With<ViewModel>>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    for mut light in &mut views {
        light.0 = camera.translation();
    }
}

/// The view model's turn under its camera: Source models face +X, the
/// camera looks down its −Z.
fn facing() -> Quat {
    Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
}

#[cfg(test)]
mod tests {
    use rl_assets::coords;

    use super::*;

    #[test]
    fn view_models_face_along_the_view() {
        // Source +X is Bevy +X; turned, it points where the camera looks.
        let forward = facing() * coords::position([1.0, 0.0, 0.0]).normalize();
        assert!(forward.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{forward}");
        let up = facing() * coords::position([0.0, 0.0, 1.0]).normalize();
        assert!(up.abs_diff_eq(Vec3::Y, 1e-5), "{up}");
    }

    #[test]
    fn swings_play_hit_or_miss() {
        let swing = Shot::Melee(rl_data::MeleeConfig {
            range: 1.0,
            size: 1.0,
            damage: 1.0,
            force: 1.0,
        });
        assert_eq!(
            fire_activity(&swing, Mode::Primary, Some(Struck::World)),
            "ACT_VM_HITCENTER"
        );
        assert_eq!(
            fire_activity(&swing, Mode::Primary, Some(Struck::Nothing)),
            "ACT_VM_MISSCENTER"
        );
    }
}
