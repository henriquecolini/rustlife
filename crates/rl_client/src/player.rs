//! The local player: input in, view out.
//!
//! The game runs on a server (see [`crate::net`]); this module is the
//! player's end of it. Each fixed tick, at the data's tick rate in Bevy's
//! `FixedUpdate`, the player's command is built from the input
//! ([`crate::input`]) — adjusted by the input script, if any — and handed
//! to the connection, which numbers it, runs it at once for the local
//! player (prediction) and sends it. What the prediction fired is shown
//! straight away as a [`Ticked`] message, like the events the server sends.
//!
//! Input is sampled every frame: the mouse turns the view immediately, and
//! presses are latched until the next tick. The camera sits at the
//! predicted player's eyes, interpolated between the last two ticks so
//! motion is smooth at any frame rate, shifted by what is left of the last
//! prediction correction so corrections glide rather than snap; while dead
//! it drops to the floor where the player fell.
//!
//! `V` toggles noclip: the free [`crate::camera::FlyCam`] takes over. In a
//! game this client hosts, switching back puts the player where the camera
//! is. Maps without a collision model are view-only and stay in noclip.
//!
//! The simulation works in Source space; [`rl_assets::coords`] converts at
//! the camera, the one place rendering meets it.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use rl_assets::coords;
use rl_data::{GameData, MovementConfig};
use rl_sim::movement::{Buttons, PlayerState};
use rl_sim::simulation::{MapEvent, PlayerId, Prediction, Simulation, Spawn, TickEvents};
use rl_sim::weapons::WeaponId;

use crate::AppState;
use crate::camera::FlyCam;
use crate::input::{Latched, idle_cmd, latch_input, user_cmd};
use crate::menu::Menu;
use crate::net::{Connection, Game, Host, NetStatus, Receive};
use crate::script::{self, InputScript};
use crate::selector::WeaponSelector;
use crate::settings::{Bindings, RADIANS_PER_PIXEL, Settings};
use crate::world::StartSpawn;
use crate::zoom::Zoom;

/// Look-up/down limit, degrees (Source's default `cl_pitchup`/`down`).
const PITCH_LIMIT: f32 = 89.0;
/// Height of the camera above a dead player's feet, units.
const DEAD_EYE_HEIGHT: f32 = 12.0;

/// How the camera moves.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// As the simulated player.
    #[default]
    Walk,
    /// As a free-flying camera that ignores collision.
    Noclip,
}

/// The movement tuning in use (the server's, once in a game).
#[derive(Resource, Debug, Clone)]
pub struct Movement(pub MovementConfig);

/// The gameplay data in use: the client's own until a server sends its
/// own.
#[derive(Resource, Debug, Clone)]
pub struct Gameplay(pub GameData);

