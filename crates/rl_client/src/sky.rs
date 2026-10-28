//! The 3D skybox: scenery drawn behind the sky.
//!
//! Source maps build their distant scenery (skylines, mountains) as a
//! miniature, usually a sixteenth of full size, in a closed-off room of the
//! map holding a `sky_camera`. The engine draws that room behind the world
//! as seen from the sky camera's spot plus the eye's position over the
//! scale, so the miniature seems to lie far off, all around.
//!
//! Here a second camera does that: it draws first, sees only the skybox's
//! layer (`SKY_LAYER`) with the 2D sky behind it, and follows the main
//! camera at `origin + eye / scale`, turned as it is. The main camera then
//! draws the world over it without clearing, so the skybox shows wherever
//! the world leaves sky.
//!
//! The sky camera follows the main one late in the frame, from where the
//! main camera finally is (its propagated `GlobalTransform`), and settles
//! its own `GlobalTransform` before frusta are computed. However the main
//! camera was placed this frame, and by whichever system, the two draw the
//! same view: the skybox never trails the world as the player turns.

use bevy::camera::CameraUpdateSystems;
use bevy::camera::visibility::{RenderLayers, VisibilitySystems};
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::light::Skybox;
use bevy::prelude::*;
use bevy::transform::TransformSystems;
use rl_assets::MapAsset;

use crate::AppState;
use crate::camera::FlyCam;
use crate::world::{MapHandle, MapScoped, SKY_LAYER};

/// Draws the 3D skybox of maps that have one.
pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(AppState::InGame), restore_main_camera)
            .add_systems(
                Update,
                spawn_sky_camera
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            );
        follow_the_main_camera(app);
    }
}

/// Schedules the sky camera's following of the main one, after everything
/// in `Update` has had its say: the view (field of view, clip planes)
/// before Bevy computes the cameras' projections, and the place and turn
/// once transforms are propagated, before frusta and visibility are worked
/// out from them.
fn follow_the_main_camera(app: &mut App) {
    app.add_systems(
        PostUpdate,
        (
            follow_main_projection.before(CameraUpdateSystems),
            follow_main_camera
                .after(TransformSystems::Propagate)
                .before(VisibilitySystems::UpdateFrusta),
        ),
    );
}

/// Marks the camera that draws the 3D skybox (for others to find it).
#[derive(Component, Debug, Clone, Copy)]
pub struct SkyView;

/// The camera that draws the 3D skybox.
#[derive(Component, Debug, Clone, Copy)]
struct SkyCamera {
    /// Where the skybox's miniature of the map's centre is, metres.
    origin: Vec3,
    /// How many times smaller than the map the skybox is.
    scale: f32,
}

/// What handing the main camera's sky over reads and changes.
type MainCamera<'a> = (
    Entity,
    &'a mut Camera,
    Option<&'a Skybox>,
    Option<&'a AmbientLight>,
);

/// The main camera, not the sky's.
type MainOnly = (With<FlyCam>, Without<SkyCamera>);

/// Once the map has loaded, gives it a sky camera if it has a 3D skybox,
/// handing it the 2D sky from the main camera.
fn spawn_sky_camera(
    mut commands: Commands,
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    existing: Query<(), With<SkyCamera>>,
    mut main: Query<MainCamera, With<FlyCam>>,
) {
    if !existing.is_empty() {
        return;
    }
    let Some(map) = handle.and_then(|handle| maps.get(&handle.0)) else {
        return;
    };
    let Some(sky) = map.sky_camera else {
        return;
    };
    let Ok((entity, mut camera, skybox, ambient)) = main.single_mut() else {
        return;
    };
    // Wait for the world to hand the main camera its sky, so it can be
    // moved behind the skybox.
    if map.skybox.is_some() && skybox.is_none() {
        return;
    }
    let mut sky_camera = commands.spawn((
        Camera3d::default(),
        Camera {
            order: -1,
            ..default()
        },
        Tonemapping::None,
        DebandDither::Disabled,
        RenderLayers::layer(SKY_LAYER),
        SkyCamera {
            origin: sky.origin,
            scale: sky.scale,
        },
        SkyView,
        MapScoped,
    ));
    if let Some(skybox) = skybox {
        sky_camera.insert(skybox.clone());
    }
    if let Some(ambient) = ambient {
        sky_camera.insert(ambient.clone());
    }
    commands.entity(entity).remove::<Skybox>();
    camera.clear_color = ClearColorConfig::None;
    info!(scale = sky.scale, "drawing the 3D skybox");
}

/// What [`follow_main_camera`] writes: the sky camera's place, both as set
/// and as propagated.
type SkyPlaced = (
    &'static SkyCamera,
    &'static mut Transform,
    &'static mut GlobalTransform,
);

/// Keeps the sky camera at the skybox's miniature of the eye, turned as the
/// main camera is. It runs after transform propagation, so it reads where
/// the main camera finally is this frame, and sets its own
/// `GlobalTransform` too (propagation has already run), so the frustum and
/// the view drawn are this frame's.
fn follow_main_camera(main: Query<&GlobalTransform, MainOnly>, mut sky: Query<SkyPlaced>) {
    let Ok(eye) = main.single() else {
        return;
    };
    let (_, rotation, translation) = eye.to_scale_rotation_translation();
    for (camera, mut transform, mut global) in &mut sky {
        let placed = Transform {
            translation: camera.origin + translation / camera.scale,
            rotation,
            ..*transform
        };
        // Written only when it moved, so a still view stays unchanged.
        transform.set_if_neq(placed);
        global.set_if_neq(GlobalTransform::from(placed));
    }
}

