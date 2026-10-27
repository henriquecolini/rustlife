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
//! when it reloads (a shotgun's start, each shell and finish), `ACT_VM_DRYFIRE`
//! on an empty click, `ACT_SHOTGUN_PUMP` when it is pumped, `ACT_VM_FIDGET`
//! while a shot charges, and `ACT_VM_IDLE` once any of those is over. A weapon
//! that goes off on release (a grenade) is drawn back while its button is
//! held (`ACT_VM_PULLBACK_HIGH` for the overhand throw, `ACT_VM_PULLBACK_LOW`
//! for the roll), holding the pose until it is let go, then thrown
//! (`ACT_VM_THROW`, or the roll's `ACT_VM_SECONDARYATTACK`); the next one is
//! brought up (`ACT_VM_DRAW`) if there is one. It is lit like a moving prop,
//! where the player's eyes are.

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::prelude::*;
use rl_data::{FireConfig, Shot};
use rl_sim::combat::hitscan::Struck;
use rl_sim::weapons::{Arsenal, Inventory, Mode, Phase, ReloadStep, WeaponId};

use crate::AppState;
use crate::animation::{Animated, SequencePlayer};
use crate::camera::FlyCam;
use crate::laser::LaserPlugin;
use crate::net::Game;
use crate::player::{LocalPlayer, Ticked, ViewMode};
use crate::props::{LightAt, ModelHandles, Moving, spawn_model};
use crate::zoom::ZoomPlugin;

/// The render layer only the view model camera sees.
pub(crate) const VIEW_LAYER: usize = 1;
/// The view model camera's vertical field of view, degrees: Source's
/// `viewmodel_fov` of 54° across a 4:3 screen.
pub(crate) const VIEW_FOV_DEGREES: f32 = 42.0;
/// The view model camera's near plane, metres: weapons sit just in front
/// of the eyes.
const VIEW_NEAR: f32 = 0.01;

/// Draws and animates the view model.
pub struct ViewModelPlugin;

impl Plugin for ViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((ZoomPlugin, LaserPlugin))
            .add_systems(Update, add_view_camera)
            .add_systems(
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
pub(crate) struct ViewModel {
    weapon: WeaponId,
    /// Whether its draw sequence has been started.
    drawn: bool,
    /// The fire mode being held primed, whose pull-back has been started.
    primed: Option<Mode>,
    /// The weapon was just thrown: when that ends the next one is drawn
    /// rather than the weapon idling.
    thrown: bool,
    /// How many reload steps, empty clicks and pumps the inventory had
    /// counted when they were last looked at: a change plays the animation.
    reloads: u8,
    dries: u8,
    pumps: u8,
    /// A shot charging (or a grenade being let go) has started its
    /// animation.
    charging: bool,
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
    eyes: Query<&GlobalTransform, With<FlyCam>>,
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
            primed: None,
            thrown: false,
            reloads: 0,
            dries: 0,
            pumps: 0,
            charging: false,
        },
        Moving,
        // Lit from where the eyes are from the first frame: a model that
        // loads at once (a weapon taken up again) would otherwise be lit
        // from the world's origin, usually inside a wall.
        LightAt(
            eyes.single()
                .map_or(Vec3::ZERO, GlobalTransform::translation),
        ),
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

/// The activity of drawing back a fire mode that goes off on release.
fn prime_activity(mode: Mode) -> &'static str {
    match mode {
        Mode::Primary => "ACT_VM_PULLBACK_HIGH",
        Mode::Secondary => "ACT_VM_PULLBACK_LOW",
    }
}

/// The activity a fire mode's shot plays. A grenade let go of crouching is
/// rolled (`ducked`), standing it is lobbed.
fn fire_activity(
    fire: &FireConfig,
    mode: Mode,
    struck: Option<Struck>,
    ducked: bool,
) -> &'static str {
    match (&fire.shot, mode) {
        (_, Mode::Primary) if fire.on_release => "ACT_VM_THROW",
        (_, Mode::Secondary) if fire.on_release && !ducked => "ACT_VM_HAULBACK",
        (Shot::Melee(_), _) => match struck {
            Some(Struck::Nothing) | None => "ACT_VM_MISSCENTER",
            Some(_) => "ACT_VM_HITCENTER",
        },
        (_, Mode::Primary) => "ACT_VM_PRIMARYATTACK",
        (_, Mode::Secondary) => "ACT_VM_SECONDARYATTACK",
    }
}

/// The activity a reload step plays.
fn reload_activity(step: ReloadStep) -> &'static str {
    match step {
        ReloadStep::Whole | ReloadStep::Round => "ACT_VM_RELOAD",
        ReloadStep::Start => "ACT_SHOTGUN_RELOAD_START",
        ReloadStep::Finish => "ACT_SHOTGUN_RELOAD_FINISH",
    }
}

