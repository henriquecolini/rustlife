//! Placing the map's models: static props and prop entities.
//!
//! Once the map asset is ready, every [`PlacedProp`] becomes a root entity
//! with the prop's transform, and its model starts loading (each distinct
//! model once). When a model arrives, each of its meshes is attached as a
//! child `Surface`, so the world's material resolver textures it with the
//! skin's material like any world surface.
//!
//! Models are not lightmapped. Each prop instead draws a copy of its meshes
//! carrying its light as vertex colours, unlit, exactly as lightmapped
//! surfaces show `albedo × light`:
//!
//! - a static prop compiled with baked per-vertex lighting (`sp_<index>.vhv`)
//!   uses vrad's colours, direct light and shadows included;
//! - every other prop is lit as the engine lights dynamic models: the ambient
//!   cube at its centre plus the world lights that can see it, evaluated per
//!   vertex normal.
//!
//! A prop for which neither is available falls back to the irradiance
//! volume.
//!
//! Every solid prop has a body in the game's physics world, made when the
//! level was loaded (`rl_level`) and numbered by placement, so the client
//! knows each prop's body from its place in the map's list
//! ([`LevelBodies`]). Props that can move carry a [`PhysicsBody`] and are
//! drawn each frame where [`Drawn`] puts them: interpolated between the
//! server's snapshots, a little in the past. The prop the local player
//! holds is the exception: it is drawn where the server last had it
//! relative to the player's view, moved with the view as it turns now, so
//! carrying feels immediate. A moving prop lit by the engine's model
//! lighting is relit when it has moved or turned noticeably, at most a few
//! times a second.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::camera::visibility::RenderLayers;
use bevy::mesh::skinning::SkinnedMesh;
use bevy::prelude::*;
use rl_assets::coords;
use rl_assets::light_field::LightField;
use rl_assets::vertex_lighting::{
    colors_by_vertex, cube_lit_mesh, field_lit_mesh, vertex_lit_mesh,
};
use rl_assets::{MapAsset, ModelAsset, PlacedProp, PropKind, VertexLightingAsset};
use rl_sim::physics::{BodyId, BodyPose};

use crate::AppState;
use crate::actors::PickupView;
use crate::animation::{Animated, SequencePlayer, spawn_skeleton};
use crate::net::{Drawn, Game, LevelBodies};
use crate::player::{LocalPlayer, Movement, ViewAngles};
use crate::world::{
    MapHandle, MapScoped, MaterialMaps, Placeholder, SKY_LAYER, Surface, VertexLit,
};

/// A moving prop is relit once it has moved this far since it was last
/// lit, metres (2 units)...
const RELIGHT_DISTANCE: f32 = 2.0 * coords::METERS_PER_UNIT;
/// ...or turned this far, radians. The light field is continuous, so small
/// steps change the lighting only a little each time...
const RELIGHT_ANGLE: f32 = 0.04;
/// ...but at most this often, seconds.
const RELIGHT_INTERVAL: f32 = 0.04;

