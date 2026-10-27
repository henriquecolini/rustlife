//! Playing the game's particle systems on screen.
//!
//! [`PcfPlugin`] loads every `particles/*.pcf` of the mounted games into a
//! [`Library`]. Sending a [`PcfSpawn`] starts a system by name; the plugin
//! waits for the sprite materials it draws with to load (so sheet-driven
//! lifetimes are known), steps the simulation each frame and draws it: one
//! mesh per material and kind (sprites, trails, ropes) per effect, rebuilt
//! every frame with the quads facing the camera and sorted back to front.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_sim::collision::CollisionWorld;
use rl_sim::collision::contents::SOLID;

use super::draw::{self, Facing, Quad, RopePoint, View};
use super::sim::{
    Collider, ControlPoint, EmptyWorld, Hit, Instance, Library, Particle, Renderer, material_key,
};
use crate::camera::FlyCam;
use crate::net::Game;
use crate::world::MapScoped;
use crate::{AppState, GameVfs};

/// Longest an effect waits for its sprites to load before starting anyway,
/// seconds.
const LOAD_PATIENCE: f32 = 3.0;
/// Most effects alive at once; new ones are dropped beyond it.
const MAX_EFFECTS: usize = 64;

/// Plays particle systems from the game's `.pcf` files.
pub struct PcfPlugin;

impl Plugin for PcfPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PcfSpawn>()
            .init_resource::<Pcf>()
            .add_systems(
                Update,
                (load_library, play_scripted, start_effects, play_effects)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Starts the particle system `name` where something happened.
#[derive(Message, Debug, Clone)]
pub struct PcfSpawn {
    /// The system's name in the `.pcf` files, such as `explosion_huge`.
    pub name: String,
    /// Where, in Source units.
    pub position: Vec3,
    /// Which way the system's up points (a surface normal), Source space.
    pub normal: Vec3,
}

/// The loaded systems and the materials they draw with.
#[derive(Resource, Default)]
pub(crate) struct Pcf {
    library: Library,
    loaded: bool,
    sprites: HashMap<String, Handle<SpriteAsset>>,
    materials: HashMap<String, Handle<StandardMaterial>>,
    pending: Vec<(PcfSpawn, f32)>,
    spawned: u64,
}

impl Pcf {
    /// Whether the game's particle system `name` is loaded.
    pub(crate) fn has(&self, name: &str) -> bool {
        self.library.get(name).is_some()
    }

    /// Plays the system `name` at `position` (Source units) with its up along
    /// `normal`, if there is one, and says whether it did: when it did not,
    /// the caller draws its own effect.
    pub(crate) fn play(
        &self,
        name: Option<&str>,
        spawns: &mut MessageWriter<PcfSpawn>,
        position: Vec3,
        normal: Vec3,
    ) -> bool {
        let Some(name) = name.filter(|name| self.has(name)) else {
            return false;
        };
        spawns.write(PcfSpawn {
            name: name.to_owned(),
            position,
            normal,
        });
        true
    }

    /// The sprite for `material`, started loading if it was not yet.
    fn sprite(&mut self, material: &str, assets: &AssetServer) -> Handle<SpriteAsset> {
        self.sprites
            .entry(material.to_owned())
            .or_insert_with(|| assets.load(format!("{SOURCE}://materials/{material}.vmt")))
            .clone()
    }
}

/// A playing system and the meshes it is drawn on.
#[derive(Component)]
struct PcfEffect {
    instance: Instance,
    /// One drawing entity per `material|kind`.
    layers: HashMap<String, (Entity, Handle<Mesh>)>,
}

/// Looks the mounted games' particle files up once, on entering a map.
fn load_library(vfs: Res<GameVfs>, mut pcf: ResMut<Pcf>) {
    if pcf.loaded {
        return;
    }
    pcf.loaded = true;
    let mut unknown = Vec::new();
    for file in vfs.0.list("particles/*.pcf") {
        let Ok(Some(bytes)) = vfs.0.read(&file.path) else {
            continue;
        };
        match source_formats::pcf::parse(&bytes) {
            Ok(parsed) => unknown.extend(pcf.library.add(&parsed.systems())),
            Err(error) => warn!("{}: {error}", file.path),
        }
    }
    unknown.sort();
    unknown.dedup();
    if !unknown.is_empty() {
        debug!("particle operators not implemented: {unknown:?}");
    }
    info!("{} particle systems", pcf.library.len());
}

/// How long into a map a scripted capture waits before playing its system,
/// seconds, and how far in front of the eye it plays, Source units.
const SCRIPT_DELAY: f32 = 1.5;
const SCRIPT_DISTANCE: f32 = 300.0;

/// Plays the system a capture script (`--pcf`) asks for, once, where the view
/// points.
fn play_scripted(
    script: Option<Res<crate::script::InputScript>>,
    game: Game,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    time: Res<Time>,
    mut spawns: MessageWriter<PcfSpawn>,
    mut started: Local<Option<f32>>,
    mut done: Local<bool>,
) {
    let Some(name) = script.as_deref().and_then(|s| s.pcf.clone()) else {
        return;
    };
    let (Some(_), Ok(camera)) = (game.sim(), camera.single()) else {
        return;
    };
    let since = *started.get_or_insert(time.elapsed_secs());
    if *done || time.elapsed_secs() - since < SCRIPT_DELAY {
        return;
    }
    *done = true;
    let eye = Vec3::from(coords::to_source_position(camera.translation()));
    let forward = Vec3::from(coords::to_source_direction(camera.forward().into()));
    // Where a shot along the view would land, like a real impact: at the
    // surface hit, standing off it a little, facing out of it.
    let world = game.sim().map(rl_sim::simulation::Simulation::world);
    let trace = world.map(|world| world.trace_ray(eye, eye + forward * SCRIPT_DISTANCE, SOLID));
    let (position, normal) = match trace {
        Some(trace) if trace.hit() => (trace.end + trace.normal * 4.0, trace.normal),
        _ => (eye + forward * SCRIPT_DISTANCE - Vec3::Z * 20.0, Vec3::Z),
    };
    spawns.write(PcfSpawn {
        name,
        position,
        normal,
    });
}

/// Traces particles against the map.
struct WorldCollider<'a>(&'a CollisionWorld);

