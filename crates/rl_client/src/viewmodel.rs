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
use bevy::camera::{CameraOutputMode, Hdr};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::post_process::bloom::{Bloom, BloomCompositeMode, BloomPrefilter};
use bevy::prelude::*;
use bevy::render::render_resource::BlendState;
use rl_data::{FireConfig, Shot};
use rl_sim::combat::hitscan::Struck;
use rl_sim::tools::manipulator::ToolEvent;
use rl_sim::weapons::{Arsenal, Inventory, Mode, Phase, ReloadStep, WeaponId};

use crate::AppState;
use crate::animation::{Animated, SequencePlayer};
use crate::camera::FlyCam;
use crate::laser::LaserPlugin;
use crate::net::Game;
use crate::player::{LocalPlayer, Ticked, ViewMode};
use crate::props::{LightAt, ModelHandles, Moving, Prop, spawn_model};
use crate::world::{MaterialMaps, PartSlot, Surface};
use crate::zoom::ZoomPlugin;

/// The render layer only the view model camera sees.
pub(crate) const VIEW_LAYER: usize = 1;
/// The view model camera's vertical field of view, degrees: Source's
/// `viewmodel_fov` of 54° across a 4:3 screen.
pub(crate) const VIEW_FOV_DEGREES: f32 = 42.0;
/// The view model camera's near plane, metres: weapons sit just in front
/// of the eyes.
const VIEW_NEAR: f32 = 0.01;

/// The view model's bloom, which is what makes the Gravity Gun's cage and
/// sprites glow: Source's HDR bloom, which lets the brightest parts of the
/// frame bleed into the pixels around them. Only what is lit past
/// `threshold` does; the metal of the weapons stays as it is.
fn view_bloom() -> Bloom {
    Bloom {
        intensity: 0.5,
        low_frequency_boost: 0.6,
        low_frequency_boost_curvature: 0.9,
        prefilter: BloomPrefilter {
            threshold: 0.45,
            threshold_softness: 0.5,
        },
        composite_mode: BloomCompositeMode::Additive,
        ..Bloom::NATURAL
    }
}

/// Draws and animates the view model.
pub struct ViewModelPlugin;

impl Plugin for ViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ZoomPlugin,
            LaserPlugin,
            crate::gravity_gun::GravityGunPlugin,
        ))
        .add_systems(Update, add_view_camera)
        .add_systems(
            Update,
            (
                swap_view_model,
                layer_meshes,
                animate_view_model,
                show_loaded_skin,
                follow_camera,
                light_view_model,
            )
                .chain()
                .after(crate::camera::CameraPlaced)
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
    /// The same for a SLAM's changes of mode and detonations.
    slam_switches: u8,
    slam_detonations: u8,
    /// Activities to play, in turn, as the one playing ends (a SLAM's
    /// second half of attaching a mine).
    queue: Vec<&'static str>,
    /// A shot charging (or a grenade being let go) has started its
    /// animation.
    charging: bool,
}

impl ViewModel {
    /// The weapon it is the view model of.
    pub(crate) fn weapon(&self) -> WeaponId {
        self.weapon
    }
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
            // The view model is drawn over a clear, transparent frame of its
            // own (HDR, so its glow can bloom), then laid over the world
            // with its alpha, glow included, which adds to the world.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            output_mode: CameraOutputMode::Write {
                blend_state: Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                clear_color: ClearColorConfig::None,
            },
            ..default()
        },
        Hdr,
        view_bloom(),
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
            slam_switches: 0,
            slam_detonations: 0,
            queue: Vec::new(),
            charging: false,
        },
        Moving,
        crate::gravity_gun::Prongs::default(),
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

/// The skin of a view model whose weapon shows a loaded round (the
/// Crossbow's glowing bolt): that skin while a round is in it, or once a
/// reload has put one in, and the first otherwise.
fn loaded_skin(
    loaded: rl_data::LoadedSkinConfig,
    reload: Option<rl_data::Reload>,
    phase: Phase,
    clip: Option<u32>,
) -> usize {
    let reloaded = match (phase, reload) {
        (Phase::Reloading(left), Some(rl_data::Reload::Whole { time })) => {
            time - left >= loaded.shows_at
        }
        _ => false,
    };
    if clip.is_some_and(|clip| clip > 0) || reloaded {
        loaded.skin
    } else {
        0
    }
}

