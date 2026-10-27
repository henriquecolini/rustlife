//! The Gravity Gun's look, from HL2DM's `C_WeaponPhysCannon`: its prongs open
//! (a delta layer of the view model, driven by the `active` pose parameter)
//! and glow, and a beam runs from the muzzle to the prop it is pulling or
//! holding.
//!
//! The prongs open when the gun has something to take (a prop it could pull
//! or grab is under the crosshair), is pulling or is holding one, and close
//! again otherwise. `active` is an interpolated value
//! (`m_ElementParameter`) that moves to its target along a spline: to open
//! takes 0.2 s, to close 0.5 s.
//!
//! Glows sit on the model's attachments: a core (`sprites/orangecore1`,
//! scale 14) on the muzzle, at half strength when the gun is ready and full
//! when it is pulling or holding, and soft lights (`orangelight1`, strength
//! 64 of 255) on the forks, fading in and out over 0.2 s. Their sizes are the
//! game's scales read as units across, which was not verified against the
//! game.

use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_data::Shot;
use rl_sim::tools::manipulator::{Aim, liftable_target};

use crate::AppState;
use crate::animation::{Animated, Attachment, SequencePlayer};
use crate::camera::FlyCam;
use crate::effects::as_seen_by_main;
use crate::net::Game;
use crate::player::{LocalPlayer, ViewAngles};
use crate::settings::Bindings;
use crate::viewmodel::{VIEW_LAYER, ViewModel};

/// Seconds the prongs take to open...
pub(crate) const OPEN_SECS: f32 = 0.2;
/// ...and to close.
pub(crate) const CLOSE_SECS: f32 = 0.5;

/// A value on its way to a target along a spline, as `InterpolatedValue`
/// moves HL2's: restarted from where it is whenever the target changes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Element {
    from: f32,
    to: f32,
    start: f32,
    duration: f32,
}

impl Element {
    /// Where it is at time `now`, seconds.
    pub(crate) fn value(&self, now: f32) -> f32 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let t = ((now - self.start) / self.duration).clamp(0.0, 1.0);
        let eased = t * t * (3.0 - 2.0 * t);
        self.from + (self.to - self.from) * eased
    }

    /// Sends it to `target` over `duration` from where it is at `now`, if
    /// that is not already where it is headed.
    pub(crate) fn go_to(&mut self, target: f32, duration: f32, now: f32) {
        if (self.to - target).abs() < f32::EPSILON {
            return;
        }
        *self = Self {
            from: self.value(now),
            to: target,
            start: now,
            duration,
        };
    }
}

/// The prongs' value, kept with the Gravity Gun's view model.
#[derive(Component, Debug, Default)]
pub(crate) struct Prongs(pub(crate) Element);

/// What the gun is doing, as far as how it looks goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Look {
    /// Nothing to take: prongs shut, no glow.
    #[default]
    Shut,
    /// A prop it could take is under the crosshair: prongs open, a glow.
    Ready,
    /// Pulling a prop in or holding one: a brighter glow and a beam.
    Working,
}

/// The Gravity Gun in hand this frame, if it is.
#[derive(Resource, Debug, Default)]
struct GunLook {
    /// Its view model.
    view: Option<Entity>,
    look: Look,
    /// The prop it works on (Bevy space), for the beam.
    target: Option<Vec3>,
}

/// The game's sprites for the effects.
#[derive(Resource, Default)]
struct GunSprites {
    core: Handle<SpriteAsset>,
    light: Handle<SpriteAsset>,
    beam: Handle<SpriteAsset>,
}

fn load_sprites(mut sprites: ResMut<GunSprites>, assets: Res<AssetServer>) {
    let load = |name: &str| assets.load(format!("{SOURCE}://materials/sprites/{name}.vmt"));
    sprites.core = load("orangecore1");
    sprites.light = load("orangelight1_noz");
    sprites.beam = load("physbeam");
}