/// The local player: their id and their predicted state after the last
/// two ticks, for interpolating the view between them.
#[derive(Resource, Debug, Clone, Copy)]
pub struct LocalPlayer {
    /// The player in the game.
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

/// What happened: one server tick's events as the client heard them, or
/// what the local player's prediction fired.
#[derive(Message, Debug, Clone)]
pub struct Ticked(pub TickEvents);

/// The fixed-tick systems that issue the local player's command; anything
/// reading its results in `FixedUpdate` runs after this set.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimulationTick;

/// Moves and views the local player.
pub struct PlayerPlugin {
    /// The client's gameplay data: movement, weapons, rules, tick rate.
    pub data: GameData,
}

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Movement(self.data.movement.clone()))
            .insert_resource(Gameplay(self.data.clone()))
            .insert_resource(Time::<Fixed>::from_hz(f64::from(
                self.data.simulation.tick_rate,
            )))
            .add_message::<Ticked>()
            .init_resource::<ViewMode>()
            .init_resource::<ViewAngles>()
            .init_resource::<Latched>()
            .init_resource::<WeaponSelector>()
            .add_systems(OnEnter(AppState::InGame), enter_map)
            .add_systems(OnExit(AppState::InGame), leave_map)
            .add_systems(
                PreUpdate,
                follow_corrections
                    .after(Receive)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                (
                    adopt_player,
                    view_only_without_a_game,
                    face_respawns,
                    toggle_noclip,
                    (
                        look,
                        latch_input.run_if(menu_closed),
                        place_camera,
                        crate::camera::settle_camera,
                    )
                        .chain()
                        .in_set(crate::camera::CameraPlaced)
                        .run_if(resource_equals(ViewMode::Walk)),
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

fn enter_map(
    mut commands: Commands,
    mut mode: ResMut<ViewMode>,
    mut selector: ResMut<WeaponSelector>,
) {
    *mode = ViewMode::Walk;
    *selector = WeaponSelector::default();
    commands.remove_resource::<LocalPlayer>();
}

fn leave_map(mut commands: Commands) {
    commands.remove_resource::<LocalPlayer>();
}

/// Whether no menu is up: the player is playing.
fn menu_closed(menu: Res<Menu>) -> bool {
    !menu.is_open()
}

/// Once the server has put the local player in the game, follows them: the
/// view turns to where they face. In a game this client hosts, a start
/// spawn asked for (`--spawn`) moves them there first.
fn adopt_player(
    mut commands: Commands,
    game: Game,
    existing: Option<Res<LocalPlayer>>,
    host: Option<ResMut<Host>>,
    start: Res<StartSpawn>,
    mut angles: ResMut<ViewAngles>,
) {
    let (None, Some(id)) = (existing, game.local()) else {
        return;
    };
    let Some(sim) = game.sim() else {
        return;
    };
    let mut yaw = sim.player(id).map_or(0.0, |player| player.look.1);
    if let Some(mut host) = host
        && let Some(spawn) = start.0
    {
        let sim = host.0.sim_mut();
        if let Some(chosen) = start_spawn(sim, spawn) {
            sim.teleport_player(id, chosen.origin + Vec3::Z);
            yaw = chosen.yaw;
        }
    }
    let Some(state) = sim.player(id).map(|player| player.state) else {
        return;
    };
    commands.insert_resource(LocalPlayer {
        id,
        previous: state,
        current: state,
    });
    *angles = ViewAngles { pitch: 0.0, yaw };
    info!("playing as {id:?} from {}", state.origin);
}

/// The spawn point at `index` of `sim`'s, wrapping around.
fn start_spawn(sim: &Simulation, index: usize) -> Option<Spawn> {
    let spawns = sim.spawns();
    (!spawns.is_empty()).then(|| spawns[index % spawns.len()])
}

/// A map that could not be hosted (one without collision) is flown in
/// noclip.
fn view_only_without_a_game(
    status: Res<NetStatus>,
    connection: Option<Res<Connection>>,
    mut mode: ResMut<ViewMode>,
) {
    if connection.is_none() && matches!(*status, NetStatus::Failed(_)) && *mode == ViewMode::Walk {
        warn!("no game on this map; flying in noclip");
        *mode = ViewMode::Noclip;
    }
}

/// Turns the view to where the local player respawned, or the way a
/// teleport's destination faces (keeping the pitch).
fn face_respawns(
    mut ticks: MessageReader<Ticked>,
    local: Option<Res<LocalPlayer>>,
    mut angles: ResMut<ViewAngles>,
) {
    let Some(local) = local else {
        ticks.clear();
        return;
    };
    for Ticked(events) in ticks.read() {
        if let Some(respawn) = events
            .respawns
            .iter()
            .find(|respawn| respawn.player == local.id)
        {
            *angles = ViewAngles {
                pitch: 0.0,
                yaw: respawn.spawn.yaw,
            };
        }
        let teleported = events.map.iter().find_map(|event| match *event {
            MapEvent::Teleported { player, spawn } if player == local.id => Some(spawn.yaw),
            _ => None,
        });
        if let Some(yaw) = teleported {
            angles.yaw = yaw;
        }
    }
}

/// After a snapshot moved the predicted player (a correction), moves the
/// interpolated view's two ends with it; the correction's error, added at
/// the camera, keeps the view where it was and lets it glide over.
fn follow_corrections(game: Game, local: Option<ResMut<LocalPlayer>>) {
    let (Some(mut local), Some(sim)) = (local, game.sim()) else {
        return;
    };
    let Some(state) = sim.player(local.id).map(|player| player.state) else {
        return;
    };
    let moved = state.origin - local.current.origin;
    if moved != Vec3::ZERO {
        local.previous.origin += moved;
        local.current = state;
    }
}

/// `V` switches between walking and noclip, keeping the view where it is.
/// In a hosted game, walking again puts the player where the camera is.
fn toggle_noclip(
    keys: Res<ButtonInput<KeyCode>>,
    menu: Res<Menu>,
    mut mode: ResMut<ViewMode>,
    mut angles: ResMut<ViewAngles>,
    player: Option<Res<LocalPlayer>>,
    host: Option<ResMut<Host>>,
    movement: Res<Movement>,
    mut camera: Query<(&Transform, &mut FlyCam)>,
) {
    if !keys.just_pressed(KeyCode::KeyV) || menu.is_open() {
        return;
    }
    let (Some(player), Ok((transform, mut fly))) = (player, camera.single_mut()) else {
        return;
    };
    match *mode {
        ViewMode::Walk => {
            fly.yaw = coords::yaw(angles.yaw);
            fly.pitch = -angles.pitch.to_radians();
            *mode = ViewMode::Noclip;
        }
        ViewMode::Noclip => {
            if let Some(mut host) = host {
                let eye = coords::to_source_position(transform.translation);
                let feet = Vec3::from(eye) - Vec3::Z * movement.0.hulls.standing.eye_height;
                if !host.0.sim_mut().teleport_player(player.id, feet) {
                    info!("no room to walk here; staying in noclip");
                    return;
                }
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
    settings: Res<Settings>,
    zoom: Option<Res<Zoom>>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    mut angles: ResMut<ViewAngles>,
) {
    let grabbed = cursor
        .single()
        .is_ok_and(|options| options.grab_mode != CursorGrabMode::None);
    if !grabbed || mouse.delta == Vec2::ZERO {
        return;
    }
    let degrees_per_pixel = (RADIANS_PER_PIXEL * settings.controls.sensitivity).to_degrees()
        * zoom.map_or(1.0, |zoom| zoom.sensitivity);
    let invert = if settings.controls.invert_y {
        -1.0
    } else {
        1.0
    };
    angles.yaw = (angles.yaw - mouse.delta.x * degrees_per_pixel).rem_euclid(360.0);
    angles.pitch = (angles.pitch + mouse.delta.y * degrees_per_pixel * invert)
        .clamp(-PITCH_LIMIT, PITCH_LIMIT);
}

/// Issues the local player's command for this tick (adjusted by the input
/// script, which acts on the hosted server's simulation), shows what its
/// prediction fired, and records the predicted state for the view.
fn tick(
    clock: Res<Time<Virtual>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    bindings: Res<Bindings>,
    menu: Res<Menu>,
    mut latched: ResMut<Latched>,
    mut selector: ResMut<WeaponSelector>,
    mut angles: ResMut<ViewAngles>,
    connection: Option<ResMut<Connection>>,
    mut host: Option<ResMut<Host>>,
    player: Option<ResMut<LocalPlayer>>,
    script: Option<ResMut<InputScript>>,
    mut ticked: MessageWriter<Ticked>,
) {
    let (Some(mut connection), Some(mut player)) = (connection, player) else {
        return;
    };
    let Some(session) = connection.0.session() else {
        return;
    };
    let mut cmd = if menu.is_open() {
        idle_cmd(*angles)
    } else {
        user_cmd(&keys, mouse.as_deref(), &bindings, latched.buttons, *angles)
    };
    let scroll = scroll.map_or(0.0, |scroll| scroll.delta.y);
    let fire = ["attack", "attack2"];
    let (fire_held, fire_pressed) = (
        fire.iter()
            .any(|action| bindings.pressed(action, &keys, mouse.as_deref())),
        fire.iter()
            .any(|action| bindings.just_pressed(action, &keys, mouse.as_deref(), scroll)),
    );
    if selector.swallows_fire(fire_held, fire_pressed) {
        cmd.buttons = cmd
            .buttons
            .with(Buttons::ATTACK, false)
            .with(Buttons::ATTACK2, false);
    }
    if let (Some(choice), Some(local)) = (latched.choice, session.sim().player(player.id)) {
        cmd.select = local.inventory.choose(session.sim().arsenal(), choice);
    }
    *latched = Latched::default();
    if let Some(mut script) = script {
        let now = clock.elapsed_secs();
        match host.as_mut() {
            Some(host) => script::apply(&mut script, now, player.id, host.0.sim_mut(), &mut cmd),
            None => script::apply_input(&mut script, now, &mut cmd),
        }
        *angles = ViewAngles {
            pitch: cmd.pitch,
            yaw: cmd.yaw,
        };
    }
    let Some(prediction) = connection.0.tick(cmd) else {
        return;
    };
    if let Some(host) = &mut host {
        host.0.exchange(&mut connection.0);
    }
    let active = connection
        .0
        .session()
        .and_then(|session| session.sim().player(player.id))
        .and_then(|player| player.inventory.active());
    ticked.write(Ticked(predicted_events(player.id, active, &prediction)));
    let state = connection
        .0
        .session()
        .and_then(|session| session.sim().player(player.id))
        .map(|player| player.state);
    if let Some(state) = state {
        player.previous = player.current;
        player.current = state;
    }
}

/// What the local player's prediction fired, as tick events.
fn predicted_events(
    player: PlayerId,
    active: Option<WeaponId>,
    prediction: &Prediction,
) -> TickEvents {
    let with_active = |happened: bool| {
        active
            .filter(|_| happened)
            .map(|weapon| (player, weapon))
            .into_iter()
            .collect()
    };
    TickEvents {
        fired: prediction
            .weapon
            .fired
            .map(|(weapon, mode)| (player, weapon, mode))
            .into_iter()
            .collect(),
        reloads: with_active(prediction.weapon.reloading),
        deploys: with_active(prediction.weapon.switched),
        noises: active
            .zip(prediction.weapon.noise)
            .map(|(weapon, noise)| (player, weapon, noise))
            .into_iter()
            .collect(),
        shots: prediction.shots.clone(),
        moves: vec![(player, prediction.moved)],
        ..TickEvents::default()
    }
}

/// Puts the camera at the predicted player's eyes, interpolated between the
/// last two ticks by how far the frame is into the next one and shifted by
/// what is left of the last correction; low to the floor while dead.
fn place_camera(
    time: Res<Time<Fixed>>,
    player: Option<Res<LocalPlayer>>,
    game: Game,
    angles: Res<ViewAngles>,
    movement: Res<Movement>,
    shake: Option<Res<crate::particles::Shake>>,
    punch: Option<Res<crate::view_punch::ViewPunch>>,
    clock: Res<Time>,
    mut camera: Query<&mut Transform, With<FlyCam>>,
) {
    let (Some(player), Ok(mut transform)) = (player, camera.single_mut()) else {
        return;
    };
    let alive = game
        .sim()
        .and_then(|sim| sim.player(player.id))
        .is_none_or(rl_sim::simulation::Player::alive);
    let error = game
        .session()
        .map_or(Vec3::ZERO, rl_net::Session::prediction_error);
    let alpha = time.overstep_fraction();
    let config = &movement.0;
    let eye = |state: &PlayerState| {
        let height = if alive {
            state.eye_height(config)
        } else {
            DEAD_EYE_HEIGHT
        };
        state.origin + Vec3::Z * height
    };
    let position = eye(&player.previous).lerp(eye(&player.current), alpha) + error;
    transform.translation = coords::position(position.to_array());
    // The view is kicked by a punch (the Gravity Gun's) without the aim
    // moving: only what is drawn turns.
    let (punch_pitch, punch_yaw) = punch.map_or((0.0, 0.0), |punch| punch.pitch_yaw());
    let punched = ViewAngles {
        pitch: angles.pitch + punch_pitch,
        yaw: angles.yaw + punch_yaw,
    };
    transform.rotation = view_rotation(punched)
        * shake.map_or(Quat::IDENTITY, |shake| shake.rotation(clock.elapsed_secs()));
}

/// The camera rotation for Source view angles.
pub(crate) fn view_rotation(angles: ViewAngles) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        coords::yaw(angles.yaw),
        -angles.pitch.to_radians(),
        0.0,
    )
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
    fn predicted_shots_become_tick_events() {
        let player = PlayerId::from_index(2);
        let weapon = WeaponId::from_index(1);
        let prediction = Prediction {
            weapon: rl_sim::weapons::WeaponTick {
                fired: Some((weapon, rl_sim::weapons::Mode::Primary)),
                ..Default::default()
            },
            ..Prediction::default()
        };
        let events = predicted_events(player, Some(weapon), &prediction);
        assert_eq!(
            events.fired,
            [(player, weapon, rl_sim::weapons::Mode::Primary)]
        );
        assert!(events.shots.is_empty());
    }
}
