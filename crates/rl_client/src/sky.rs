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

use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::{DebandDither, Tonemapping};
use bevy::light::Skybox;
use bevy::prelude::*;
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
                (spawn_sky_camera, follow_main_camera)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
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

/// Keeps the sky camera at the skybox's miniature of the eye, turned as the
/// main camera is, with its view scaled down alike.
fn follow_main_camera(
    main: Query<(&Transform, &Projection), MainOnly>,
    mut sky: Query<(&SkyCamera, &mut Transform, &mut Projection)>,
) {
    let Ok((eye, projection)) = main.single() else {
        return;
    };
    for (camera, mut transform, mut sky_projection) in &mut sky {
        transform.translation = camera.origin + eye.translation / camera.scale;
        transform.rotation = eye.rotation;
        if let (Projection::Perspective(main), Projection::Perspective(view)) =
            (projection, sky_projection.as_mut())
        {
            view.fov = main.fov;
            view.near = main.near / camera.scale;
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
