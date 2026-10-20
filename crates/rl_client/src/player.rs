//! Local play: the player fights through the map.
//!
//! Once a map with a collision model is loaded, its [`Simulation`] is built
//! — with the map's spawn points, and its weapon and item entities turned
//! into pickups — and the player is placed at the start spawn, joined by
//! any training dummies asked for (players who stand still and respawn).
//! The simulation ticks at the fixed rate from `simulation.ron`, in Bevy's
//! `FixedUpdate`: movement, weapons, the Manipulator, prop physics, pickups
//! and the match all advance together (see [`rl_sim::simulation`]). What
//! each tick did is sent on as a [`Ticked`] message, for the HUD and
//! effects.
//!
//! Input is sampled every frame ([`crate::input`]): the mouse turns the view
//! immediately, and presses are latched until the next tick. The camera
//! sits at the player's eyes, interpolated between the last two ticks so
//! motion is smooth at any frame rate; while dead it drops to the floor
//! where the player fell.
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
use rl_sim::collision::CollisionWorld;
use rl_sim::movement::PlayerState;
use rl_sim::simulation::{PlayerId, Simulation, Spawn, TickEvents};

use crate::AppState;
use crate::camera::{FlyCam, MOUSE_SENSITIVITY};
use crate::input::{Latched, latch_input, user_cmd};
use crate::script::{self, InputScript};
use crate::world::{MapHandle, StartSpawn};

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

/// How many training dummies join the local player.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct Dummies(pub usize);

/// What one simulation tick did.
#[derive(Message, Debug, Clone)]
pub struct Ticked(pub TickEvents);

/// The fixed-tick systems that advance the simulation; anything reading
/// its results in `FixedUpdate` runs after this set.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct SimulationTick;

/// Simulates and views the local player.
pub struct PlayerPlugin {
    /// The gameplay data: movement, weapons, rules, tick rate.
    pub data: GameData,
    /// Training dummies to add.
    pub dummies: usize,
}

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Movement(self.data.movement.clone()))
            .insert_resource(Gameplay(self.data.clone()))
            .insert_resource(Dummies(self.dummies))
            .insert_resource(Time::<Fixed>::from_hz(f64::from(
                self.data.simulation.tick_rate,
            )))
            .add_message::<Ticked>()
            .init_resource::<ViewMode>()
            .init_resource::<ViewAngles>()
            .init_resource::<Latched>()
            .add_systems(OnEnter(AppState::InGame), enter_map)
            .add_systems(OnExit(AppState::InGame), leave_map)
            .add_systems(
                Update,
                (
                    spawn_player,
                    toggle_noclip,
                    (look, latch_input, place_camera)
                        .chain()
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
    dummies: Res<Dummies>,
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
    let spawns: Vec<Spawn> = map
        .spawns
        .iter()
        .map(|spawn| Spawn {
            origin: Vec3::from(spawn.source.origin),
            yaw: spawn.source.yaw,
        })
        .collect();
    simulation.set_spawns(spawns.clone());
    simulation.add_entities(&map.entities);

    let spawn = (!spawns.is_empty()).then(|| spawns[start.0 % spawns.len()]);
    let (origin, yaw) = spawn.map_or((Vec3::ZERO, 0.0), |spawn| (spawn.origin, spawn.yaw));
    let id = simulation.spawn_player(origin + Vec3::Z);
    for _ in 0..dummies.0 {
        simulation.join();
    }
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
    info!(
        "player spawned at {} facing {yaw}°, with {} dummies",
        state.origin, dummies.0
    );
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

/// Advances the simulation by one fixed tick with the local player's
/// command (adjusted by the input script, if any). Dummies send none.
fn tick(
    clock: Res<Time<Virtual>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    mut latched: ResMut<Latched>,
    mut angles: ResMut<ViewAngles>,
    sim: Option<ResMut<Sim>>,
    player: Option<ResMut<LocalPlayer>>,
    script: Option<ResMut<InputScript>>,
    mut ticked: MessageWriter<Ticked>,
) {
    let (Some(mut sim), Some(mut player)) = (sim, player) else {
        return;
    };
    let mut cmd = user_cmd(&keys, mouse.as_deref(), latched.buttons, *angles);
    // Commands are numbered by tick: the number seeds bullet spread.
    cmd.number = (sim.0.tick_count() + 1) as u32;
    if let (Some(choice), Some(local)) = (latched.choice, sim.0.player(player.id)) {
        cmd.select = local.inventory.choose(sim.0.arsenal(), choice);
    }
    *latched = Latched::default();
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
    let respawned = events
        .respawns
        .iter()
        .find(|respawn| respawn.player == player.id);
    if let Some(respawn) = respawned {
        *angles = ViewAngles {
            pitch: 0.0,
            yaw: respawn.spawn.yaw,
        };
    }
    if let Some(state) = sim.0.player(player.id).map(|player| player.state) {
        // A respawn jumps across the map: do not glide there.
        player.previous = if respawned.is_some() {
            state
        } else {
            player.current
        };
        player.current = state;
    }
    ticked.write(Ticked(events));
}

/// Puts the camera at the player's eyes, interpolated between the last two
/// ticks by how far the frame is into the next one; low to the floor while
/// dead.
fn place_camera(
    time: Res<Time<Fixed>>,
    player: Option<Res<LocalPlayer>>,
    sim: Option<Res<Sim>>,
    angles: Res<ViewAngles>,
    movement: Res<Movement>,
    mut camera: Query<&mut Transform, With<FlyCam>>,
) {
    let (Some(player), Ok(mut transform)) = (player, camera.single_mut()) else {
        return;
    };
    let alive = sim
        .as_ref()
        .and_then(|sim| sim.0.player(player.id))
        .is_none_or(rl_sim::simulation::Player::alive);
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
    let position = eye(&player.previous).lerp(eye(&player.current), alpha);
    transform.translation = coords::position(position.to_array());
    transform.rotation = view_rotation(*angles);
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
}
