//! A free-flying "noclip" camera for inspecting maps.
//!
//! Mouse to look, the movement keys to move, jump/crouch to rise and fall,
//! walk to move faster. The menus grab and release the cursor
//! ([`crate::menu`]). The same camera is the player's view while walking
//! ([`crate::player`] places it); it flies freely only in noclip
//! ([`ViewMode::Noclip`]).

use bevy::camera::visibility::VisibilitySystems;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use crate::AppState;
use crate::menu::Menu;
use crate::player::ViewMode;
use crate::settings::{Bindings, RADIANS_PER_PIXEL, Settings};

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

/// The systems that put the camera where it is this frame. Whatever follows
/// or faces the camera runs after them, so it sees the camera as it is now,
/// not as it was last frame (which is what shows as a lag, the more so the
/// faster the player turns).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraPlaced;

/// Where effects that sit on a model's attachments (muzzle flashes, the
/// Gravity Gun's glows) follow them: after transforms are propagated, so the
/// attachments are where the animation and the camera put them this frame
/// (in `Update` they are where they were last frame), and before anything is
/// culled. Such an effect sets its own `GlobalTransform` too, which is not
/// propagated again.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttachedEffects;

/// What [`settle_camera`] reads and writes.
type CameraSettled = (&'static Transform, &'static mut GlobalTransform);

/// Brings the camera's `GlobalTransform` up to its `Transform` as soon as it
/// is placed: transforms are propagated late in the frame, and everything
/// that faces the camera reads it before that.
pub(crate) fn settle_camera(mut camera: Query<CameraSettled, (With<FlyCam>, Without<ChildOf>)>) {
    for (transform, mut global) in &mut camera {
        *global = GlobalTransform::from(*transform);
    }
}

/// Spawns the fly camera and drives its look and movement.
pub struct FlyCamPlugin;

impl Plugin for FlyCamPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            PostUpdate,
            AttachedEffects
                .after(TransformSystems::Propagate)
                .before(VisibilitySystems::CheckVisibility),
        )
        .add_systems(Startup, spawn_camera)
        .add_systems(
            Update,
            (look, movement, settle_camera)
                .chain()
                .in_set(CameraPlaced)
                .run_if(flying)
                .run_if(in_state(AppState::InGame)),
        )
        .add_systems(
            PostUpdate,
            keep_cameras_in_step.before(bevy::render::camera::camera_system),
        );
    }
}

/// Makes every camera learn the window's new size in the same frame.
///
/// Bevy recomputes a camera's target size only when a window event arrives or
/// that camera's own `Projection` changes. The field of view follows the
/// window's shape ([`crate::settings`]), so the main camera's projection
/// changes on its own, and the `Window` component takes a new mode or size
/// before the compositor has resized anything. For a frame the main camera
/// saw the new size while the view model's still had the old one. Cameras
/// drawing to the same window share their depth and colour textures, sized
/// by whichever camera comes first, so the two disagreed, wgpu refused the
/// render pass ("Attachments have differing sizes") and Bevy quit; switching
/// to or from fullscreen did it. A camera whose size lags the window is
/// marked here, so Bevy refreshes it with the others.
///
/// Every camera draws to the primary window.
fn keep_cameras_in_step(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&Camera, &mut Projection)>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let size = window.physical_size();
    for (camera, mut projection) in &mut cameras {
        if camera
            .physical_target_size()
            .is_some_and(|target| target != size)
        {
            projection.set_changed();
        }
    }
}

/// Whether the camera flies freely: in noclip, or always when there is no
/// player simulation (as in tests that only view maps).
fn flying(mode: Option<Res<ViewMode>>) -> bool {
    mode.is_none_or(|mode| *mode == ViewMode::Noclip)
}

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

/// Turns the camera with the mouse while the cursor is grabbed.
fn look(
    mouse: Res<AccumulatedMouseMotion>,
    settings: Res<Settings>,
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
        let turn = RADIANS_PER_PIXEL * settings.controls.sensitivity;
        let invert = if settings.controls.invert_y {
            -1.0
        } else {
            1.0
        };
        cam.yaw -= mouse.delta.x * turn;
        cam.pitch = (cam.pitch - mouse.delta.y * turn * invert).clamp(-PITCH_LIMIT, PITCH_LIMIT);
        transform.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    }
}

/// Moves the camera with WASD + space/control, faster while shift is held.
fn movement(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    bindings: Res<Bindings>,
    menu: Res<Menu>,
    mut camera: Query<(&mut Transform, &FlyCam)>,
) {
    let Ok((mut transform, cam)) = camera.single_mut() else {
        return;
    };

    if menu.is_open() {
        return;
    }
    let held = |action: &str| bindings.pressed(action, &keys, mouse.as_deref());
    let mut direction = Vec3::ZERO;
    let forward = *transform.forward();
    let right = *transform.right();
    if held("forward") {
        direction += forward;
    }
    if held("back") {
        direction -= forward;
    }
    if held("right") {
        direction += right;
    }
    if held("left") {
        direction -= right;
    }
    if held("jump") {
        direction += Vec3::Y;
    }
    if held("duck") {
        direction -= Vec3::Y;
    }

    if direction != Vec3::ZERO {
        let boost = if held("sprint") { 4.0 } else { 1.0 };
        transform.translation += direction.normalize() * cam.speed * boost * time.delta_secs();
    }
}

