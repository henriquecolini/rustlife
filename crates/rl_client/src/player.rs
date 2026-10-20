//! Local play: the player walks the map and handles its props.
//!
//! Once a map with a collision model is loaded, its [`Simulation`] is built
//! and a player is placed at the start spawn. The simulation ticks at the
//! fixed rate from `simulation.ron`, in Bevy's `FixedUpdate`: the player's
//! movement, the Manipulator and prop physics all advance together (see
//! [`rl_sim::simulation`]). Input is sampled every frame: the mouse turns
//! the view immediately, and button presses are latched until the next tick
//! so a tap shorter than a tick is never lost. The camera sits at the
//! player's eyes, interpolated between the last two ticks so motion is
//! smooth at any frame rate.
//!
//! The left mouse button is primary fire, the right one secondary fire: with
//! the Manipulator, right grabs (and drops) a prop and left throws it or
//! punts one.
//!
//! `V` toggles noclip: the free [`crate::camera::FlyCam`] takes over, and
//! switching back drops the player where the camera is. Maps without a
//! collision model are view-only and stay in noclip.
//!
//! The simulation works in Source space; [`rl_assets::coords`] converts at
//! the camera, the one place rendering meets it.

use std::fmt::Write as _;
use std::sync::Arc;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use rl_assets::{MapAsset, coords};
use rl_data::{GameData, MovementConfig};
use rl_sim::collision::CollisionWorld;
use rl_sim::movement::{Buttons, PlayerState, UserCmd};
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::tools::manipulator::ToolEvent;

use crate::AppState;
use crate::camera::{FlyCam, MOUSE_SENSITIVITY};
use crate::script::{self, InputScript};
use crate::world::{MapHandle, StartSpawn};

/// Look-up/down limit, degrees (Source's default `cl_pitchup`/`down`).
const PITCH_LIMIT: f32 = 89.0;
/// Seconds a "hit by a prop" note stays on screen.
const DAMAGE_NOTE_SECS: f32 = 2.0;

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

/// The gameplay data the simulation is built from.
#[derive(Resource, Debug, Clone)]
pub struct Gameplay(pub GameData);

/// The loaded map's simulation.
#[derive(Resource, Debug)]
pub struct Sim(pub Simulation);

/// The locally controlled player: its id and its state after the last two
/// ticks, for interpolating the view between them.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalPlayer {
    /// The player in the [`Sim`].
    pub id: PlayerId,
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

/// The fixed-tick systems that advance the simulation; anything reading
/// its results in `FixedUpdate` runs after this set.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimulationTick;

/// Buttons pressed since the last tick, held for it even if already
/// released.
#[derive(Resource, Debug, Default)]
struct LatchedButtons(Buttons);

/// The last time a prop hurt the player, for the overlay.
#[derive(Resource, Debug, Default)]
struct DamageNote {
    damage: f32,
    at_secs: f32,
}

/// Marks the read-out of the debug overlay.
#[derive(Component)]
struct StatusText;

/// Simulates and views the local player.
pub struct PlayerPlugin {
    /// The gameplay data: movement, physics, tools and tick rate.
    pub data: GameData,
}

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Movement(self.data.movement.clone()))
            .insert_resource(Gameplay(self.data.clone()))
            .insert_resource(Time::<Fixed>::from_hz(f64::from(
                self.data.simulation.tick_rate,
            )))
            .init_resource::<ViewMode>()
            .init_resource::<ViewAngles>()
            .init_resource::<LatchedButtons>()
            .init_resource::<DamageNote>()
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
                tick.in_set(SimulationTick)
                    .run_if(in_state(AppState::InGame))
                    .run_if(resource_equals(ViewMode::Walk)),
            );
    }
}

fn enter_map(mut commands: Commands, mut mode: ResMut<ViewMode>) {
    *mode = ViewMode::Walk;
    commands.remove_resource::<Sim>();
    commands.remove_resource::<LocalPlayer>();
}

fn leave_map(mut commands: Commands) {
    commands.remove_resource::<Sim>();
    commands.remove_resource::<LocalPlayer>();
}

/// Builds the simulation and places the player at the start spawn as soon
/// as the map has loaded. A map without collision switches to noclip for
/// good.
fn spawn_player(
    mut commands: Commands,
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    existing: Option<Res<Sim>>,
    gameplay: Res<Gameplay>,
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
    let world = Arc::new(CollisionWorld::new(collision.as_ref().clone()));
    let mut simulation = Simulation::new(world, &gameplay.0);
    let spawn = (!map.spawns.is_empty()).then(|| map.spawns[start.0 % map.spawns.len()]);
    let (origin, yaw) = spawn.map_or((Vec3::ZERO, 0.0), |spawn| {
        (Vec3::from(spawn.source.origin), spawn.source.yaw)
    });
    let id = simulation.spawn_player(origin + Vec3::Z);
    let Some(state) = simulation.player(id).map(|player| player.state) else {
        return;
    };
    commands.insert_resource(LocalPlayer {
        id,
        previous: state,
        current: state,
    });
    commands.insert_resource(Sim(simulation));
    *angles = ViewAngles { pitch: 0.0, yaw };
    info!("player spawned at {} facing {yaw}°", state.origin);
}