/// The activity the inventory's state calls for: a new reload step, an empty
/// click or a pump (counted by the inventory since last looked at), a shot
/// charging, or a grenade let go of.
fn inventory_activity(
    view: &mut ViewModel,
    inventory: &Inventory,
    arsenal: &Arsenal,
    ducked: bool,
) -> Option<&'static str> {
    let mut wanted = None;
    if inventory.reload_count() != view.reloads {
        view.reloads = inventory.reload_count();
        wanted = Some(reload_activity(inventory.reload_step()));
    }
    if inventory.dry_count() != view.dries {
        view.dries = inventory.dry_count();
        wanted = Some("ACT_VM_DRYFIRE");
    }
    if inventory.pump_count() != view.pumps {
        view.pumps = inventory.pump_count();
        wanted = Some("ACT_SHOTGUN_PUMP");
    }
    match inventory.phase() {
        Phase::Charging(mode, _) if !view.charging => {
            view.charging = true;
            if let Some(fire) = arsenal.fire(view.weapon, mode) {
                view.thrown = fire.on_release;
                wanted = Some(if fire.on_release {
                    fire_activity(fire, mode, None, ducked)
                } else {
                    "ACT_VM_FIDGET"
                });
            }
        }
        Phase::Charging(..) => {}
        _ => view.charging = false,
    }
    wanted
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
    let local_player = sim.player(local.id);
    let inventory = local_player.map(|player| &player.inventory);
    let ducked = local_player.is_some_and(|player| player.state.ducked);
    if !view.drawn {
        view.drawn = true;
        player.fade = VIEW_FADE_SECS;
        wanted = Some("ACT_VM_DRAW");
        if let Some(inventory) = inventory {
            view.reloads = inventory.reload_count();
            view.dries = inventory.dry_count();
            view.pumps = inventory.pump_count();
        }
    }
    if let Some(inventory) = inventory {
        wanted = inventory_activity(&mut view, inventory, sim.arsenal(), ducked).or(wanted);
    }
    // A weapon drawn back keeps its pose for as long as it is held.
    let primed = sim
        .player(local.id)
        .and_then(|player| match player.inventory.phase() {
            Phase::Priming(mode) => Some(mode),
            _ => None,
        });
    if primed != view.primed {
        view.primed = primed;
        if let Some(mode) = primed {
            wanted = Some(prime_activity(mode));
        }
    }
    for Ticked(events) in ticks.read() {
        let mine = |id| id == local.id;
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
            // A grenade's throw began when the button was let go.
            if fire.on_release {
                view.thrown = true;
                continue;
            }
            wanted = Some(fire_activity(fire, mode, struck, ducked));
        }
        if events.tools.iter().any(|&(id, _)| mine(id)) && wanted.is_none() {
            wanted = Some("ACT_VM_PRIMARYATTACK");
        }
    }
    let sequences = &animated.animation.sequences;
    if let Some(activity) = wanted
        && let Some(sequence) = animated.activity(activity)
    {
        if view.primed.is_some() && activity.starts_with("ACT_VM_PULLBACK") {
            player.restart_held(sequence, 1.0);
        } else {
            player.restart(sequence, 1.0);
        }
        return;
    }
    // The idle follows a swing or shot as it ends, faded in over it (a
    // thrown weapon's next one is brought up instead); a weapon drawn back
    // stays so.
    let idle = player.current().is_none() || player.ends_within(sequences, player.fade);
    if idle && view.primed.is_none() {
        if view.thrown {
            view.thrown = false;
            if let Some(sequence) = animated.activity("ACT_VM_DRAW") {
                player.restart(sequence, 1.0);
            }
        } else if let Some(sequence) = animated.activity("ACT_VM_IDLE") {
            player.play(sequence, 0.0, 1.0);
        }
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

/// Seconds one view model sequence fades into the next over: Source's own
/// sequence blends are about this long. Without it a swing's last pose cuts
/// to the idle's first.
const VIEW_FADE_SECS: f32 = 0.15;

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

    fn fire(shot: Shot, on_release: bool) -> FireConfig {
        FireConfig {
            interval: 1.0,
            other_interval: 0.0,
            click_interval: None,
            charge: 0.0,
            automatic: false,
            ammo: rl_data::FireAmmo::Unlimited,
            shot,
            on_release,
        }
    }

    #[test]
    fn swings_play_hit_or_miss() {
        let swing = fire(
            Shot::Melee(rl_data::MeleeConfig {
                range: 1.0,
                size: 1.0,
                damage: 1.0,
                force: 1.0,
            }),
            false,
        );
        assert_eq!(
            fire_activity(&swing, Mode::Primary, Some(Struck::World), false),
            "ACT_VM_HITCENTER"
        );
        assert_eq!(
            fire_activity(&swing, Mode::Primary, Some(Struck::Nothing), false),
            "ACT_VM_MISSCENTER"
        );
    }

    #[test]
    fn grenades_are_drawn_back_then_thrown_or_rolled() {
        let data = rl_data::GameData::builtin().expect("data");
        let frag = data.weapons.get("frag").expect("frag");
        let overhand = frag.primary.as_ref().expect("primary");
        let roll = frag.secondary.as_ref().expect("secondary");
        assert_eq!(prime_activity(Mode::Primary), "ACT_VM_PULLBACK_HIGH");
        assert_eq!(prime_activity(Mode::Secondary), "ACT_VM_PULLBACK_LOW");
        assert_eq!(
            fire_activity(overhand, Mode::Primary, None, false),
            "ACT_VM_THROW"
        );
        assert_eq!(
            fire_activity(roll, Mode::Secondary, None, true),
            "ACT_VM_SECONDARYATTACK"
        );
        assert_eq!(
            fire_activity(roll, Mode::Secondary, None, false),
            "ACT_VM_HAULBACK"
        );
    }
}