/// Reads what the gun is doing, from the local player's game state and
/// whether the alternate button is held, and opens or shuts the prongs.
fn read_gun(
    time: Res<Time>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    angles: Res<ViewAngles>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    bindings: Res<Bindings>,
    mut look: ResMut<GunLook>,
    mut views: Query<(
        Entity,
        &ViewModel,
        &Animated,
        &mut SequencePlayer,
        &mut Prongs,
    )>,
) {
    *look = GunLook::default();
    let (Some(sim), Some(local)) = (game.sim(), local) else {
        return;
    };
    let now = time.elapsed_secs();
    for (entity, view, animated, mut player, mut prongs) in &mut views {
        let manipulator = sim.arsenal().weapon(view.weapon()).is_some_and(|weapon| {
            weapon
                .fire_modes()
                .any(|fire| matches!(fire.shot, Shot::Manipulator))
        });
        let Some(layer) = animated.layer("prong_blend").filter(|_| manipulator) else {
            continue;
        };
        let pulling_wanted = bindings.pressed("attack2", &keys, mouse.as_deref());
        let (state, target) = gun_state(sim, local.id, *angles, pulling_wanted);
        *look = GunLook {
            view: Some(entity),
            look: state,
            target,
        };
        if state == Look::Shut {
            prongs.0.go_to(0.0, CLOSE_SECS, now);
        } else {
            prongs.0.go_to(1.0, OPEN_SECS, now);
        }
        player.set_layer(layer, prongs.0.value(now), 1.0);
    }
}

/// What the gun of `player` is doing, and the centre of the prop it works
/// on: holding one, pulling one in (`pulling_wanted`, the alternate button
/// held, on a prop beyond grab range), ready to take one, or nothing.
fn gun_state(
    sim: &rl_sim::simulation::Simulation,
    player: rl_sim::simulation::PlayerId,
    angles: ViewAngles,
    pulling_wanted: bool,
) -> (Look, Option<Vec3>) {
    let centre = |body| {
        sim.physics()
            .center_of_mass(body)
            .map(|centre| coords::position(centre.to_array()))
    };
    if let Some(held) = sim.player(player).and_then(|p| p.manipulator.held()) {
        return (Look::Working, centre(held));
    }
    let Some(eye) = sim.eye_position(player) else {
        return (Look::Shut, None);
    };
    let aim = Aim {
        eye,
        pitch: angles.pitch,
        yaw: angles.yaw,
    };
    let config = &sim.data().manipulator;
    let Some((body, point)) = liftable_target(&aim, sim.world(), sim.physics(), config) else {
        return (Look::Shut, None);
    };
    if pulling_wanted && point.distance(eye) > config.grab.range {
        (Look::Working, centre(body))
    } else {
        (Look::Ready, None)
    }
}

/// A glow on one of the view model's attachments.
#[derive(Component)]
struct Glow {
    view: Entity,
    attachment: Entity,
    /// The muzzle's core, rather than a light on a fork.
    core: bool,
    alpha: Element,
    size: Element,
    material: Handle<StandardMaterial>,
}

/// The beam to the prop being worked, drawn in the world.
#[derive(Component)]
struct PhysBeam {
    view: Entity,
    material: Handle<StandardMaterial>,
    /// How many frames its sprite has: it crackles through them.
    frames: u32,
}

/// How many beam frames pass in a second.
const BEAM_FRAMES_PER_SECOND: f32 = 20.0;

/// The texture coordinates of frame `frame` of a sprite whose `frames`
/// frames lie side by side.
fn frame_uv(frames: u32, frame: u32) -> bevy::math::Affine2 {
    let frames = frames.max(1) as f32;
    bevy::math::Affine2::from_scale_angle_translation(
        Vec2::new(1.0 / frames, 1.0),
        0.0,
        Vec2::new((frame % frames as u32) as f32 / frames, 0.0),
    )
}

/// Seconds a glow takes to come and go.
const GLOW_SECS: f32 = 0.2;
/// Where the glows go, by attachment, and whether it is the muzzle's core.
const GLOWS: [(&str, bool); 5] = [
    ("muzzle", true),
    ("fork1m", false),
    ("fork1t", false),
    ("fork2m", false),
    ("fork2t", false),
];
/// How big the muzzle's core is, and the forks' lights ready and working,
/// units across.
const CORE_SIZE: f32 = 14.0;
const LIGHT_SIZE: (f32, f32) = (6.4, 8.0);
/// How strong they are: the core's ready and working (128 and 255 of 255),
/// the lights' (64 of 255).
const CORE_ALPHA: (f32, f32) = (0.5, 1.0);
const LIGHT_ALPHA: f32 = 0.25;
/// Width of the beam, units, and how strong.
const BEAM_WIDTH: f32 = 2.0;
const BEAM_ALPHA: f32 = 0.6;