/// `V` switches between walking and noclip, keeping the view where it is.
fn toggle_noclip(
    keys: Res<ButtonInput<KeyCode>>,
    mut mode: ResMut<ViewMode>,
    mut angles: ResMut<ViewAngles>,
    player: Option<ResMut<LocalPlayer>>,
    sim: Option<ResMut<Sim>>,
    movement: Res<Movement>,
    mut camera: Query<(&Transform, &mut FlyCam)>,
) {
    if !keys.just_pressed(KeyCode::KeyV) {
        return;
    }
    let (Some(mut player), Some(mut sim), Ok((transform, mut fly))) =
        (player, sim, camera.single_mut())
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
            let eye = coords::to_source_position(transform.translation);
            let feet = Vec3::from(eye) - Vec3::Z * movement.0.hulls.standing.eye_height;
            if !sim.0.teleport_player(player.id, feet) {
                info!("no room to walk here; staying in noclip");
                return;
            }
            if let Some(state) = sim.0.player(player.id).map(|player| player.state) {
                player.previous = state;
                player.current = state;
            }
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

/// Keys and the buttons they press.
const BUTTON_KEYS: [(KeyCode, Buttons); 5] = [
    (KeyCode::Space, Buttons::JUMP),
    (KeyCode::ControlLeft, Buttons::DUCK),
    (KeyCode::KeyC, Buttons::DUCK),
    (KeyCode::ShiftLeft, Buttons::WALK),
    (KeyCode::KeyE, Buttons::USE),
];

/// Mouse buttons and the buttons they press.
const BUTTON_MOUSE: [(MouseButton, Buttons); 2] = [
    (MouseButton::Left, Buttons::ATTACK),
    (MouseButton::Right, Buttons::ATTACK2),
];

/// Latches button presses until the next tick consumes them.
fn latch_buttons(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    mut latched: ResMut<LatchedButtons>,
) {
    for (key, button) in BUTTON_KEYS {
        if keys.just_pressed(key) {
            latched.0 = latched.0 | button;
        }
    }
    for (key, button) in BUTTON_MOUSE {
        if mouse.as_ref().is_some_and(|mouse| mouse.just_pressed(key)) {
            latched.0 = latched.0 | button;
        }
    }
}

/// The command for this tick from the keys and mouse buttons held now plus
/// latched presses.
fn user_cmd(
    keys: &ButtonInput<KeyCode>,
    mouse: Option<&ButtonInput<MouseButton>>,
    latched: Buttons,
    angles: ViewAngles,
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
    for (key, button) in BUTTON_MOUSE {
        if mouse.is_some_and(|mouse| mouse.pressed(key)) {
            buttons = buttons | button;
        }
    }
    UserCmd {
        forward: axis(KeyCode::KeyW, KeyCode::KeyS),
        side: axis(KeyCode::KeyD, KeyCode::KeyA),
        pitch: angles.pitch,
        yaw: angles.yaw,
        buttons,
    }
}

/// Advances the simulation by one fixed tick with the local player's
/// command (adjusted by the input script, if any).
fn tick(
    clock: Res<Time<Virtual>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    mut latched: ResMut<LatchedButtons>,
    mut angles: ResMut<ViewAngles>,
    sim: Option<ResMut<Sim>>,
    player: Option<ResMut<LocalPlayer>>,
    script: Option<ResMut<InputScript>>,
    mut note: ResMut<DamageNote>,
) {
    let (Some(mut sim), Some(mut player)) = (sim, player) else {
        return;
    };
    let mut cmd = user_cmd(&keys, mouse.as_deref(), latched.0, *angles);
    latched.0 = Buttons::NONE;
    if let Some(mut script) = script {
        script::apply(
            &mut script,
            clock.elapsed_secs(),
            player.id,
            &mut sim.0,
            &mut cmd,
        );
        *angles = ViewAngles {
            pitch: cmd.pitch,
            yaw: cmd.yaw,
        };
    }
    let events = sim.0.tick(&[(player.id, cmd)]);
    for (id, impact) in &events.impacts {
        if *id == player.id {
            info!(
                "hit by a prop at {:.0} units/s: {:.0} damage",
                impact.speed, impact.damage
            );
            *note = DamageNote {
                damage: impact.damage,
                at_secs: clock.elapsed_secs(),
            };
        }
    }
    for (_, event) in &events.tools {
        if !matches!(event, ToolEvent::DryFire) {
            debug!("manipulator: {event:?}");
        }
    }
    if let Some(state) = sim.0.player(player.id).map(|player| player.state) {
        player.previous = player.current;
        player.current = state;
    }
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

/// A crosshair and a status read-out, until the real HUD arrives (M5).
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
        StatusText,
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
    clock: Res<Time<Virtual>>,
    mode: Res<ViewMode>,
    player: Option<Res<LocalPlayer>>,
    sim: Option<Res<Sim>>,
    note: Res<DamageNote>,
    mut text: Query<&mut Text, With<StatusText>>,
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
            let held = sim.as_ref().and_then(|sim| {
                let body = sim.0.player(player.id)?.manipulator.held()?;
                sim.0.physics().mass(body)
            });
            if let Some(mass) = held {
                let _ = write!(line, "  holding {mass:.0} kg");
            }
            if note.damage > 0.0 && clock.elapsed_secs() - note.at_secs < DAMAGE_NOTE_SECS {
                let _ = write!(line, "  hit: {:.0} damage", note.damage);
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
    fn commands_combine_held_keys_mouse_and_latched_presses() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::ShiftLeft);
        let mut mouse = ButtonInput::<MouseButton>::default();
        mouse.press(MouseButton::Right);
        let cmd = user_cmd(&keys, Some(&mouse), Buttons::JUMP, ViewAngles::default());
        assert_eq!((cmd.forward, cmd.side), (1.0, -1.0));
        assert!(
            cmd.buttons
                .contains(Buttons::JUMP | Buttons::WALK | Buttons::ATTACK2)
        );
        assert!(!cmd.buttons.contains(Buttons::DUCK));
        assert!(!cmd.buttons.contains(Buttons::ATTACK));
    }
}