/// Gives the sky camera the main camera's view, with its near plane scaled
/// down like the skybox.
fn follow_main_projection(
    main: Query<&Projection, MainOnly>,
    mut sky: Query<(&SkyCamera, &mut Projection)>,
) {
    let Ok(Projection::Perspective(main)) = main.single() else {
        return;
    };
    for (camera, mut projection) in &mut sky {
        let near = main.near / camera.scale;
        // Copied exactly, and written only when it differs, so Bevy
        // recomputes the projection only when the view changed (zooming, a
        // new field of view).
        let differs = |a: f32, b: f32| a.to_bits() != b.to_bits();
        if let Projection::Perspective(view) = projection.as_ref()
            && (differs(view.fov, main.fov)
                || differs(view.near, near)
                || differs(view.far, main.far))
            && let Projection::Perspective(view) = projection.as_mut()
        {
            view.fov = main.fov;
            view.near = near;
            view.far = main.far;
        }
    }
}

/// The main camera clears to its own sky again once the map is left.
fn restore_main_camera(mut main: Query<&mut Camera, With<FlyCam>>) {
    for mut camera in &mut main {
        camera.clear_color = ClearColorConfig::Default;
    }
}

#[cfg(test)]
mod tests {
    use bevy::transform::TransformPlugin;

    use super::*;
    use crate::camera::CameraPlaced;

    /// Where and how the main camera is turned this frame.
    #[derive(Resource, Clone, Copy)]
    struct Turn {
        rotation: Quat,
        translation: Vec3,
    }

    fn turn_main_camera(turn: Res<Turn>, mut main: Query<&mut Transform, MainOnly>) {
        for mut transform in &mut main {
            transform.rotation = turn.rotation;
            transform.translation = turn.translation;
        }
    }

    const ORIGIN: Vec3 = Vec3::new(10.0, -4.0, 2.0);
    const SCALE: f32 = 16.0;

    /// An app with a main camera and a sky camera, the sky following, and
    /// the main camera turned by `turn_main_camera` wherever `place` puts it.
    fn app_turning_in(place: impl FnOnce(&mut App)) -> App {
        let mut app = App::new();
        app.add_plugins(TransformPlugin).insert_resource(Turn {
            rotation: Quat::IDENTITY,
            translation: Vec3::ZERO,
        });
        follow_the_main_camera(&mut app);
        place(&mut app);
        app.world_mut().spawn((
            FlyCam::default(),
            Transform::default(),
            Projection::Perspective(PerspectiveProjection::default()),
        ));
        app.world_mut().spawn((
            SkyCamera {
                origin: ORIGIN,
                scale: SCALE,
            },
            Transform::default(),
            Projection::Perspective(PerspectiveProjection::default()),
        ));
        app.update();
        app
    }

    /// Turns the main camera for one frame and checks that the sky camera,
    /// as propagated, was turned alike in that same frame.
    fn assert_sky_follows_within_a_frame(mut app: App) {
        let turn = Turn {
            rotation: Quat::from_euler(EulerRot::YXZ, 1.2, -0.4, 0.0),
            translation: Vec3::new(32.0, 1.5, -64.0),
        };
        app.insert_resource(turn);
        app.update();

        let world = app.world_mut();
        let main = *world
            .query_filtered::<&GlobalTransform, MainOnly>()
            .single(world)
            .unwrap();
        let sky = *world
            .query_filtered::<&GlobalTransform, With<SkyCamera>>()
            .single(world)
            .unwrap();
        assert!(
            main.rotation().angle_between(turn.rotation) < 1e-5,
            "the main camera turned"
        );
        assert!(
            sky.rotation().angle_between(main.rotation()) < 1e-5,
            "the sky camera turned with it, not a frame later"
        );
        let expected = ORIGIN + turn.translation / SCALE;
        assert!(sky.translation().distance(expected) < 1e-5);
    }

    #[test]
    fn the_sky_turns_with_the_camera_in_the_same_frame() {
        // As the player's look and the noclip camera turn it.
        assert_sky_follows_within_a_frame(app_turning_in(|app| {
            app.add_systems(Update, turn_main_camera.in_set(CameraPlaced));
        }));
    }

    #[test]
    fn the_sky_follows_the_camera_wherever_it_was_last_moved() {
        // A camera moved after `Update` (or by a system not ordered against
        // the sky) is still followed in the same frame: the sky reads where
        // it ends up.
        assert_sky_follows_within_a_frame(app_turning_in(|app| {
            app.add_systems(
                PostUpdate,
                turn_main_camera.before(TransformSystems::Propagate),
            );
        }));
    }

    #[test]
    fn the_sky_takes_the_main_cameras_view_scaled_down() {
        let mut app = app_turning_in(|_| {});
        let world = app.world_mut();
        let mut main = world.query_filtered::<&mut Projection, MainOnly>();
        if let Projection::Perspective(view) = main.single_mut(world).unwrap().as_mut() {
            view.fov = 0.6;
            view.near = 0.08;
        }
        app.update();

        let world = app.world_mut();
        let sky = world
            .query_filtered::<&Projection, With<SkyCamera>>()
            .single(world)
            .unwrap();
        let Projection::Perspective(sky) = sky else {
            panic!("a perspective view");
        };
        assert_eq!(sky.fov, 0.6);
        assert!((sky.near - 0.08 / SCALE).abs() < 1e-7);
    }
}