impl Collider for WorldCollider<'_> {
    fn trace(&self, from: Vec3, to: Vec3) -> Option<Hit> {
        let trace = self.0.trace_ray(from, to, SOLID);
        (trace.hit() && !trace.start_solid).then_some(Hit {
            point: trace.end,
            normal: trace.normal,
        })
    }
}

/// Turns spawn requests into effects once their sprites are in.
fn start_effects(
    mut commands: Commands,
    mut requests: MessageReader<PcfSpawn>,
    mut pcf: ResMut<Pcf>,
    assets: Res<AssetServer>,
    sprites: Res<Assets<SpriteAsset>>,
    time: Res<Time>,
    live: Query<(), With<PcfEffect>>,
) {
    pcf.pending
        .extend(requests.read().cloned().map(|request| (request, 0.0)));
    let mut room = MAX_EFFECTS.saturating_sub(live.iter().count());
    let mut waiting = Vec::new();
    for (request, waited) in std::mem::take(&mut pcf.pending) {
        let materials = pcf.library.materials(&request.name);
        let handles: Vec<_> = materials.iter().map(|m| pcf.sprite(m, &assets)).collect();
        let ready = handles.iter().all(|h| sprites.get(h).is_some());
        let failed = handles.iter().any(|h| assets.load_state(h).is_failed());
        if !ready && !failed && waited < LOAD_PATIENCE {
            waiting.push((request, waited + time.delta_secs()));
            continue;
        }
        if room == 0 {
            continue;
        }
        for (material, handle) in materials.iter().zip(&handles) {
            if let Some(sheet) = sprites.get(handle).and_then(|s| s.sheet.as_ref()) {
                pcf.library.set_sheet(material, sheet.frame_counts());
            }
        }
        pcf.spawned += 1;
        let point = ControlPoint::facing(request.position, request.normal);
        let Some(instance) = pcf.library.spawn(&request.name, &[point], pcf.spawned) else {
            warn!("no particle system called {}", request.name);
            continue;
        };
        room -= 1;
        commands.spawn((
            PcfEffect {
                instance,
                layers: HashMap::new(),
            },
            Transform::IDENTITY,
            Visibility::default(),
            MapScoped,
        ));
    }
    pcf.pending = waiting;
}

/// What a layer draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Sprite,
    Trail,
    Rope,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Sprite => "sprite",
            Self::Trail => "trail",
            Self::Rope => "rope",
        }
    }
}

