//! A free-flying "noclip" camera for inspecting maps.
//!
//! This is the M1 viewer camera: mouse to look, WASD to move, space/control to
//! rise and fall, shift to move faster. The cursor is grabbed on entering a
//! map, released in the map browser, and toggled with `Escape`. Later milestones replace this with the predicted
//! player view, but the fly camera stays useful for debugging.

use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::AppState;

/// Whether the fly camera grabs the cursor on startup. Inserted by
/// [`crate::run`]; disabled for non-interactive screenshot captures so a
/// capture never steals the mouse.
#[derive(Resource)]
pub struct CursorGrab(pub bool);

/// Marker component and look state for the fly camera.
#[derive(Component)]
pub struct FlyCam {
    /// Yaw in radians, around Bevy's +Y (up) axis.
    pub yaw: f32,
    /// Pitch in radians, clamped just short of straight up/down.
    pub pitch: f32,
    /// Movement speed in metres per second.
    pub speed: f32,
}

impl Default for FlyCam {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            speed: 8.0,
        }
    }
}

/// Spawns the fly camera and drives its look and movement.
pub struct FlyCamPlugin;

impl Plugin for FlyCamPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_camera)
            .add_systems(OnEnter(AppState::InGame), grab_cursor)
            .add_systems(OnEnter(AppState::Browser), release_cursor)
            .add_systems(
                Update,
                (toggle_grab, look, movement).run_if(in_state(AppState::InGame)),
            );
    }
}

/// How far (radians) one pixel of mouse motion turns the view.
const MOUSE_SENSITIVITY: f32 = 0.003;
/// Pitch limit, just under 90 degrees, to avoid gimbal flip at the poles.
const PITCH_LIMIT: f32 = 1.54;

fn spawn_camera(mut commands: Commands) {
    commands.spawn((
        Camera3d::default(),
        // Source's LDR renderer writes `albedo × lightmap` straight to the
        // screen. Bevy's default filmic tone-mapper compresses and desaturates
        // that, so it is turned off to keep the original look.
        Tonemapping::None,
        // Bevy's debanding dither is applied inside tone-mapping, so without a
        // tone-mapper it would do nothing; disable it explicitly.
        DebandDither::Disabled,
        Transform::from_xyz(0.0, 2.0, 0.0),
        FlyCam::default(),
        // Ambient light is a per-view component in this Bevy version, so the
        // camera carries it; without it, surfaces facing away from the sun are
        // pure black.
        AmbientLight {
            brightness: 600.0,
            // Lightmapped world surfaces carry their own baked ambient.
            affects_lightmapped_meshes: false,
            ..default()
        },
    ));
}

/// Locks and hides the cursor so mouse-look feels like an FPS, unless cursor
/// grabbing has been disabled (e.g. during a screenshot capture).
fn grab_cursor(
    grab: Option<Res<CursorGrab>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if grab.is_some_and(|g| !g.0) {
        return;
    }
    if let Ok(mut options) = cursor.single_mut() {
        set_grab(&mut options, true);
    }
}

/// Frees the cursor so the map browser can be used with the mouse.
fn release_cursor(mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>) {
    if let Ok(mut options) = cursor.single_mut() {
        set_grab(&mut options, false);
    }
}

/// `Escape` toggles the cursor grab so the window can be released.
fn toggle_grab(
    keys: Res<ButtonInput<KeyCode>>,
    mut cursor: Query<&mut CursorOptions, With<PrimaryWindow>>,
) {
    if keys.just_pressed(KeyCode::Escape)
        && let Ok(mut options) = cursor.single_mut()
    {
        let grabbed = options.grab_mode != CursorGrabMode::None;
        set_grab(&mut options, !grabbed);
    }
}

fn set_grab(options: &mut CursorOptions, grab: bool) {
    options.grab_mode = if grab {
        CursorGrabMode::Locked
    } else {
        CursorGrabMode::None
    };
    options.visible = !grab;
}

/// Turns the camera with the mouse while the cursor is grabbed.
fn look(
    mouse: Res<AccumulatedMouseMotion>,
    cursor: Query<&CursorOptions, With<PrimaryWindow>>,
    mut camera: Query<(&mut Transform, &mut FlyCam)>,
) {
    let grabbed = cursor
        .single()
        .is_ok_and(|options| options.grab_mode != CursorGrabMode::None);
    if !grabbed || mouse.delta == Vec2::ZERO {
        return;
    }
    if let Ok((mut transform, mut cam)) = camera.single_mut() {
        cam.yaw -= mouse.delta.x * MOUSE_SENSITIVITY;
        cam.pitch =
            (cam.pitch - mouse.delta.y * MOUSE_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        transform.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    }
}

/// Moves the camera with WASD + space/control, faster while shift is held.
fn movement(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut camera: Query<(&mut Transform, &FlyCam)>,
) {
    let Ok((mut transform, cam)) = camera.single_mut() else {
        return;
    };

    let mut direction = Vec3::ZERO;
    let forward = *transform.forward();
    let right = *transform.right();
    if keys.pressed(KeyCode::KeyW) {
        direction += forward;
    }
    if keys.pressed(KeyCode::KeyS) {
        direction -= forward;
    }
    if keys.pressed(KeyCode::KeyD) {
        direction += right;
    }
    if keys.pressed(KeyCode::KeyA) {
        direction -= right;
    }
    if keys.pressed(KeyCode::Space) {
        direction += Vec3::Y;
    }
    if keys.pressed(KeyCode::ControlLeft) {
        direction -= Vec3::Y;
    }

    if direction != Vec3::ZERO {
        let boost = if keys.pressed(KeyCode::ShiftLeft) {
            4.0
        } else {
            1.0
        };
        transform.translation += direction.normalize() * cam.speed * boost * time.delta_secs();
    }
}
