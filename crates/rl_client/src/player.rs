//! Local play: the player walks the map with rustlife's movement.
//!
//! Once a map with a collision model is loaded, a player is placed at the
//! start spawn and simulated by [`rl_sim::movement::simulate`] at the fixed
//! tick rate from `simulation.ron`, in Bevy's `FixedUpdate`. Input is sampled
//! every frame: the mouse turns the view immediately, and button presses are
//! latched until the next tick so a tap shorter than a tick is never lost.
//! The camera sits at the player's eyes, interpolated between the last two
//! ticks so motion is smooth at any frame rate.
//!
//! `V` toggles noclip: the free [`crate::camera::FlyCam`] takes over, and
//! switching back drops the player where the camera is. Maps without a
//! collision model are view-only and stay in noclip.
//!
//! The simulation works in Source space; [`rl_assets::coords`] converts at
//! the camera, the one place rendering meets it.

use std::sync::Arc;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use rl_assets::{MapAsset, coords};
use rl_data::{GameData, MovementConfig};
use rl_sim::collision::{CollisionWorld, Hull, find_free_position};
use rl_sim::movement::{Buttons, PlayerState, UserCmd, simulate};

use crate::AppState;
use crate::camera::{FlyCam, MOUSE_SENSITIVITY};
use crate::world::{MapHandle, StartSpawn};

/// Look-up/down limit, degrees (Source's default `cl_pitchup`/`down`).
const PITCH_LIMIT: f32 = 89.0;

/// How the camera moves.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// As the simulated player.
    #[default]
    Walk,
    /// As a free-flying camera that ignores collision.
    Noclip,
}

/// The movement tuning in use.
#[derive(Resource, Debug, Clone)]
pub struct Movement(pub MovementConfig);

/// The loaded map's collision world, shared with the simulation.
#[derive(Resource, Clone)]
pub struct SimWorld(pub Arc<CollisionWorld>);

/// The locally simulated player: its state after the last two ticks, for
/// interpolating the view between them.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalPlayer {
    /// State after the previous tick.
    pub previous: PlayerState,
    /// State after the latest tick.
    pub current: PlayerState,
}

/// Where the player is looking, Source convention: degrees, positive pitch
/// looks down, yaw 0 faces +X.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq)]
pub struct ViewAngles {
    /// Pitch, degrees.
    pub pitch: f32,
    /// Yaw, degrees.
    pub yaw: f32,
}

/// Buttons pressed since the last tick, held for it even if already
/// released.
#[derive(Resource, Debug, Default)]
struct LatchedButtons(Buttons);

/// Scripted input for non-interactive runs: hold forward between two
/// moments of app time, so a capture can be timed to land mid-move.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ScriptedForward {
    /// App time to start walking, seconds.
    pub start_secs: f32,
    /// How long to walk, seconds.
    pub duration_secs: f32,
}

/// Marks the speed read-out of the debug overlay.
#[derive(Component)]
struct SpeedText;

/// Simulates and views the local player.
pub struct PlayerPlugin {
    /// The gameplay data: movement tuning and tick rate.
    pub data: GameData,
}

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Movement(self.data.movement.clone()))
            .insert_resource(Time::<Fixed>::from_hz(f64::from(
                self.data.simulation.tick_rate,
            )))
            .init_resource::<ViewMode>()
            .init_resource::<ViewAngles>()
            .init_resource::<LatchedButtons>()
            .add_systems(OnEnter(AppState::InGame), (enter_map, spawn_overlay))
            .add_systems(OnExit(AppState::InGame), leave_map)
            .add_systems(
                Update,
                (
                    spawn_player,
                    toggle_noclip,
                    (look, latch_buttons, place_camera)
                        .chain()
                        .run_if(resource_equals(ViewMode::Walk)),
                    update_overlay,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                FixedUpdate,
                tick_player
                    .run_if(in_state(AppState::InGame))
                    .run_if(resource_equals(ViewMode::Walk)),
            );
    }
}

fn enter_map(mut commands: Commands, mut mode: ResMut<ViewMode>) {
    *mode = ViewMode::Walk;
    commands.remove_resource::<SimWorld>();
    commands.remove_resource::<LocalPlayer>();
}

fn leave_map(mut commands: Commands) {
    commands.remove_resource::<SimWorld>();
    commands.remove_resource::<LocalPlayer>();
}