/// The attachment named `name` on `view`'s model.
fn attachment_of(
    view: Entity,
    name: &str,
    attachments: &Query<(Entity, &Attachment)>,
    parents: &Query<&ChildOf>,
) -> Option<Entity> {
    attachments.iter().find_map(|(entity, attachment)| {
        if attachment.0 != name {
            return None;
        }
        let mut at = entity;
        for _ in 0..32 {
            if at == view {
                return Some(entity);
            }
            at = parents.get(at).ok()?.parent();
        }
        None
    })
}

/// Makes the gun's glows and beam when it comes into hand, and removes them
/// when it goes.
fn spawn_effects(
    mut commands: Commands,
    look: Res<GunLook>,
    sprites: Res<GunSprites>,
    assets: Res<Assets<SpriteAsset>>,
    attachments: Query<(Entity, &Attachment)>,
    parents: Query<&ChildOf>,
    glows: Query<(Entity, &Glow)>,
    beams: Query<(Entity, &PhysBeam)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, glow) in &glows {
        if look.view != Some(glow.view) {
            commands.entity(entity).try_despawn();
        }
    }
    for (entity, beam) in &beams {
        if look.view != Some(beam.view) {
            commands.entity(entity).try_despawn();
        }
    }
    let Some(view) = look.view else {
        return;
    };
    if glows.iter().any(|(_, glow)| glow.view == view) {
        return;
    }
    let (Some(core), Some(light), Some(beam)) = (
        assets.get(&sprites.core),
        assets.get(&sprites.light),
        assets.get(&sprites.beam),
    ) else {
        return;
    };
    let sprite_material = |materials: &mut Assets<StandardMaterial>, sprite: &SpriteAsset| {
        materials.add(StandardMaterial {
            base_color: Color::linear_rgba(1.0, 1.0, 1.0, 0.0),
            base_color_texture: Some(sprite.image.clone()),
            uv_transform: frame_uv(sprite.frames, 0),
            unlit: true,
            double_sided: true,
            cull_mode: None,
            alpha_mode: AlphaMode::Add,
            ..default()
        })
    };
    let quad = meshes.add(Rectangle::new(1.0, 1.0));
    for (name, is_core) in GLOWS {
        let Some(attachment) = attachment_of(view, name, &attachments, &parents) else {
            continue;
        };
        let material = sprite_material(&mut materials, if is_core { core } else { light });
        commands.spawn((
            Glow {
                view,
                attachment,
                core: is_core,
                alpha: Element::default(),
                size: Element::default(),
                material: material.clone(),
            },
            Mesh3d(quad.clone()),
            MeshMaterial3d(material),
            Transform::from_scale(Vec3::ZERO),
            NoFrustumCulling,
            RenderLayers::layer(VIEW_LAYER),
        ));
    }
    let material = sprite_material(&mut materials, beam);
    commands.spawn((
        PhysBeam {
            view,
            material: material.clone(),
            frames: beam.frames,
        },
        Mesh3d(quad),
        MeshMaterial3d(material),
        Transform::from_scale(Vec3::ZERO),
        NoFrustumCulling,
    ));
}