#[cfg(test)]
mod tests {
    use bevy::camera::RenderTargetInfo;
    use bevy::window::WindowResolution;

    use super::*;

    /// The cameras whose projection was marked changed in the last frame.
    #[derive(Resource, Default)]
    struct Marked(Vec<Entity>);

    fn record_marked(projections: Query<(Entity, Ref<Projection>)>, mut marked: ResMut<Marked>) {
        marked.0 = projections
            .iter()
            .filter(|(_, projection)| projection.is_changed())
            .map(|(entity, _)| entity)
            .collect();
    }

    fn app_with_window(width: u32, height: u32) -> App {
        let mut app = App::new();
        app.init_resource::<Marked>()
            .add_systems(Update, (keep_cameras_in_step, record_marked).chain());
        app.world_mut().spawn((
            Window {
                resolution: WindowResolution::new(width, height),
                ..default()
            },
            PrimaryWindow,
        ));
        app
    }

    /// A camera that believes its target is `width` by `height`.
    fn spawn_camera(app: &mut App, width: u32, height: u32) -> Entity {
        let mut camera = Camera::default();
        camera.computed.target_info = Some(RenderTargetInfo {
            physical_size: UVec2::new(width, height),
            scale_factor: 1.0,
        });
        app.world_mut().spawn((camera, Projection::default())).id()
    }

    /// Runs two frames: the first marks everything as new, so what the
    /// second marks is what the system did.
    fn marked_after_settling(app: &mut App) -> Vec<Entity> {
        app.update();
        app.update();
        let mut marked = app.world().resource::<Marked>().0.clone();
        marked.sort();
        marked
    }

    #[test]
    fn a_camera_that_lags_the_window_is_marked_for_a_refresh_the_others_are_not() {
        let mut app = app_with_window(1920, 1080);
        let current = spawn_camera(&mut app, 1920, 1080);
        let lagging = spawn_camera(&mut app, 1280, 720);
        assert_ne!(current, lagging);
        assert_eq!(marked_after_settling(&mut app), [lagging]);
    }

    #[test]
    fn cameras_that_agree_with_the_window_are_left_alone() {
        let mut app = app_with_window(1920, 1080);
        spawn_camera(&mut app, 1920, 1080);
        spawn_camera(&mut app, 1920, 1080);
        assert_eq!(marked_after_settling(&mut app), []);
    }

    #[test]
    fn a_resized_window_marks_every_camera_and_a_refreshed_one_is_released() {
        let mut app = app_with_window(1920, 1080);
        let first = spawn_camera(&mut app, 1920, 1080);
        let second = spawn_camera(&mut app, 1920, 1080);
        assert_eq!(marked_after_settling(&mut app), []);

        let mut window = app
            .world_mut()
            .query_filtered::<&mut Window, With<PrimaryWindow>>();
        window
            .single_mut(app.world_mut())
            .unwrap()
            .resolution
            .set_physical_resolution(1280, 720);
        app.update();
        let mut marked = app.world().resource::<Marked>().0.clone();
        marked.sort();
        let mut both = [first, second];
        both.sort();
        assert_eq!(marked, both);

        // Bevy's `camera_system` then learns the size; they are left alone.
        for camera in both {
            app.world_mut()
                .get_mut::<Camera>(camera)
                .unwrap()
                .computed
                .target_info = Some(RenderTargetInfo {
                physical_size: UVec2::new(1280, 720),
                scale_factor: 1.0,
            });
        }
        app.update();
        assert_eq!(app.world().resource::<Marked>().0, []);
    }

    #[test]
    fn a_placed_camera_is_where_it_was_placed_before_transforms_propagate() {
        let mut world = World::new();
        let camera = world
            .spawn((
                FlyCam::default(),
                Transform::from_xyz(1.0, 2.0, 3.0),
                GlobalTransform::default(),
            ))
            .id();
        let child = world
            .spawn((
                Transform::from_xyz(5.0, 0.0, 0.0),
                GlobalTransform::default(),
                ChildOf(camera),
            ))
            .id();
        let settle = world.register_system(settle_camera);
        world.run_system(settle).unwrap();
        assert_eq!(
            world.get::<GlobalTransform>(camera).unwrap().translation(),
            Vec3::new(1.0, 2.0, 3.0)
        );
        // Its children wait for the propagation.
        assert_eq!(
            world.get::<GlobalTransform>(child).unwrap().translation(),
            Vec3::ZERO
        );
    }
}