/// Spawns and dresses the map's props while in [`AppState::InGame`].
pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PropsSpawned>()
            .init_resource::<ModelHandles>()
            .add_systems(OnEnter(AppState::InGame), reset)
            .add_systems(OnExit(AppState::InGame), reset)
            .add_systems(
                Update,
                (
                    spawn_props,
                    make_light_field,
                    attach_models,
                    add_bodies,
                    show_broken,
                    follow_bodies,
                    relight_moved_props,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// A placed model: the root entity of a static prop or prop entity.
#[derive(Component, Debug)]
pub struct Prop {
    /// The model drawn here.
    pub model: Handle<ModelAsset>,
    /// Which of the model's skins to draw.
    pub skin: usize,
    /// Static prop or which kind of entity.
    pub kind: PropKind,
    /// The prop's baked per-vertex lighting, if it has any.
    pub lighting: Option<Handle<VertexLightingAsset>>,
    /// Its index in the map's placements, which names its body; `None` for
    /// models the game places (pickups, projectiles, players).
    pub index: Option<usize>,
}

/// A model that moves (a physics prop, a projectile, another player): it is
/// relit as it goes.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct Moving;

/// Where a model is lit from, Bevy space, when that is not where its root
/// stands: a view model hangs off the camera but is lit where the player's
/// eyes are. Its rotation for lighting is its global one.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct LightAt(pub Vec3);

/// A prop that can move: its body in the game.
#[derive(Component, Debug, Clone, Copy)]
pub struct PhysicsBody(pub BodyId);

/// Marks a [`Prop`] whose model has not been attached yet.
#[derive(Component)]
struct AwaitingModel;

/// Marks a map [`Prop`] not yet matched with its body.
#[derive(Component)]
struct AwaitingBody;

/// A prop lit by the engine's model lighting, and where it was last lit.
#[derive(Component, Debug, Clone, Copy)]
struct CubeLit {
    translation: Vec3,
    rotation: Quat,
    at_secs: f32,
}

/// One mesh of a [`CubeLit`] prop: the unlit model mesh its lit copy is
/// rebuilt from.
#[derive(Component, Debug, Clone)]
struct LitPart {
    base: Handle<Mesh>,
}

/// Whether the current map's props have been spawned (one-shot guard).
#[derive(Resource, Default)]
struct PropsSpawned(bool);

/// One handle per distinct model, so props sharing a model share its assets.
#[derive(Resource, Default)]
pub(crate) struct ModelHandles(HashMap<String, Handle<ModelAsset>>);

impl ModelHandles {
    /// The handle of `model` (a path such as `models/items/battery.mdl`),
    /// loading it the first time.
    pub(crate) fn get(&mut self, model: &str, asset_server: &AssetServer) -> Handle<ModelAsset> {
        let path = format!("source://{model}");
        self.0
            .entry(path.clone())
            .or_insert_with(|| asset_server.load(path))
            .clone()
    }
}

/// Spawns a model drawn and lit like a prop, but placed by the simulation
/// rather than by physics (pickups, projectiles, other players): it gets no
/// body of its own. `transform` is in Bevy space; the entity is
/// [`MapScoped`].
pub(crate) fn spawn_model(
    commands: &mut Commands,
    models: &mut ModelHandles,
    asset_server: &AssetServer,
    model: &str,
    transform: Transform,
) -> Entity {
    commands
        .spawn((
            Prop {
                model: models.get(model, asset_server),
                skin: 0,
                kind: PropKind::Dynamic,
                lighting: None,
                index: None,
            },
            AwaitingModel,
            transform,
            Visibility::default(),
            MapScoped,
        ))
        .id()
}

/// Forgets the previous map's props and models on entering or leaving a map.
/// The entities themselves are [`MapScoped`] and despawned by the world.
fn reset(
    mut commands: Commands,
    mut spawned: ResMut<PropsSpawned>,
    mut models: ResMut<ModelHandles>,
) {
    spawned.0 = false;
    models.0.clear();
    commands.remove_resource::<MapLightField>();
}

/// Spawns a root entity for every prop of the loaded map.
fn spawn_props(
    mut commands: Commands,
    mut spawned: ResMut<PropsSpawned>,
    mut models: ResMut<ModelHandles>,
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    asset_server: Res<AssetServer>,
) {
    if spawned.0 {
        return;
    }
    let Some(map) = handle.and_then(|handle| maps.get(&handle.0)) else {
        return;
    };
    for (index, prop) in map.props.iter().enumerate() {
        let model = models
            .0
            .entry(prop.model.clone())
            .or_insert_with(|| asset_server.load(prop.model.clone()))
            .clone();
        let mut entity = commands.spawn((
            Prop {
                model,
                skin: prop.skin,
                kind: prop.kind,
                lighting: prop
                    .vertex_lighting
                    .as_ref()
                    .map(|path| asset_server.load(path.clone())),
                index: Some(index),
            },
            AwaitingModel,
            AwaitingBody,
            transform_of(prop),
            Visibility::default(),
            MapScoped,
        ));
        if prop.skybox {
            entity.insert(RenderLayers::layer(SKY_LAYER));
        }
    }
    info!(
        "placed {} props using {} models",
        map.props.len(),
        models.0.len()
    );
    spawned.0 = true;
}

/// The root transform of a prop.
fn transform_of(prop: &PlacedProp) -> Transform {
    Transform {
        translation: prop.translation,
        rotation: prop.rotation,
        scale: Vec3::splat(prop.scale),
    }
}

/// How a prop's meshes are lit.
enum PropLight {
    /// vrad's baked colour per model vertex (static props).
    Baked(Vec<[f32; 3]>),
    /// The map's light field, evaluated at every vertex (or once, for a
    /// model hung off another entity), and again as the prop moves.
    Field,
    /// Nothing usable: the irradiance volume lights the prop.
    Volume,
}

/// Gives every waiting prop whose model (and baked lighting, if any) has
/// loaded one child per model mesh, tagged with the skin's material for the
/// world's material resolver. Props whose model failed to load stop waiting
/// (the asset server has already logged why) and stay empty.
fn attach_models(
    mut commands: Commands,
    waiting: Query<(Entity, &Prop, &Transform, Option<&LightAt>), With<AwaitingModel>>,
    models: Res<Assets<ModelAsset>>,
    lightings: Res<Assets<VertexLightingAsset>>,
    field: Option<Res<MapLightField>>,
    mut meshes: ResMut<Assets<Mesh>>,
    asset_server: Res<AssetServer>,
    placeholder: Res<Placeholder>,
    mut material_maps: ResMut<MaterialMaps>,
) {
    let field = field.as_deref().map(|field| &field.0);
    for (entity, prop, transform, light_at) in &waiting {
        let transform = &lighting_transform(transform, light_at, None);
        let Some(model) = models.get(&prop.model) else {
            if asset_server.load_state(&prop.model).is_failed() {
                // Without its model a prop has no shape to collide with.
                commands.entity(entity).remove::<AwaitingModel>();
            }
            continue;
        };
        let baked = match &prop.lighting {
            None => None,
            Some(handle) => match lightings.get(handle) {
                Some(lighting) => {
                    let colors = colors_by_vertex(&lighting.0, model);
                    if colors.is_none() {
                        debug!("baked lighting does not fit {:?}", prop.model.path());
                    }
                    colors
                }
                None if asset_server.load_state(handle).is_failed() => None,
                None => continue,
            },
        };
        let uniform = light_at.is_some();
        let light = match baked {
            Some(colors) => PropLight::Baked(colors),
            None => field
                .as_ref()
                .filter(|field| field_mesh_ok(field, transform))
                .map_or(PropLight::Volume, |_| PropLight::Field),
        };

        // A skeletal model's joints, which its meshes are skinned to.
        let skin = model.animation.as_ref().map(|animation| {
            let joints = spawn_skeleton(&mut commands, entity, animation);
            commands.entity(entity).try_insert((
                Animated {
                    model: prop.model.clone(),
                    animation: Arc::clone(animation),
                },
                SequencePlayer::default(),
            ));
            SkinnedMesh {
                inverse_bindposes: animation.inverse_bindposes.clone(),
                joints,
            }
        });
        let mut root = commands.entity(entity);
        root.remove::<AwaitingModel>();
        if matches!(light, PropLight::Field) {
            root.insert(CubeLit {
                translation: transform.translation,
                rotation: transform.rotation,
                at_secs: 0.0,
            });
        }
        root.with_children(|parent| {
            for part in &model.parts {
                let lit_mesh = meshes.get(&part.mesh).and_then(|base| match &light {
                    PropLight::Baked(colors) => vertex_lit_mesh(base, part, colors),
                    PropLight::Field => {
                        field.and_then(|field| field_mesh(field, base, transform, uniform))
                    }
                    PropLight::Volume => None,
                });
                let lit = lit_mesh.is_some();
                let mesh = lit_mesh.map_or_else(|| part.mesh.clone(), |mesh| meshes.add(mesh));
                let mut child = parent.spawn((Mesh3d(mesh), MeshMaterial3d(placeholder.0.clone())));
                if let Some(skin) = &skin {
                    child.insert(skin.clone());
                }
                if lit {
                    child.insert(VertexLit);
                }
                if lit && matches!(light, PropLight::Field) {
                    child.insert(LitPart {
                        base: part.mesh.clone(),
                    });
                }
                if let Some(material) = model.material(prop.skin, part.material) {
                    material_maps.request(material, &asset_server);
                    child.insert(Surface(material.to_owned()));
                }
            }
        });
    }
}

/// Matches every map prop with its body once the game is entered; props
/// that can move get a [`PhysicsBody`].
fn add_bodies(
    mut commands: Commands,
    waiting: Query<(Entity, &Prop), With<AwaitingBody>>,
    bodies: Res<LevelBodies>,
    game: Game,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let physics = sim.physics();
    for (entity, prop) in &waiting {
        let mut root = commands.entity(entity);
        root.remove::<AwaitingBody>();
        let body = prop
            .index
            .and_then(|index| bodies.0.get(index).copied().flatten());
        if let Some(body) = body
            && (physics.is_dynamic(body)
                || physics.is_grabbable(body)
                || sim.breakable(body).is_some())
        {
            root.insert((PhysicsBody(body), Moving));
        }
    }
}

/// Hides the map props the server broke, and shows them again if they
/// respawn. Pickups are hidden by their own views while taken.
fn show_broken(
    game: Game,
    mut bodies: Query<(&PhysicsBody, &mut Visibility), Without<PickupView>>,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    for (body, mut visibility) in &mut bodies {
        let wanted = if sim.physics().is_broken(body.0) {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        visibility.set_if_neq(wanted);
    }
}

/// Places every moving prop where it is drawn this frame; the one the
/// local player holds, relative to their view.
fn follow_bodies(
    drawn: Res<Drawn>,
    game: Game,
    held: HeldView,
    mut bodies: Query<(&PhysicsBody, &mut Transform)>,
) {
    let Some(world) = &drawn.0 else {
        return;
    };
    let carried = held.carried(&game);
    for (body, mut transform) in &mut bodies {
        let pose = match carried {
            Some((id, pose)) if id == body.0 => pose,
            _ => match world.props.get(&body.0) {
                Some(pose) => *pose,
                None => continue,
            },
        };
        transform.translation = coords::position(pose.position.to_array());
        transform.rotation = coords::rotation_from_source(pose.rotation);
    }
}

/// What drawing the held prop by the view needs.
#[derive(bevy::ecs::system::SystemParam)]
struct HeldView<'w> {
    local: Option<Res<'w, LocalPlayer>>,
    angles: Res<'w, ViewAngles>,
    movement: Res<'w, Movement>,
    time: Res<'w, Time<Fixed>>,
}

impl HeldView<'_> {
    /// The prop the local player holds and where to draw it: where the
    /// newest snapshot has it relative to the player's eyes and view, moved
    /// to their predicted eyes and current view.
    fn carried(&self, game: &Game) -> Option<(BodyId, BodyPose)> {
        let local = self.local.as_ref()?;
        let session = game.session()?;
        let body = session.sim().player(local.id)?.manipulator.held()?;
        let latest = session.latest()?;
        let then = latest.players.get(&local.id)?;
        let pose = *latest.props.get(&body)?;
        let config = &self.movement.0;
        let eye = |state: &rl_sim::movement::PlayerState| {
            state.origin + Vec3::Z * state.eye_height(config)
        };
        let view = |pitch: f32, yaw: f32| {
            Quat::from_rotation_z(yaw.to_radians()) * Quat::from_rotation_y(pitch.to_radians())
        };
        let (pitch, yaw) = then.motion.look;
        let offset = view(pitch, yaw).inverse() * (pose.position - eye(&then.motion.state));
        let alpha = self.time.overstep_fraction();
        let eye_now =
            eye(&local.previous).lerp(eye(&local.current), alpha) + session.prediction_error();
        let turn = Quat::from_rotation_z((self.angles.yaw - yaw).to_radians());
        Some((
            body,
            BodyPose {
                position: eye_now + view(self.angles.pitch, self.angles.yaw) * offset,
                rotation: turn * pose.rotation,
            },
        ))
    }
}

/// What relighting a moving prop reads and writes.
type MovingProp<'a> = (
    &'a Transform,
    &'a GlobalTransform,
    Option<&'a LightAt>,
    &'a mut CubeLit,
    &'a Children,
);

