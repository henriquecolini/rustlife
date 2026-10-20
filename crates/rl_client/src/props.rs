//! Placing the map's models: static props and prop entities.
//!
//! Once the map asset is ready, every [`PlacedProp`] becomes a root entity
//! with the prop's transform, and its model starts loading (each distinct
//! model once). When a model arrives, each of its meshes is attached as a
//! child `Surface`, so the world's material resolver textures it with the
//! skin's material like any world surface.
//!
//! Models are not lightmapped. A static prop compiled with baked per-vertex
//! lighting (`sp_<index>.vhv`, direct light and shadows included) draws a
//! copy of its meshes carrying that light as vertex colours, unlit, exactly
//! as lightmapped surfaces show `albedo × light`. Every other prop picks up
//! the map's ambient light from the irradiance volume.

use std::collections::HashMap;

use bevy::prelude::*;
use rl_assets::vertex_lighting::{colors_by_vertex, vertex_lit_mesh};
use rl_assets::{MapAsset, ModelAsset, PlacedProp, PropKind, VertexLightingAsset};

use crate::AppState;
use crate::world::{MapHandle, MapScoped, MaterialMaps, Placeholder, Surface, VertexLit};

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
                (spawn_props, attach_models)
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
}

/// Marks a [`Prop`] whose model has not been attached yet.
#[derive(Component)]
struct AwaitingModel;

/// Whether the current map's props have been spawned (one-shot guard).
#[derive(Resource, Default)]
struct PropsSpawned(bool);

/// One handle per distinct model, so props sharing a model share its assets.
#[derive(Resource, Default)]
struct ModelHandles(HashMap<String, Handle<ModelAsset>>);

/// Forgets the previous map's props and models on entering or leaving a map.
/// The entities themselves are [`MapScoped`] and despawned by the world.
fn reset(mut spawned: ResMut<PropsSpawned>, mut models: ResMut<ModelHandles>) {
    spawned.0 = false;
    models.0.clear();
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
    for prop in &map.props {
        let model = models
            .0
            .entry(prop.model.clone())
            .or_insert_with(|| asset_server.load(prop.model.clone()))
            .clone();
        commands.spawn((
            Prop {
                model,
                skin: prop.skin,
                kind: prop.kind,
                lighting: prop
                    .vertex_lighting
                    .as_ref()
                    .map(|path| asset_server.load(path.clone())),
            },
            AwaitingModel,
            transform_of(prop),
            Visibility::default(),
            MapScoped,
        ));
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

/// Gives every waiting prop whose model (and baked lighting, if any) has
/// loaded one child per model mesh, tagged with the skin's material for the
/// world's material resolver. Props whose model failed to load stop waiting
/// (the asset server has already logged why) and stay empty; props whose
/// lighting failed or does not fit the model fall back to the irradiance
/// volume.
fn attach_models(
    mut commands: Commands,
    waiting: Query<(Entity, &Prop), With<AwaitingModel>>,
    models: Res<Assets<ModelAsset>>,
    lightings: Res<Assets<VertexLightingAsset>>,
    mut meshes: ResMut<Assets<Mesh>>,
    asset_server: Res<AssetServer>,
    placeholder: Res<Placeholder>,
    mut material_maps: ResMut<MaterialMaps>,
) {
    for (entity, prop) in &waiting {
        let Some(model) = models.get(&prop.model) else {
            if asset_server.load_state(&prop.model).is_failed() {
                commands.entity(entity).remove::<AwaitingModel>();
            }
            continue;
        };
        let colors = match &prop.lighting {
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

        commands
            .entity(entity)
            .remove::<AwaitingModel>()
            .with_children(|parent| {
                for part in &model.parts {
                    let lit_mesh = colors
                        .as_ref()
                        .and_then(|colors| vertex_lit_mesh(meshes.get(&part.mesh)?, part, colors));
                    let lit = lit_mesh.is_some();
                    let mesh = lit_mesh.map_or_else(|| part.mesh.clone(), |mesh| meshes.add(mesh));
                    let mut child =
                        parent.spawn((Mesh3d(mesh), MeshMaterial3d(placeholder.0.clone())));
                    if lit {
                        child.insert(VertexLit);
                    }
                    if let Some(material) = model.material(prop.skin, part.material) {
                        material_maps.request(material, &asset_server);
                        child.insert(Surface(material.to_owned()));
                    }
                }
            });
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
        };
        let transform = transform_of(&prop);
        assert_eq!(transform.translation, prop.translation);
        assert_eq!(transform.rotation, prop.rotation);
        assert_eq!(transform.scale, Vec3::splat(2.0));
    }
}