/// Steps every effect and rebuilds its meshes.
fn play_effects(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    mut pcf: ResMut<Pcf>,
    sprites: Res<Assets<SpriteAsset>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut effects: Query<(Entity, &mut PcfEffect)>,
    mut layers: Query<&mut Visibility>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    let view = View {
        position: camera.translation(),
        right: camera.right().into(),
        up: camera.up().into(),
    };
    let pcf = &mut *pcf;
    let collider: Box<dyn Collider> = match game.sim() {
        Some(sim) => Box::new(WorldCollider(sim.world())),
        None => Box::new(EmptyWorld),
    };
    for (entity, mut effect) in &mut effects {
        let PcfEffect {
            instance,
            layers: layer_entities,
        } = &mut *effect;
        instance.update(&pcf.library, time.delta_secs(), collider.as_ref());
        if instance.finished() {
            commands.entity(entity).try_despawn();
            continue;
        }
        let mut quads: HashMap<String, (String, Kind, Vec<Quad>)> = HashMap::new();
        collect_quads(instance, &pcf.sprites, &sprites, view, &mut quads);
        for (layer, _) in layer_entities.values() {
            if let Ok(mut visibility) = layers.get_mut(*layer) {
                *visibility = Visibility::Hidden;
            }
        }
        for (key, (material, _, mut list)) in quads {
            let Some(sprite) = pcf.sprites.get(&material).and_then(|h| sprites.get(h)) else {
                continue;
            };
            let standard = pcf
                .materials
                .entry(material)
                .or_insert_with(|| materials.add(sprite_material(sprite)))
                .clone();
            list.sort_by(|a, b| {
                let da = a.center().distance_squared(view.position);
                let db = b.center().distance_squared(view.position);
                db.total_cmp(&da)
            });
            let mesh = build_mesh(&list, view);
            let (layer, handle) = layer_entities.entry(key).or_insert_with(|| {
                let handle = meshes.add(Mesh::new(
                    PrimitiveTopology::TriangleList,
                    RenderAssetUsages::default(),
                ));
                let layer = commands
                    .spawn((
                        Mesh3d(handle.clone()),
                        MeshMaterial3d(standard),
                        Transform::IDENTITY,
                        NoFrustumCulling,
                    ))
                    .id();
                commands.entity(entity).add_child(layer);
                (layer, handle)
            });
            if let Some(mut slot) = meshes.get_mut(&*handle) {
                *slot = mesh;
            }
            if let Ok(mut visibility) = layers.get_mut(*layer) {
                *visibility = Visibility::Inherited;
            }
        }
    }
}

/// How a sprite is drawn: unlit, both sides, vertex colour and alpha
/// multiplied in, added to what is behind it or blended over it.
fn sprite_material(sprite: &SpriteAsset) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(sprite.image.clone()),
        unlit: true,
        double_sided: true,
        cull_mode: None,
        alpha_mode: if sprite.additive {
            AlphaMode::Add
        } else {
            AlphaMode::Blend
        },
        ..default()
    }
}

/// Gathers the quads of `instance` and its children into `quads`, keyed by
/// layer.
fn collect_quads(
    instance: &Instance,
    handles: &HashMap<String, Handle<SpriteAsset>>,
    assets: &Assets<SpriteAsset>,
    view: View,
    quads: &mut HashMap<String, (String, Kind, Vec<Quad>)>,
) {
    let def = instance.def();
    let material = material_key(def.material());
    if let Some(sprite) = handles.get(&material).and_then(|h| assets.get(h)) {
        for renderer in def.renderers() {
            let (kind, list) = draw_particles(instance, renderer, sprite, view);
            if list.is_empty() {
                continue;
            }
            let key = format!("{material}|{}", kind.name());
            quads
                .entry(key)
                .or_insert_with(|| (material.clone(), kind, Vec::new()))
                .2
                .extend(list);
        }
    }
    for child in instance.children() {
        collect_quads(child, handles, assets, view, quads);
    }
}

fn world_position(p: &Particle) -> Vec3 {
    coords::position(p.position.to_array())
}

fn tint(p: &Particle, sprite: &SpriteAsset) -> [f32; 4] {
    [
        p.color[0] * sprite.overbright,
        p.color[1] * sprite.overbright,
        p.color[2] * sprite.overbright,
        p.alpha.clamp(0.0, 1.0),
    ]
}