/// Builds the collision world and places the player at the start spawn as
/// soon as the map has loaded. A map without collision switches to noclip
/// for good.
fn spawn_player(
    mut commands: Commands,
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    existing: Option<Res<SimWorld>>,
    movement: Res<Movement>,
    start: Res<StartSpawn>,
    mut angles: ResMut<ViewAngles>,
    mut mode: ResMut<ViewMode>,
) {
    if existing.is_some() || *mode == ViewMode::Noclip {
        return;
    }
    let Some(map) = handle.and_then(|handle| maps.get(&handle.0)) else {
        return;
    };
    let Some(collision) = &map.collision else {
        warn!("the map has no collision model; flying in noclip");
        *mode = ViewMode::Noclip;
        return;
    };
    let world = CollisionWorld::new(collision.as_ref().clone());
    let hull = Hull::from(&movement.0.hulls.standing);
    let spawn = (!map.spawns.is_empty()).then(|| map.spawns[start.0 % map.spawns.len()]);
    let (origin, yaw) = spawn.map_or((Vec3::ZERO, 0.0), |spawn| {
        (Vec3::from(spawn.source.origin), spawn.source.yaw)
    });
    // Mappers sometimes leave spawns touching a wall; nudge the player
    // free, as the engine does.
    let origin = find_free_position(&world, origin + Vec3::Z, &hull).unwrap_or(origin);
    let state = PlayerState::at(origin);
    commands.insert_resource(LocalPlayer {
        previous: state,
        current: state,
    });
    commands.insert_resource(SimWorld(Arc::new(world)));
    *angles = ViewAngles { pitch: 0.0, yaw };
    info!("player spawned at {origin} facing {yaw}°");
}

/// `V` switches between walking and noclip, keeping the view where it is.
fn toggle_noclip(
    keys: Res<ButtonInput<KeyCode>>,
    mut mode: ResMut<ViewMode>,
    mut angles: ResMut<ViewAngles>,
    player: Option<ResMut<LocalPlayer>>,
    world: Option<Res<SimWorld>>,
    movement: Res<Movement>,
    mut camera: Query<(&Transform, &mut FlyCam)>,
) {
    if !keys.just_pressed(KeyCode::KeyV) {
        return;
    }
    let (Some(mut player), Some(world), Ok((transform, mut fly))) =
        (player, world, camera.single_mut())
    else {
        return;
    };
    match *mode {
        ViewMode::Walk => {
            fly.yaw = coords::yaw(angles.yaw);
            fly.pitch = -angles.pitch.to_radians();
            *mode = ViewMode::Noclip;
        }
        ViewMode::Noclip => {
            let config = &movement.0;
            let eye = coords::to_source_position(transform.translation);
            let feet = Vec3::from(eye) - Vec3::Z * config.hulls.standing.eye_height;
            let hull = Hull::from(&config.hulls.standing);
            let Some(origin) = find_free_position(world.0.as_ref(), feet, &hull) else {
                info!("no room to walk here; staying in noclip");
                return;
            };
            let state = PlayerState::at(origin);
            *player = LocalPlayer {
                previous: state,
                current: state,
            };
            *angles = ViewAngles {
                pitch: -fly.pitch.to_degrees(),
                yaw: (fly.yaw + std::f32::consts::FRAC_PI_2).to_degrees(),
            };
            *mode = ViewMode::Walk;
        }
    }
}

/// Turns the view with the mouse while the cursor is grabbed.
fn look(
    mouse: Res<AccumulatedMouseMotion>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    mut angles: ResMut<ViewAngles>,
) {
    let grabbed = cursor
        .single()
        .is_ok_and(|options| options.grab_mode != CursorGrabMode::None);
    if !grabbed || mouse.delta == Vec2::ZERO {
        return;
    }
    let degrees_per_pixel = MOUSE_SENSITIVITY.to_degrees();
    angles.yaw = (angles.yaw - mouse.delta.x * degrees_per_pixel).rem_euclid(360.0);
    angles.pitch =
        (angles.pitch + mouse.delta.y * degrees_per_pixel).clamp(-PITCH_LIMIT, PITCH_LIMIT);
}

/// Latches button presses until the next tick consumes them.
fn latch_buttons(keys: Res<ButtonInput<KeyCode>>, mut latched: ResMut<LatchedButtons>) {
    for (key, button) in BUTTON_KEYS {
        if keys.just_pressed(key) {
            latched.0 = latched.0 | button;
        }
    }
}

/// Keys and the buttons they press.
const BUTTON_KEYS: [(KeyCode, Buttons); 5] = [
    (KeyCode::Space, Buttons::JUMP),
    (KeyCode::ControlLeft, Buttons::DUCK),
    (KeyCode::KeyC, Buttons::DUCK),
    (KeyCode::ShiftLeft, Buttons::WALK),
    (KeyCode::KeyE, Buttons::USE),
];

/// The command for this tick from the keys held now plus latched presses.
fn user_cmd(
    keys: &ButtonInput<KeyCode>,
    latched: Buttons,
    angles: ViewAngles,
    scripted_forward: bool,
) -> UserCmd {
    let axis = |positive: KeyCode, negative: KeyCode| {
        f32::from(u8::from(keys.pressed(positive))) - f32::from(u8::from(keys.pressed(negative)))
    };
    let mut buttons = latched;
    for (key, button) in BUTTON_KEYS {
        if keys.pressed(key) {
            buttons = buttons | button;
        }
    }
    let forward = if scripted_forward {
        1.0
    } else {
        axis(KeyCode::KeyW, KeyCode::KeyS)
    };
    UserCmd {
        forward,
        side: axis(KeyCode::KeyD, KeyCode::KeyA),
        pitch: angles.pitch,
        yaw: angles.yaw,
        buttons,
    }
}