/// Relights moving props that have moved or turned a little since they
/// were last lit, rebuilding their meshes' vertex colours in place from the
/// map's light field.
fn relight_moved_props(
    time: Res<Time>,
    mut props: Query<MovingProp, With<Moving>>,
    parts: Query<(&LitPart, &Mesh3d)>,
    field: Option<Res<MapLightField>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(field) = field else {
        return;
    };
    let now = time.elapsed_secs();
    for (transform, global, light_at, mut lit, children) in &mut props {
        let uniform = light_at.is_some();
        let transform = &lighting_transform(transform, light_at, Some(global));
        let moved = transform.translation.distance(lit.translation) > RELIGHT_DISTANCE
            || transform.rotation.angle_between(lit.rotation) > RELIGHT_ANGLE;
        if !moved || now - lit.at_secs < RELIGHT_INTERVAL {
            continue;
        }
        if !field_mesh_ok(&field.0, transform) {
            continue;
        }
        for child in children {
            let Ok((part, mesh)) = parts.get(*child) else {
                continue;
            };
            let relit = meshes
                .get(&part.base)
                .and_then(|base| field_mesh(&field.0, base, transform, uniform));
            if let Some(relit) = relit {
                let _ = meshes.insert(&mesh.0, relit);
            }
        }
        *lit = CubeLit {
            translation: transform.translation,
            rotation: transform.rotation,
            at_secs: now,
        };
    }
}