/// Gives the view model the skin its weapon shows now, where it has more
/// than one for a loaded round.
fn show_loaded_skin(
    game: Game,
    local: Option<Res<LocalPlayer>>,
    models: Res<Assets<rl_assets::ModelAsset>>,
    asset_server: Res<AssetServer>,
    mut material_maps: ResMut<MaterialMaps>,
    mut views: Query<(&ViewModel, &mut Prop, Option<&Children>)>,
    mut parts: Query<(&PartSlot, &mut Surface)>,
) {
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        return;
    };
    let Some(inventory) = sim.player(local.id).map(|player| &player.inventory) else {
        return;
    };
    for (view, mut prop, children) in &mut views {
        let Some(config) = sim.arsenal().weapon(view.weapon) else {
            continue;
        };
        let skin = config.loaded_skin.map_or(0, |loaded| {
            loaded_skin(
                loaded,
                config.clip.as_ref().map(|clip| clip.reload),
                inventory.phase(),
                inventory.clip(view.weapon),
            )
        });
        let (true, Some(model), Some(children)) =
            (prop.skin != skin, models.get(&prop.model), children)
        else {
            continue;
        };
        prop.skin = skin;
        for child in children {
            let Ok((slot, mut surface)) = parts.get_mut(*child) else {
                continue;
            };
            if let Some(material) = model.material(skin, slot.0) {
                material_maps.request(material, &asset_server);
                material.clone_into(&mut surface.0);
            }
        }
    }
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

/// A SLAM's own names for the activities every weapon has. It is in one of
/// two modes: *throwing* (a satchel in hand, with the detonator in the other
/// when satchels are out, `detonator`) and *sticking*, held up to a surface
/// to set a mine. `ACT_VM_DRAW_NEXT` is the draw of the next charge after
/// one was thrown, which has the detonator in hand.
fn slam_activity(activity: &'static str, sticking: bool, detonator: bool) -> &'static str {
    match (activity, sticking) {
        ("ACT_VM_DRAW" | "ACT_VM_DRAW_NEXT", true) => "ACT_SLAM_TRIPMINE_DRAW",
        ("ACT_VM_DRAW", false) if detonator => "ACT_SLAM_THROW_DRAW",
        ("ACT_VM_DRAW", false) => "ACT_SLAM_THROW_ND_DRAW",
        ("ACT_VM_DRAW_NEXT", false) => "ACT_SLAM_DETONATOR_THROW_DRAW",
        ("ACT_VM_IDLE", true) if detonator => "ACT_SLAM_STICKWALL_IDLE",
        ("ACT_VM_IDLE", true) => "ACT_SLAM_STICKWALL_ND_IDLE",
        ("ACT_VM_IDLE", false) if detonator => "ACT_SLAM_THROW_IDLE",
        ("ACT_VM_IDLE", false) => "ACT_SLAM_THROW_ND_IDLE",
        ("ACT_VM_PRIMARYATTACK" | "ACT_VM_SECONDARYATTACK", true) => "ACT_SLAM_STICKWALL_ATTACH",
        ("ACT_VM_PRIMARYATTACK" | "ACT_VM_SECONDARYATTACK", false) if detonator => {
            "ACT_SLAM_THROW_THROW"
        }
        ("ACT_VM_PRIMARYATTACK" | "ACT_VM_SECONDARYATTACK", false) => "ACT_SLAM_THROW_THROW_ND",
        _ => activity,
    }
}

/// The activity of a SLAM that changed mode or detonated its satchels since
/// last looked at.
fn slam_event_activity(view: &mut ViewModel, inventory: &Inventory) -> Option<&'static str> {
    let mut wanted = None;
    if inventory.slam_switch_count() != view.slam_switches {
        view.slam_switches = inventory.slam_switch_count();
        view.queue.clear();
        wanted = Some(slam_switch_activity(
            inventory.slam_sticking(),
            inventory.slam_detonator(),
        ));
    }
    if inventory.slam_detonate_count() != view.slam_detonations {
        view.slam_detonations = inventory.slam_detonate_count();
        wanted = Some("ACT_SLAM_THROW_DETONATE");
    }
    wanted
}

/// The activity of a SLAM changing to `sticking` mode (or back to throwing).
fn slam_switch_activity(sticking: bool, detonator: bool) -> &'static str {
    match (sticking, detonator) {
        (true, true) => "ACT_SLAM_THROW_TO_STICKWALL",
        (true, false) => "ACT_SLAM_THROW_TO_STICKWALL_ND",
        (false, true) => "ACT_SLAM_STICKWALL_TO_THROW",
        (false, false) => "ACT_SLAM_STICKWALL_TO_THROW_ND",
    }
}