/// Fades the glows with what the gun is doing and keeps them on their
/// attachments, facing the camera.
fn shine(
    time: Res<Time>,
    look: Res<GunLook>,
    attachments: Query<&GlobalTransform, With<Attachment>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut glows: Query<(&mut Glow, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let now = time.elapsed_secs();
    for (mut glow, mut transform) in &mut glows {
        let (alpha, size) = match (glow.core, look.look) {
            (_, Look::Shut) => (0.0, if glow.core { CORE_SIZE } else { LIGHT_SIZE.0 }),
            (true, Look::Ready) => (CORE_ALPHA.0, CORE_SIZE),
            (true, Look::Working) => (CORE_ALPHA.1, CORE_SIZE),
            (false, Look::Ready) => (LIGHT_ALPHA, LIGHT_SIZE.0),
            (false, Look::Working) => (LIGHT_ALPHA, LIGHT_SIZE.1),
        };
        glow.alpha.go_to(alpha, GLOW_SECS, now);
        glow.size.go_to(size, GLOW_SECS, now);
        let shown = glow.alpha.value(now);
        if let Ok(attachment) = attachments.get(glow.attachment) {
            transform.translation = attachment.translation();
        }
        if let Ok(camera) = camera.single() {
            transform.rotation = camera.to_scale_rotation_translation().1;
        }
        let across = glow.size.value(now) * coords::METERS_PER_UNIT;
        transform.scale = if shown > 0.001 {
            Vec3::new(across, across, 1.0)
        } else {
            Vec3::ZERO
        };
        if let Some(mut material) = materials.get_mut(&glow.material) {
            material.base_color = Color::linear_rgba(1.0, 1.0, 1.0, shown);
        }
    }
}

/// Draws the beam from the muzzle to the prop being worked, while there is
/// one.
fn beam(
    time: Res<Time>,
    look: Res<GunLook>,
    named: Query<(Entity, &Attachment)>,
    parents: Query<&ChildOf>,
    attachments: Query<&GlobalTransform, With<Attachment>>,
    camera: Query<(&GlobalTransform, &Projection), With<FlyCam>>,
    mut beams: Query<(&PhysBeam, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let (Ok((camera, projection)), Ok((beam, mut transform))) =
        (camera.single(), beams.single_mut())
    else {
        return;
    };
    let muzzle = look
        .view
        .and_then(|view| attachment_of(view, "muzzle", &named, &parents))
        .and_then(|entity| attachments.get(entity).ok())
        .map(GlobalTransform::translation);
    let (Some(muzzle), Some(target), Projection::Perspective(view), Look::Working) =
        (muzzle, look.target, projection, look.look)
    else {
        transform.scale = Vec3::ZERO;
        return;
    };
    // The view model camera has a narrower view: where the main camera sees
    // the muzzle is the beam's start.
    let start = as_seen_by_main(muzzle, camera, view.fov);
    let along = target - start;
    let length = along.length();
    let Some(direction) = along.try_normalize() else {
        transform.scale = Vec3::ZERO;
        return;
    };
    // A strip along the beam, turned about it to face the camera.
    let toward = (camera.translation() - start.midpoint(target)).normalize_or_zero();
    let across = direction.cross(toward).normalize_or_zero();
    let facing = across.cross(direction);
    transform.translation = start.midpoint(target);
    transform.rotation = Quat::from_mat3(&Mat3::from_cols(across, direction, facing));
    transform.scale = Vec3::new(BEAM_WIDTH * coords::METERS_PER_UNIT, length, 1.0);
    if let Some(mut material) = materials.get_mut(&beam.material) {
        material.base_color = Color::linear_rgba(1.0, 1.0, 1.0, BEAM_ALPHA);
        let frame = (time.elapsed_secs() * BEAM_FRAMES_PER_SECOND) as u32;
        material.uv_transform = frame_uv(beam.frames, frame);
    }
}

/// Draws the Gravity Gun's prongs, glows and beam.
pub(crate) struct GravityGunPlugin;

impl Plugin for GravityGunPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GunLook>()
            .init_resource::<GunSprites>()
            .add_systems(Startup, load_sprites)
            .add_systems(
                Update,
                (read_gun, spawn_effects, shine, beam)
                    .chain()
                    .after(crate::viewmodel::animate_view_model)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_along_a_spline_in_two_tenths_of_a_second() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 10.0);
        assert_eq!(element.value(10.0), 0.0);
        assert!(
            (element.value(10.1) - 0.5).abs() < 1e-5,
            "halfway at the middle"
        );
        assert!(element.value(10.05) < 0.25, "slow to start");
        assert_eq!(element.value(10.2), 1.0);
        assert_eq!(element.value(11.0), 1.0);
    }

    #[test]
    fn closing_starts_from_wherever_opening_got_to() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 0.0);
        let partway = element.value(0.1);
        element.go_to(0.0, CLOSE_SECS, 0.1);
        assert!((element.value(0.1) - partway).abs() < 1e-5, "no jump");
        assert_eq!(element.value(0.6), 0.0);
    }

    #[test]
    fn a_frame_of_a_strip_is_a_slice_of_the_texture() {
        let uv = frame_uv(20, 3);
        assert!((uv.transform_point2(Vec2::new(0.0, 0.5)).x - 0.15).abs() < 1e-6);
        assert!((uv.transform_point2(Vec2::new(1.0, 0.5)).x - 0.2).abs() < 1e-6);
        // Frames wrap round.
        assert_eq!(frame_uv(20, 23), frame_uv(20, 3));
    }

    #[test]
    fn the_same_target_does_not_restart_it() {
        let mut element = Element::default();
        element.go_to(1.0, OPEN_SECS, 0.0);
        let before = element.value(0.15);
        element.go_to(1.0, OPEN_SECS, 0.15);
        assert!((element.value(0.15) - before).abs() < 1e-6);
    }
}