/// Advances the player by one fixed tick.
fn tick_player(
    time: Res<Time>,
    clock: Res<Time<Virtual>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut latched: ResMut<LatchedButtons>,
    angles: Res<ViewAngles>,
    movement: Res<Movement>,
    world: Option<Res<SimWorld>>,
    player: Option<ResMut<LocalPlayer>>,
    scripted: Option<Res<ScriptedForward>>,
) {
    let (Some(world), Some(mut player)) = (world, player) else {
        return;
    };
    let dt = time.delta_secs();
    let now = clock.elapsed_secs();
    let scripted_forward = scripted.is_some_and(|script| {
        (script.start_secs..script.start_secs + script.duration_secs).contains(&now)
    });
    let cmd = user_cmd(&keys, latched.0, *angles, scripted_forward);
    latched.0 = Buttons::NONE;
    let (next, _events) = simulate(&player.current, &cmd, &movement.0, dt, world.0.as_ref());
    player.previous = player.current;
    player.current = next;
}

/// Puts the camera at the player's eyes, interpolated between the last two
/// ticks by how far the frame is into the next one.
fn place_camera(
    time: Res<Time<Fixed>>,
    player: Option<Res<LocalPlayer>>,
    angles: Res<ViewAngles>,
    movement: Res<Movement>,
    mut camera: Query<&mut Transform, With<FlyCam>>,
) {
    let (Some(player), Ok(mut transform)) = (player, camera.single_mut()) else {
        return;
    };
    let alpha = time.overstep_fraction();
    let config = &movement.0;
    let eye = |state: &PlayerState| state.origin + Vec3::Z * state.eye_height(config);
    let position = eye(&player.previous).lerp(eye(&player.current), alpha);
    transform.translation = coords::position(position.to_array());
    transform.rotation = view_rotation(*angles);
}

/// The camera rotation for Source view angles.
fn view_rotation(angles: ViewAngles) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        coords::yaw(angles.yaw),
        -angles.pitch.to_radians(),
        0.0,
    )
}

/// A crosshair and a speed read-out, until the real HUD arrives (M5).
fn spawn_overlay(mut commands: Commands) {
    commands.spawn((
        DespawnOnExit(AppState::InGame),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(50.0),
            top: Val::Percent(50.0),
            width: Val::Px(4.0),
            height: Val::Px(4.0),
            margin: UiRect {
                left: Val::Px(-2.0),
                top: Val::Px(-2.0),
                ..default()
            },
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.8)),
    ));
    commands.spawn((
        DespawnOnExit(AppState::InGame),
        SpeedText,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..default()
        },
        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.85)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(12.0),
            bottom: Val::Px(10.0),
            ..default()
        },
    ));
}

fn update_overlay(
    mode: Res<ViewMode>,
    player: Option<Res<LocalPlayer>>,
    mut text: Query<&mut Text, With<SpeedText>>,
) {
    let Ok(mut text) = text.single_mut() else {
        return;
    };
    let line = match (*mode, player) {
        (ViewMode::Walk, Some(player)) => {
            let state = &player.current;
            let mut line = format!("{:.0} u/s", state.speed_2d());
            if state.ducked {
                line.push_str("  crouched");
            }
            if state.ladder.is_some() {
                line.push_str("  ladder");
            }
            line
        }
        _ => "noclip  (V to walk)".to_owned(),
    };
    if text.0 != line {
        text.0 = line;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_rotation_matches_source_angles() {
        // Yaw 0 looks along Source +X, which is Bevy +X.
        let forward = view_rotation(ViewAngles::default()) * Vec3::NEG_Z;
        assert!(forward.abs_diff_eq(Vec3::X, 1e-5), "{forward}");
        // Positive pitch looks down.
        let down = view_rotation(ViewAngles {
            pitch: 90.0,
            yaw: 0.0,
        }) * Vec3::NEG_Z;
        assert!(down.abs_diff_eq(Vec3::NEG_Y, 1e-5), "{down}");
        // Yaw 90 faces Source +Y (left), which is Bevy -Z.
        let left = view_rotation(ViewAngles {
            pitch: 0.0,
            yaw: 90.0,
        }) * Vec3::NEG_Z;
        assert!(left.abs_diff_eq(Vec3::NEG_Z, 1e-5), "{left}");
    }

    #[test]
    fn commands_combine_held_keys_and_latched_presses() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::ShiftLeft);
        let cmd = user_cmd(&keys, Buttons::JUMP, ViewAngles::default(), false);
        assert_eq!((cmd.forward, cmd.side), (1.0, -1.0));
        assert!(cmd.buttons.contains(Buttons::JUMP | Buttons::WALK));
        assert!(!cmd.buttons.contains(Buttons::DUCK));
    }
}