/// The activity the Gravity Gun plays for what it did: `fire` for a throw,
/// a punt or an empty swing, `altfire` for a grab (and letting go).
fn tool_activity(event: ToolEvent) -> &'static str {
    match event {
        ToolEvent::Grabbed(_) | ToolEvent::Dropped(_) | ToolEvent::GripBroke(_) => {
            "ACT_VM_SECONDARYATTACK"
        }
        ToolEvent::Launched(_) | ToolEvent::Punted(_) | ToolEvent::DryFire => {
            "ACT_VM_PRIMARYATTACK"
        }
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

/// What the view model does once the sequence playing ends: the next of
/// those queued, the next charge being brought up after a throw, or the
/// idle, faded in over the end of the last (a weapon drawn back stays so).
fn follow_up(
    view: &mut ViewModel,
    animated: &Animated,
    player: &mut SequencePlayer,
    act: &impl Fn(&'static str) -> &'static str,
    slam: bool,
) {
    let sequences = &animated.animation.sequences;
    let idle = player.current().is_none() || player.ends_within(sequences, player.fade);
    if !idle || view.primed.is_some() {
        return;
    }
    if !view.queue.is_empty() {
        let activity = view.queue.remove(0);
        if let Some(sequence) = animated.activity(activity) {
            player.restart(sequence, 1.0);
        }
    } else if view.thrown {
        view.thrown = false;
        let next = if slam {
            "ACT_VM_DRAW_NEXT"
        } else {
            "ACT_VM_DRAW"
        };
        if let Some(sequence) = animated.activity(act(next)) {
            player.restart(sequence, 1.0);
        }
    } else if let Some(sequence) = animated.activity(act("ACT_VM_IDLE")) {
        player.play(sequence, 0.0, 1.0);
    }
}

/// Plays the view model's activities as the local player acts.
pub(crate) fn animate_view_model(
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
    let slam = inventory
        .zip(sim.arsenal().weapon(view.weapon))
        .filter(|(_, config)| config.slam)
        .map(|(inventory, _)| (inventory.slam_sticking(), inventory.slam_detonator()));
    // The Gravity Gun, while it holds a prop, idles with its `hold_idle`
    // (the model gives it the activity `ACT_VM_RELOAD`).
    let holding = local_player.is_some_and(|player| player.manipulator.held().is_some());
    let act = |name: &'static str| {
        if holding && name == "ACT_VM_IDLE" {
            return "ACT_VM_RELOAD";
        }
        slam.map_or(name, |(sticking, detonator)| {
            slam_activity(name, sticking, detonator)
        })
    };
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
        if slam.is_some() {
            wanted = slam_event_activity(&mut view, inventory).or(wanted);
        }
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
            // A SLAM brings up its next charge when it is done (a mine goes
            // on in two parts first).
            if let Some((sticking, _)) = slam {
                view.thrown = true;
                if sticking {
                    view.queue = vec!["ACT_SLAM_STICKWALL_ATTACH2"];
                }
            }
        }
        if wanted.is_none()
            && let Some(&(_, event)) = events.tools.iter().rev().find(|&&(id, _)| mine(id))
        {
            wanted = Some(tool_activity(event));
        }
    }
    if let Some(activity) = wanted
        && let Some(sequence) = animated.activity(act(activity))
    {
        if view.primed.is_some() && activity.starts_with("ACT_VM_PULLBACK") {
            player.restart_held(sequence, 1.0);
        } else {
            player.restart(sequence, 1.0);
        }
        return;
    }
    follow_up(&mut view, animated, &mut player, &act, slam.is_some());
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

    #[test]
    fn a_loaded_crossbow_shows_its_glowing_skin() {
        let loaded = rl_data::LoadedSkinConfig {
            skin: 1,
            shows_at: 0.933,
        };
        let reload = Some(rl_data::Reload::Whole { time: 1.833 });
        let skin = |phase, clip| loaded_skin(loaded, reload, phase, clip);
        assert_eq!(skin(Phase::Ready, Some(1)), 1, "a bolt in it");
        assert_eq!(skin(Phase::Ready, Some(0)), 0, "nothing in it");
        // Reloading: dark until the animation puts the bolt in.
        assert_eq!(skin(Phase::Reloading(1.8), Some(0)), 0);
        assert_eq!(skin(Phase::Reloading(1.833 - 0.9), Some(0)), 0);
        assert_eq!(skin(Phase::Reloading(1.833 - 0.95), Some(0)), 1);
        assert_eq!(skin(Phase::Reloading(0.1), Some(0)), 1);
        // A weapon not carried shows nothing.
        assert_eq!(skin(Phase::Ready, None), 0);
    }
}