/// The quads for `instance`'s particles under one renderer.
fn draw_particles(
    instance: &Instance,
    renderer: &Renderer,
    sprite: &SpriteAsset,
    view: View,
) -> (Kind, Vec<Quad>) {
    let sheet = sprite.sheet.as_ref();
    let scale = coords::METERS_PER_UNIT;
    match *renderer {
        Renderer::AnimatedSprite {
            rate,
            fit_lifetime,
            orientation,
            orientation_point,
        } => {
            let facing = match orientation {
                2 => Facing::Upright,
                3 => {
                    let point = usize::try_from(orientation_point)
                        .ok()
                        .and_then(|i| instance.points.get(i))
                        .or_else(|| instance.points.first());
                    point.map_or(Facing::Camera, |point| {
                        Facing::Plane(
                            coords::direction(point.right.to_array()),
                            coords::direction(point.forward.to_array()),
                        )
                    })
                }
                _ => Facing::Camera,
            };
            let quads = instance
                .particles()
                .iter()
                .filter(|p| p.alpha > 0.0 && p.radius > 0.0)
                .map(|p| {
                    let rect = draw::frame_rect(
                        sheet,
                        sprite.frames,
                        p.sequence,
                        p.age,
                        p.life_fraction(),
                        rate,
                        fit_lifetime,
                    );
                    draw::sprite(
                        world_position(p),
                        p.radius * scale,
                        p.rotation,
                        facing,
                        view,
                        rect,
                        tint(p, sprite),
                    )
                })
                .collect();
            (Kind::Sprite, quads)
        }
        Renderer::SpriteTrail {
            min_length,
            max_length,
            ..
        } => {
            let quads = instance
                .particles()
                .iter()
                .filter(|p| p.alpha > 0.0)
                .filter_map(|p| {
                    let rect = draw::frame_rect(
                        sheet,
                        sprite.frames,
                        p.sequence,
                        p.age,
                        p.life_fraction(),
                        0.0,
                        false,
                    );
                    draw::trail(
                        world_position(p),
                        coords::direction(p.velocity.to_array()),
                        p.velocity.length() * p.trail_length * scale,
                        (min_length * scale, max_length * scale),
                        p.radius * scale,
                        view.position,
                        rect,
                        tint(p, sprite),
                    )
                })
                .collect();
            (Kind::Trail, quads)
        }
        Renderer::Rope { .. } => {
            let mut ordered: Vec<&Particle> = instance.particles().iter().collect();
            ordered.sort_by_key(|p| p.id);
            let points: Vec<RopePoint> = ordered
                .iter()
                .map(|p| RopePoint {
                    position: world_position(p),
                    radius: p.radius * scale,
                    color: tint(p, sprite),
                })
                .collect();
            let rect = draw::frame_rect(sheet, sprite.frames, 0, 0.0, 0.0, 0.0, false);
            (Kind::Rope, draw::rope(&points, view.position, rect))
        }
    }
}

/// One triangle-list mesh of `quads`, in the order given.
fn build_mesh(quads: &[Quad], view: View) -> Mesh {
    let mut positions = Vec::with_capacity(quads.len() * 4);
    let mut uvs = Vec::with_capacity(quads.len() * 4);
    let mut colors = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    let normal = view.right.cross(view.up).to_array();
    for quad in quads {
        let base = positions.len() as u32;
        positions.extend(quad.corners.iter().map(Vec3::to_array));
        uvs.extend(quad.uvs);
        colors.extend([quad.color; 4]);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    let count = positions.len();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![normal; count])
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    .with_inserted_indices(Indices::U32(indices))
}

#[cfg(test)]
mod tests {
    use source_formats::pcf::ParticleSystem;

    use super::*;

    #[test]
    fn only_loaded_systems_are_available() {
        let mut pcf = Pcf::default();
        assert!(!pcf.has("impact_metal"));
        pcf.library.add(&[ParticleSystem {
            name: "impact_metal".into(),
            ..ParticleSystem::default()
        }]);
        assert!(pcf.has("impact_metal"));
        assert!(!pcf.has("impact_wood"));
    }

    #[test]
    fn a_mesh_has_four_vertices_and_six_indices_per_quad() {
        let view = View {
            position: Vec3::Z,
            right: Vec3::X,
            up: Vec3::Y,
        };
        let quad = draw::sprite(
            Vec3::ZERO,
            1.0,
            0.0,
            Facing::Camera,
            view,
            source_formats::sheet::Rect {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            },
            [1.0; 4],
        );
        let mesh = build_mesh(&[quad.clone(), quad], view);
        assert_eq!(mesh.count_vertices(), 8);
        assert_eq!(mesh.indices().map(Indices::len), Some(12));
    }
}
