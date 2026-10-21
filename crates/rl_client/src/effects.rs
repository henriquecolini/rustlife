//! Combat effects: muzzle flashes, tracers, impact sparks and explosions.
//!
//! Each is one of the mounted game's sprite materials, as `effects.ron`
//! names them, drawn on a quad facing the camera:
//!
//! - a **muzzle flash** when a gun fires, at its model's `muzzle`
//!   attachment: the view model's in first person (drawn on its layer, so
//!   it sits on the weapon in hand), the world model's for everyone else;
//! - a **tracer** flying from the muzzle to where each bullet stopped, a
//!   streak turned about its line to face the camera;
//! - **sparks** where a bullet or swing strikes anything but a player;
//! - an **explosion**, which [`crate::particles`] draws.
//!
//! A sprite's frames lie side by side in one image (see
//! [`rl_assets::SpriteAsset`]); a material shows one by shifting its
//! texture coordinates.

use std::collections::HashMap;

use bevy::camera::visibility::RenderLayers;
use bevy::math::Affine2;
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_data::{EffectsConfig, MuzzleFlashConfig, Shot, SpriteEffectConfig, TracerConfig};
use rl_sim::combat::hitscan::Struck;
use rl_sim::movement::view_direction;
use rl_sim::rng::Rng;
use rl_sim::simulation::{PlayerId, Simulation};
use rl_sim::weapons::WeaponId;

use crate::AppState;
use crate::actors::PlayerView;
use crate::animation::Attachment;
use crate::camera::FlyCam;
use crate::net::Game;
use crate::particles::Blast;
use crate::player::{LocalPlayer, Ticked};
use crate::viewmodel::{VIEW_FOV_DEGREES, VIEW_LAYER, ViewModel};
use crate::world::MapScoped;

/// The attachment guns fire from.
const MUZZLE: &str = "muzzle";
/// Where a gun without a muzzle attachment fires from, relative to the
/// eyes: forward, right and down, units.
const FALLBACK_MUZZLE: Vec3 = Vec3::new(24.0, 5.0, 6.0);
/// How far off a struck surface sparks are drawn, units, so they do not
/// cut into it.
const IMPACT_LIFT: f32 = 1.0;
/// Most steps from an attachment up to the model that owns it.
const MAX_DEPTH: usize = 64;

/// Draws combat effects.
pub struct EffectsPlugin;