/// The map's light field, once the map has loaded.
#[derive(Resource, Clone)]
pub(crate) struct MapLightField(pub(crate) LightField);

/// Makes the light field when the map's lighting arrives.
fn make_light_field(
    mut commands: Commands,
    existing: Option<Res<MapLightField>>,
    map: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
) {
    if existing.is_some() {
        return;
    }
    if let Some(lighting) = map
        .and_then(|map| maps.get(&map.0))
        .and_then(|map| map.lighting.clone())
    {
        commands.insert_resource(MapLightField(LightField::new(lighting)));
    }
}

/// Whether there is any light at the lit point (so the field can serve).
fn field_mesh_ok(field: &LightField, transform: &Transform) -> bool {
    field
        .cube_at(coords::to_source_position(transform.translation))
        .is_some()
}

/// A copy of `base` lit from the field: per vertex for a model standing in
/// the world, one light for all of it for a model hung off another entity
/// (its vertices are not where it is lit).
fn field_mesh(
    field: &LightField,
    base: &Mesh,
    transform: &Transform,
    uniform: bool,
) -> Option<Mesh> {
    if uniform {
        let cube = field.cube_at(coords::to_source_position(transform.translation))?;
        cube_lit_mesh(base, transform.rotation, &cube)
    } else {
        field_lit_mesh(
            base,
            transform.translation,
            transform.rotation,
            transform.scale.x,
            field,
        )
    }
}

/// Where a model is lit as: its own placement, or — for one hanging off
/// another entity, with a [`LightAt`] — that point, turned as the model is
/// in the world.
fn lighting_transform(
    transform: &Transform,
    light_at: Option<&LightAt>,
    global: Option<&GlobalTransform>,
) -> Transform {
    match light_at {
        Some(at) => Transform {
            translation: at.0,
            rotation: global.map_or(transform.rotation, |global| {
                global.compute_transform().rotation
            }),
            scale: transform.scale,
        },
        None => *transform,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transforms_carry_placement_and_uniform_scale() {
        let prop = PlacedProp {
            model: "source://models/a.mdl".to_owned(),
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(1.0),
            scale: 2.0,
            skin: 0,
            kind: PropKind::Static,
            vertex_lighting: None,
            source: source_formats::props::PropPlacement::new(
                "models/a.mdl",
                [0.0; 3],
                [0.0; 3],
                PropKind::Static,
            ),
            skybox: false,
        };
        let transform = transform_of(&prop);
        assert_eq!(transform.translation, prop.translation);
        assert_eq!(transform.rotation, prop.rotation);
        assert_eq!(transform.scale, Vec3::splat(2.0));
    }
}