impl Plugin for EffectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Sprites>().add_systems(
            Update,
            (
                load_sprites,
                spawn_effects,
                fly_tracers,
                follow_attachments,
                face_camera,
                expire,
            )
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// The effects' sprites, by name, and what they are drawn with.
#[derive(Resource)]
struct Sprites {
    handles: HashMap<String, Handle<SpriteAsset>>,
    /// A material per sprite showing its first frame, made once it loads.
    materials: HashMap<String, Handle<StandardMaterial>>,
    /// The unit quad every effect is drawn on.
    quad: Option<Handle<Mesh>>,
    /// Picks flashes and their roll.
    rng: Rng,
}

impl Default for Sprites {
    fn default() -> Self {
        Self {
            handles: HashMap::new(),
            materials: HashMap::new(),
            quad: None,
            rng: Rng::seeded(&[0x0065_6666_6563_7473]),
        }
    }
}

impl Sprites {
    /// The material showing sprite `name`'s first frame, once it has loaded.
    fn still(
        &mut self,
        name: &str,
        sprites: &Assets<SpriteAsset>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Option<Handle<StandardMaterial>> {
        if let Some(material) = self.materials.get(name) {
            return Some(material.clone());
        }
        let sprite = sprites.get(self.handles.get(name)?)?;
        let material = materials.add(sprite_material(sprite));
        self.materials.insert(name.to_owned(), material.clone());
        Some(material)
    }
}

/// How a sprite is drawn: unlit, both sides, added to what is behind it or
/// blended over it, showing its first frame.
fn sprite_material(sprite: &SpriteAsset) -> StandardMaterial {
    StandardMaterial {
        base_color_texture: Some(sprite.image.clone()),
        unlit: true,
        double_sided: true,
        cull_mode: None,
        alpha_mode: if sprite.additive {
            AlphaMode::Add
        } else {
            AlphaMode::Blend
        },
        uv_transform: frame_uv(0, sprite.frames),
        ..default()
    }
}

/// The texture coordinates showing frame `frame` of `frames` side by side.
fn frame_uv(frame: u32, frames: u32) -> Affine2 {
    let frames = frames.max(1) as f32;
    Affine2::from_scale_angle_translation(
        Vec2::new(1.0 / frames, 1.0),
        0.0,
        Vec2::new(frame as f32 / frames, 0.0),
    )
}

/// An effect's quad, removed at `until` (seconds since startup).
#[derive(Component)]
struct Effect {
    until: f32,
}

/// Turns to face the camera, rolled by `roll` radians about the view.
#[derive(Component)]
struct FacesCamera {
    roll: f32,
}

/// Stays where an attachment is (a muzzle flash on its gun).
#[derive(Component)]
struct AtAttachment(Entity);

/// A tracer in flight, metres.
#[derive(Component)]
struct Tracer {
    start: Vec3,
    direction: Vec3,
    /// How far the bullet went.
    distance: f32,
    /// How far the tracer's head has come.
    travelled: f32,
    length: f32,
    width: f32,
    speed: f32,
}

/// Starts loading every sprite `effects.ron` names, once the game's data is
/// known, and makes the quad they are drawn on.
fn load_sprites(
    game: Game,
    mut sprites: ResMut<Sprites>,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if sprites.quad.is_some() {
        return;
    }
    let Some(sim) = game.sim() else {
        return;
    };
    for name in sprite_names(&sim.data().effects) {
        let handle = asset_server.load(format!("{SOURCE}://materials/{name}.vmt"));
        sprites.handles.insert(name.to_owned(), handle);
    }
    sprites.quad = Some(meshes.add(Rectangle::new(1.0, 1.0)));
}

/// Every sprite the effects use.
fn sprite_names(effects: &EffectsConfig) -> impl Iterator<Item = &str> {
    effects
        .muzzle_flash
        .sprites
        .iter()
        .chain([&effects.tracer.sprite, &effects.impact.sprite])
        .map(String::as_str)
}

/// Where a gun's muzzle is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Muzzle {
    /// The view model's attachment, seen by the view model camera.
    View(Entity),
    /// A world model's attachment.
    World(Entity),
}

/// The muzzle of `shooter`'s gun: the view model's if they are the local
/// player seeing it, else the weapon in their model's hands.
fn find_muzzle(
    shooter: PlayerId,
    local: Option<PlayerId>,
    attachments: &Query<(Entity, &Attachment, &GlobalTransform)>,
    parents: &Query<&ChildOf>,
    owners: &Query<(Option<&PlayerView>, Has<ViewModel>)>,
) -> Option<Muzzle> {
    let mut world = None;
    for (entity, attachment, _) in attachments {
        if attachment.0 != MUZZLE {
            continue;
        }
        let mut at = entity;
        for _ in 0..MAX_DEPTH {
            if let Ok((view, is_view_model)) = owners.get(at) {
                if is_view_model && local == Some(shooter) {
                    return Some(Muzzle::View(entity));
                }
                if view.is_some_and(|view| view.0 == shooter) {
                    world = Some(Muzzle::World(entity));
                    break;
                }
            }
            let Ok(parent) = parents.get(at) else {
                break;
            };
            at = parent.parent();
        }
    }
    world
}

/// The point the main camera sees where the view model camera, with its
/// narrower view, draws `point`: pushed out from the view's centre by the
/// ratio of their fields of view, so a tracer starts where the muzzle
/// shows on screen.
fn as_seen_by_main(point: Vec3, camera: &GlobalTransform, main_fov: f32) -> Vec3 {
    let ratio = (main_fov / 2.0).tan() / (VIEW_FOV_DEGREES.to_radians() / 2.0).tan();
    let local = camera.affine().inverse().transform_point3(point);
    camera.transform_point(Vec3::new(local.x * ratio, local.y * ratio, local.z))
}

/// The quad of an effect: drawn with `material`, scaled to `size` metres.
fn quad_bundle(
    quad: &Handle<Mesh>,
    material: Handle<StandardMaterial>,
    at: Vec3,
    size: Vec2,
    until: f32,
) -> impl Bundle {
    (
        Mesh3d(quad.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(at).with_scale(size.extend(1.0)),
        Effect { until },
        MapScoped,
    )
}

/// What spawning effects reads about the scene.
#[derive(bevy::ecs::system::SystemParam)]
struct Scene<'w, 's> {
    attachments: Query<'w, 's, (Entity, &'static Attachment, &'static GlobalTransform)>,
    parents: Query<'w, 's, &'static ChildOf>,
    owners: Query<'w, 's, (Option<&'static PlayerView>, Has<ViewModel>)>,
    camera: Query<'w, 's, (&'static GlobalTransform, &'static Projection), With<FlyCam>>,
}

impl Scene<'_, '_> {
    /// Where `shooter`'s gun fires from, as the main camera sees it, and the
    /// muzzle attachment it is at, if their model has one.
    fn muzzle(
        &self,
        sim: &Simulation,
        shooter: PlayerId,
        local: Option<PlayerId>,
    ) -> (Option<Muzzle>, Vec3) {
        let muzzle = find_muzzle(
            shooter,
            local,
            &self.attachments,
            &self.parents,
            &self.owners,
        );
        let at = muzzle.and_then(|muzzle| {
            let (Muzzle::View(entity) | Muzzle::World(entity)) = muzzle;
            let (_, _, global) = self.attachments.get(entity).ok()?;
            Some(match (muzzle, self.camera.single()) {
                (Muzzle::View(_), Ok((camera, Projection::Perspective(view)))) => {
                    as_seen_by_main(global.translation(), camera, view.fov)
                }
                _ => global.translation(),
            })
        });
        (muzzle, at.unwrap_or_else(|| fallback_muzzle(sim, shooter)))
    }
}

/// Where the gun of a player whose model has no muzzle is: ahead of,
/// right of and below their eyes.
fn fallback_muzzle(sim: &Simulation, player: PlayerId) -> Vec3 {
    let (Some(state), Some(eye)) = (sim.player(player), sim.eye_position(player)) else {
        return Vec3::ZERO;
    };
    let (pitch, yaw) = state.look;
    let forward = view_direction(pitch, yaw);
    let right = Vec3::new(yaw.to_radians().sin(), -yaw.to_radians().cos(), 0.0);
    let at =
        eye + forward * FALLBACK_MUZZLE.x + right * FALLBACK_MUZZLE.y - Vec3::Z * FALLBACK_MUZZLE.z;
    coords::position(at.to_array())
}

/// What spawning effects writes with.
#[derive(bevy::ecs::system::SystemParam)]
struct Painter<'w, 's> {
    commands: Commands<'w, 's>,
    time: Res<'w, Time>,
    sprites: ResMut<'w, Sprites>,
    sprite_assets: Res<'w, Assets<SpriteAsset>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
}

impl Painter<'_, '_> {
    /// A muzzle flash at `at`, kept on `muzzle` if there is one.
    fn flash(
        &mut self,
        quad: &Handle<Mesh>,
        flash: &MuzzleFlashConfig,
        muzzle: Option<Muzzle>,
        at: Vec3,
    ) {
        let name = &flash.sprites[self.sprites.rng.below(flash.sprites.len())];
        let Some(material) = self
            .sprites
            .still(name, &self.sprite_assets, &mut self.materials)
        else {
            return;
        };
        let view = matches!(muzzle, Some(Muzzle::View(_)));
        let size = if view { flash.view_size } else { flash.size } * coords::METERS_PER_UNIT;
        let roll = self.sprites.rng.unit() * std::f32::consts::TAU;
        let until = self.time.elapsed_secs() + flash.duration;
        let mut entity = self.commands.spawn((
            quad_bundle(quad, material, at, Vec2::splat(size), until),
            FacesCamera { roll },
        ));
        match muzzle {
            Some(Muzzle::View(attachment)) => {
                entity.insert((AtAttachment(attachment), RenderLayers::layer(VIEW_LAYER)));
            }
            Some(Muzzle::World(attachment)) => {
                entity.insert(AtAttachment(attachment));
            }
            None => {}
        }
    }

    /// A tracer flying from `start` to `end`.
    fn tracer(&mut self, quad: &Handle<Mesh>, tracer: &TracerConfig, start: Vec3, end: Vec3) {
        let line = end - start;
        let Some(direction) = line.try_normalize() else {
            return;
        };
        let Some(material) =
            self.sprites
                .still(&tracer.sprite, &self.sprite_assets, &mut self.materials)
        else {
            return;
        };
        let scale = coords::METERS_PER_UNIT;
        self.commands.spawn((
            quad_bundle(quad, material, start, Vec2::ZERO, f32::INFINITY),
            Tracer {
                start,
                direction,
                distance: line.length(),
                travelled: 0.0,
                length: tracer.length * scale,
                width: tracer.width * scale,
                speed: tracer.speed * scale,
            },
        ));
    }

    /// Sparks at `at`.
    fn sparks(&mut self, quad: &Handle<Mesh>, impact: &SpriteEffectConfig, at: Vec3) {
        let Some(material) =
            self.sprites
                .still(&impact.sprite, &self.sprite_assets, &mut self.materials)
        else {
            return;
        };
        let roll = self.sprites.rng.unit() * std::f32::consts::TAU;
        let size = Vec2::splat(impact.size * coords::METERS_PER_UNIT);
        let until = self.time.elapsed_secs() + impact.duration;
        self.commands.spawn((
            quad_bundle(quad, material, at, size, until),
            FacesCamera { roll },
        ));
    }
}

/// Whether a weapon fires bullets (some fire mode of it is hitscan).
fn fires_bullets(sim: &Simulation, weapon: WeaponId) -> bool {
    sim.arsenal().weapon(weapon).is_some_and(|weapon| {
        weapon
            .fire_modes()
            .any(|fire| matches!(fire.shot, Shot::Hitscan(_)))
    })
}

/// Turns the tick events' shots and explosions into effects.
fn spawn_effects(
    mut ticks: MessageReader<Ticked>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    mut blasts: MessageWriter<Blast>,
    mut painter: Painter,
    scene: Scene,
) {
    let (Some(sim), Some(quad)) = (game.sim(), painter.sprites.quad.clone()) else {
        ticks.clear();
        return;
    };
    let effects = &sim.data().effects;
    let local = local.map(|local| local.id);
    for Ticked(events) in ticks.read() {
        // Where each shooter's gun is this tick, for flashes and tracers.
        let mut muzzles: HashMap<PlayerId, (Option<Muzzle>, Vec3)> = HashMap::new();
        let mut muzzle_of = |player| {
            *muzzles
                .entry(player)
                .or_insert_with(|| scene.muzzle(sim, player, local))
        };
        for &(player, weapon, mode) in &events.fired {
            let bullets = sim
                .arsenal()
                .fire(weapon, mode)
                .is_some_and(|fire| matches!(fire.shot, Shot::Hitscan(_)));
            if bullets {
                let (muzzle, at) = muzzle_of(player);
                painter.flash(&quad, &effects.muzzle_flash, muzzle, at);
            }
        }
        for shot in &events.shots {
            let end = coords::position(shot.trace.end.to_array());
            if fires_bullets(sim, shot.weapon) {
                let (_, start) = muzzle_of(shot.player);
                painter.tracer(&quad, &effects.tracer, start, end);
            }
            if !matches!(shot.trace.struck, Struck::Nothing | Struck::Player(_)) {
                let lift = coords::direction(shot.trace.normal.to_array())
                    * IMPACT_LIFT
                    * coords::METERS_PER_UNIT;
                painter.sparks(&quad, &effects.impact, end + lift);
            }
        }
        for explosion in &events.explosions {
            blasts.write(Blast {
                at: coords::position(explosion.position.to_array()),
                radius: explosion.radius,
            });
        }
    }
}

/// Moves tracers along their bullets' paths, stretched between their head
/// and tail; removes them once their tail arrives.
fn fly_tracers(
    mut commands: Commands,
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut tracers: Query<(Entity, &mut Tracer, &mut Transform)>,
) {
    let eye = camera
        .single()
        .map_or(Vec3::ZERO, GlobalTransform::translation);
    for (entity, mut tracer, mut transform) in &mut tracers {
        tracer.travelled += tracer.speed * time.delta_secs();
        let Some((middle, rotation, length)) = tracer_pose(&tracer, eye) else {
            commands.entity(entity).try_despawn();
            continue;
        };
        transform.translation = middle;
        transform.rotation = rotation;
        transform.scale = Vec3::new(tracer.width, length, 1.0);
    }
}

/// A tracer's middle, turn and drawn length, seen from `eye`: its quad's
/// height along the bullet's line, its face turned about that line toward
/// the eye. `None` once its tail has reached the end.
fn tracer_pose(tracer: &Tracer, eye: Vec3) -> Option<(Vec3, Quat, f32)> {
    let head = tracer.travelled.min(tracer.distance);
    let tail = (tracer.travelled - tracer.length).max(0.0);
    if tail >= tracer.distance {
        return None;
    }
    let middle = tracer.start + tracer.direction * (head + tail) / 2.0;
    let along = tracer.direction;
    let to_eye = eye - middle;
    let facing = (to_eye - along * to_eye.dot(along))
        .try_normalize()
        .unwrap_or_else(|| along.any_orthonormal_vector());
    let side = along.cross(facing);
    let rotation = Quat::from_mat3(&Mat3::from_cols(side, along, facing));
    Some((middle, rotation, head - tail))
}

/// Keeps muzzle flashes on their guns.
fn follow_attachments(
    attachments: Query<&GlobalTransform, With<Attachment>>,
    mut effects: Query<(&AtAttachment, &mut Transform)>,
) {
    for (at, mut transform) in &mut effects {
        if let Ok(attachment) = attachments.get(at.0) {
            transform.translation = attachment.translation();
        }
    }
}

/// Turns sprites to face the camera.
fn face_camera(
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut sprites: Query<(&FacesCamera, &mut Transform)>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    let (_, view, _) = camera.to_scale_rotation_translation();
    for (faces, mut transform) in &mut sprites {
        transform.rotation = view * Quat::from_rotation_z(faces.roll);
    }
}

/// Removes effects whose time is up.
fn expire(mut commands: Commands, time: Res<Time>, effects: Query<(Entity, &Effect)>) {
    let now = time.elapsed_secs();
    for (entity, effect) in &effects {
        if now >= effect.until {
            commands.entity(entity).try_despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_picked_by_shifting_texture_coordinates() {
        let uv = frame_uv(2, 4);
        assert_eq!(uv.transform_point2(Vec2::ZERO), Vec2::new(0.5, 0.0));
        assert_eq!(uv.transform_point2(Vec2::ONE), Vec2::new(0.75, 1.0));
        assert_eq!(frame_uv(0, 1), Affine2::IDENTITY);
    }

    fn tracer(travelled: f32) -> Tracer {
        Tracer {
            start: Vec3::ZERO,
            direction: Vec3::X,
            distance: 10.0,
            travelled,
            length: 2.0,
            width: 0.1,
            speed: 100.0,
        }
    }

    #[test]
    fn tracers_stretch_between_head_and_tail() {
        let eye = Vec3::new(5.0, 0.0, 3.0);
        let (middle, rotation, length) = tracer_pose(&tracer(1.0), eye).expect("in flight");
        assert_eq!((middle, length), (Vec3::new(0.5, 0.0, 0.0), 1.0));
        // The quad's height runs along the line; its face looks at the eye.
        assert!((rotation * Vec3::Y).abs_diff_eq(Vec3::X, 1e-5));
        assert!((rotation * Vec3::Z).abs_diff_eq(Vec3::Z, 1e-5));
        let (middle, _, length) = tracer_pose(&tracer(6.0), eye).expect("in flight");
        assert_eq!((middle, length), (Vec3::new(5.0, 0.0, 0.0), 2.0));
        let (middle, _, length) = tracer_pose(&tracer(11.0), eye).expect("arriving");
        assert_eq!((middle, length), (Vec3::new(9.5, 0.0, 0.0), 1.0));
        assert!(tracer_pose(&tracer(12.0), eye).is_none());
    }

    #[test]
    fn view_model_points_move_out_by_the_ratio_of_views() {
        let camera = GlobalTransform::IDENTITY;
        let fov = VIEW_FOV_DEGREES.to_radians();
        let point = Vec3::new(0.1, -0.1, -1.0);
        assert!(as_seen_by_main(point, &camera, fov).abs_diff_eq(point, 1e-6));
        let wider = as_seen_by_main(point, &camera, 2.0 * fov);
        assert!(wider.x > point.x && wider.y < point.y && (wider.z - point.z).abs() < 1e-6);
    }

    #[test]
    fn every_configured_sprite_is_loaded() {
        let data = rl_data::GameData::builtin().expect("builtin data");
        let names: Vec<&str> = sprite_names(&data.effects).collect();
        assert!(names.contains(&"effects/muzzleflash1"));
    }
}
