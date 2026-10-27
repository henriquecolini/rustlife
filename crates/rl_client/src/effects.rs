//! Combat effects: muzzle flashes, tracers, bullet impacts and explosions.
//!
//! Each is one of the mounted game's materials, as `effects.ron` names
//! them, drawn on a quad facing the camera:
//!
//! - a **muzzle flash** when a gun fires, at its model's `muzzle`
//!   attachment: the view model's in first person (drawn on its layer, so
//!   it sits on the weapon in hand), the world model's for everyone else;
//! - a **tracer** flying from the muzzle to where each bullet stopped, a
//!   streak turned about its line to face the camera;
//! - **impact spray** where a bullet or swing strikes something, by its
//!   surface class (concrete, metal, wood, glass, ...);
//! - **blood** spray when a bullet strikes a player;
//! - an **explosion**, which [`crate::particles`] draws.
//!
//! A sprite's frames lie side by side in one image (see
//! [`rl_assets::SpriteAsset`]); a material shows one by shifting its
//! texture coordinates. The marks themselves are projected meshes; see
//! [`crate::decals`].

use std::collections::HashMap;

use bevy::camera::visibility::RenderLayers;
use bevy::math::Affine2;
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_data::{
    EffectsConfig, ImpactEffectConfig, ImpactFlashConfig, MuzzleFlashConfig, Shot, TracerConfig,
};
use rl_sim::collision::contents::MASK_PLAYER_SOLID;
use rl_sim::collision::{CollisionQuery, Hull};
use rl_sim::combat::hitscan::Struck;
use rl_sim::movement::view_direction;
use rl_sim::rng::Rng;
use rl_sim::scene::Scene as WorldScene;
use rl_sim::simulation::{PlayerId, ShotEvent, Simulation};
use rl_sim::weapons::WeaponId;

use crate::AppState;
use crate::actors::PlayerView;
use crate::animation::Attachment;
use crate::camera::FlyCam;
use crate::decals::{DecalKind, DecalRequest, trace_stain};
use crate::net::Game;
use crate::particles::Blast;
use crate::pcf_particles::{Pcf, PcfSpawn};
use crate::player::{LocalPlayer, Ticked};
use crate::surfaces::{MapSurfaces, class_of_prop};
use crate::viewmodel::{VIEW_FOV_DEGREES, VIEW_LAYER, ViewModel};
use crate::world::MapScoped;

/// The attachment guns fire from.
const MUZZLE: &str = "muzzle";
/// Where a gun without a muzzle attachment fires from, relative to the
/// eyes: forward, right and down, units.
const FALLBACK_MUZZLE: Vec3 = Vec3::new(24.0, 5.0, 6.0);
/// How far off a struck surface particles are drawn, units, so they do not
/// cut into it.
const IMPACT_LIFT: f32 = 1.0;
/// Gravity pulling impact spray down, metres/s².
const SPRAY_GRAVITY: f32 = -9.8;
/// How quickly the air slows spray, per second.
const SPRAY_DRAG: f32 = 2.5;
/// How wide the cone of spray spreads around its main direction.
const SPRAY_SPREAD: f32 = 0.6;
/// Most spray particles one hit spawns.
const MAX_SPRAY: u32 = 16;
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
                fly_spray,
                follow_attachments,
                animate_flashes,
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
    /// A material per sprite, tint and lighting, showing its first frame,
    /// made once it loads.
    materials: HashMap<(String, [u32; 3], bool), Handle<StandardMaterial>>,
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
        self.still_tinted(name, [1.0; 3], false, sprites, materials)
    }

    /// The same, multiplied by `color` and lit by the scene if `lit`.
    fn still_tinted(
        &mut self,
        name: &str,
        color: [f32; 3],
        lit: bool,
        sprites: &Assets<SpriteAsset>,
        materials: &mut Assets<StandardMaterial>,
    ) -> Option<Handle<StandardMaterial>> {
        let key = (name.to_owned(), color.map(f32::to_bits), lit);
        if let Some(material) = self.materials.get(&key) {
            return Some(material.clone());
        }
        let sprite = sprites.get(self.handles.get(name)?)?;
        let material = materials.add(sprite_material(sprite, color, lit));
        self.materials.insert(key, material.clone());
        Some(material)
    }
}

/// How a sprite is drawn: unlit, both sides, added to what is behind it or
/// blended over it, showing its first frame.
fn sprite_material(sprite: &SpriteAsset, color: [f32; 3], lit: bool) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::linear_rgb(color[0], color[1], color[2]),
        base_color_texture: Some(sprite.image.clone()),
        unlit: !lit,
        perceptual_roughness: 1.0,
        reflectance: 0.0,
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

/// A flash lying on a struck surface (the game's `FX_AddQuad`): its size and
/// opacity move from a start to an end along bias curves.
#[derive(Component)]
struct SurfaceFlash {
    age: f32,
    life: f32,
    /// Half-size at birth and death, metres.
    size: (f32, f32),
    size_bias: f32,
    alpha: (f32, f32),
    alpha_bias: f32,
    material: Handle<StandardMaterial>,
}

/// The game's `Bias`: `t^-log2(bias)`. Below one half the value lags behind
/// `t`, above it runs ahead.
fn bias(t: f32, bias: f32) -> f32 {
    t.clamp(0.0, 1.0).powf(-bias.log2())
}

/// Turns to face the camera, rolled by `roll` radians about the view.
#[derive(Component)]
struct FacesCamera {
    roll: f32,
}

/// Stays where an attachment is (a muzzle flash on its gun).
#[derive(Component)]
struct AtAttachment(Entity);

/// A sprite flung off a struck surface, metres and seconds.
#[derive(Component)]
struct Spray {
    velocity: Vec3,
    /// Downward acceleration, metres/s².
    gravity: f32,
    age: f32,
    life: f32,
    /// Size at birth and at death, metres.
    size: (f32, f32),
    /// Turn, radians/s.
    spin: f32,
}

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
    let effects = &sim.data().effects;
    for name in sprite_names(effects) {
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
        .chain([&effects.tracer.sprite])
        .chain(
            effects
                .bullet_impacts
                .values()
                .chain([&effects.blood])
                .flat_map(|impact| &impact.sprites),
        )
        .chain(effects.weapon_flashes.values().map(|flash| &flash.sprite))
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

    /// Spray where a bullet or swing struck a surface.
    fn surface_impact(
        &mut self,
        quad: &Handle<Mesh>,
        impact: &ImpactEffectConfig,
        at: Vec3,
        normal: Vec3,
    ) {
        self.spray(quad, impact, at, normal);
    }

    /// A flat flash on the surface struck at `at`, facing out along `normal`.
    fn surface_flash(
        &mut self,
        quad: &Handle<Mesh>,
        flash: &ImpactFlashConfig,
        at: Vec3,
        normal: Vec3,
    ) {
        let Some(sprite) = self
            .sprites
            .handles
            .get(&flash.sprite)
            .and_then(|handle| self.sprite_assets.get(handle))
        else {
            return;
        };
        let scale = coords::METERS_PER_UNIT;
        let start = flash.size.0 + (flash.size.1 - flash.size.0) * self.sprites.rng.unit();
        let roll = self.sprites.rng.unit() * std::f32::consts::TAU;
        let material = self.materials.add(sprite_material(sprite, [1.0; 3], false));
        let until = self.time.elapsed_secs() + flash.life;
        let turn = Quat::from_rotation_arc(Vec3::Z, normal.normalize_or(Vec3::Z))
            * Quat::from_rotation_z(roll);
        self.commands
            .spawn(quad_bundle(quad, material.clone(), at, Vec2::ZERO, until))
            .insert((
                Transform::from_translation(at)
                    .with_rotation(turn)
                    .with_scale(Vec3::ZERO),
                SurfaceFlash {
                    age: 0.0,
                    life: flash.life,
                    size: (start * scale, flash.end_size * scale),
                    size_bias: flash.size_bias,
                    alpha: flash.alpha,
                    alpha_bias: flash.alpha_bias,
                    material,
                },
            ));
    }

    /// Blood spray thrown out of a wound.
    fn blood(
        &mut self,
        quad: &Handle<Mesh>,
        impact: &ImpactEffectConfig,
        at: Vec3,
        direction: Vec3,
    ) {
        self.spray(quad, impact, at, direction);
    }

    /// Spray sprites flying roughly along `away` from `at`.
    fn spray(&mut self, quad: &Handle<Mesh>, impact: &ImpactEffectConfig, at: Vec3, away: Vec3) {
        let away = away.try_normalize().unwrap_or(Vec3::Z);
        let (side, up) = away.any_orthonormal_pair();
        let scale = coords::METERS_PER_UNIT;
        for _ in 0..impact.particles.min(MAX_SPRAY) {
            let name = &impact.sprites[self.sprites.rng.below(impact.sprites.len())];
            let Some(material) = self.sprites.still_tinted(
                name,
                impact.color,
                impact.lit,
                &self.sprite_assets,
                &mut self.materials,
            ) else {
                continue;
            };
            let (x, y) = self.sprites.rng.in_disc();
            let direction = (away + (side * x + up * y) * SPRAY_SPREAD).normalize_or(away);
            let speed = impact.speed * scale * (0.45 + 0.75 * self.sprites.rng.unit());
            let life = impact.duration * (0.65 + 0.7 * self.sprites.rng.unit());
            let particle_size = impact.size * scale * (0.65 + 0.7 * self.sprites.rng.unit());
            let roll = self.sprites.rng.unit() * std::f32::consts::TAU;
            self.commands.spawn((
                quad_bundle(
                    quad,
                    material,
                    at,
                    Vec2::splat(particle_size),
                    f32::INFINITY,
                ),
                FacesCamera { roll },
                Spray {
                    velocity: direction * speed,
                    gravity: SPRAY_GRAVITY,
                    age: 0.0,
                    life,
                    size: (particle_size, particle_size * 0.35),
                    spin: (self.sprites.rng.unit() - 0.5) * 8.0,
                },
            ));
        }
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
    surfaces: Res<MapSurfaces>,
    mut blasts: MessageWriter<Blast>,
    mut decals: MessageWriter<DecalRequest>,
    pcf: Res<Pcf>,
    mut pcf_spawns: MessageWriter<PcfSpawn>,
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
            let bullets = fires_bullets(sim, shot.weapon);
            let end = coords::position(shot.trace.end.to_array());
            let normal = coords::direction(shot.trace.normal.to_array());
            if bullets {
                let (_, start) = muzzle_of(shot.player);
                painter.tracer(&quad, &effects.tracer, start, end);
            }
            let lift = normal * IMPACT_LIFT * coords::METERS_PER_UNIT;
            match shot.trace.struck {
                Struck::Nothing => {}
                Struck::Player(_) => {
                    let wound = Wound { shot, end, bullets };
                    bleed(
                        &wound,
                        sim,
                        &quad,
                        &mut painter,
                        (&pcf, &mut pcf_spawns),
                        &mut decals,
                    );
                }
                Struck::World | Struck::Prop(_) | Struck::Brush(_) => {
                    let prop = surfaces.of_shot(sim, &shot.trace);
                    let class = class_of_prop(prop);
                    let Some(impact) = effects.bullet_impact(class.name()) else {
                        continue;
                    };
                    let played = pcf.play(
                        impact.pcf.as_deref(),
                        &mut pcf_spawns,
                        shot.trace.end + shot.trace.normal * IMPACT_LIFT,
                        shot.trace.normal,
                    );
                    if !played {
                        painter.surface_impact(&quad, impact, end + lift, normal);
                    }
                    if bullets
                        && let Some(flash) = sim
                            .arsenal()
                            .weapon_name(shot.weapon)
                            .and_then(|weapon| effects.weapon_flashes.get(weapon))
                    {
                        painter.surface_flash(&quad, flash, end + lift, normal);
                    }
                    if bullets {
                        decals.write(DecalRequest {
                            kind: DecalKind::Bullet(class),
                            target: shot.trace.struck,
                            at: shot.trace.end,
                            normal: shot.trace.normal,
                            direction: (shot.trace.end - shot.start)
                                .normalize_or(shot.trace.normal),
                        });
                    }
                }
            }
        }
        for explosion in &events.explosions {
            let played = pcf.play(
                effects.explosion.pcf.as_deref(),
                &mut pcf_spawns,
                explosion.position,
                Vec3::Z,
            );
            blasts.write(Blast {
                at: coords::position(explosion.position.to_array()),
                radius: explosion.radius,
                procedural: !played,
            });
        }
    }
}

/// A bullet that struck a player.
struct Wound<'a> {
    shot: &'a ShotEvent,
    /// Where, Bevy space.
    end: Vec3,
    /// Whether it was fired by a gun (and so leaves a stain).
    bullets: bool,
}

/// Blood thrown out of a wound, and the stain on whatever lies beyond.
fn bleed(
    wound: &Wound,
    sim: &Simulation,
    quad: &Handle<Mesh>,
    painter: &mut Painter,
    (pcf, pcf_spawns): (&Pcf, &mut MessageWriter<PcfSpawn>),
    decals: &mut MessageWriter<DecalRequest>,
) {
    let Wound { shot, end, bullets } = wound;
    let (end, bullets) = (*end, *bullets);
    let blood = &sim.data().effects.blood;
    let source_direction = (shot.trace.end - shot.start).normalize_or(shot.trace.normal);
    let spray_direction = shot.trace.normal.normalize_or(source_direction);
    let played = pcf.play(
        blood.pcf.as_deref(),
        pcf_spawns,
        shot.trace.end,
        spray_direction,
    );
    if !played {
        painter.blood(
            quad,
            blood,
            end,
            coords::direction(spray_direction.to_array()),
        );
    }
    if bullets {
        stain_behind(sim, shot.trace.end, source_direction, decals);
    }
}

/// Asks for a blood stain on whatever lies beyond a wound at `at`, along
/// the bullet's `direction`.
fn stain_behind(
    sim: &Simulation,
    at: Vec3,
    direction: Vec3,
    decals: &mut MessageWriter<DecalRequest>,
) {
    let scene = WorldScene {
        world: sim.world(),
        physics: sim.physics(),
        ignore: None,
        others: &[],
    };
    let trace = |to| scene.trace_hull(at, to, &Hull::POINT, MASK_PLAYER_SOLID);
    if let Some((stain, normal, target)) = trace_stain(at, direction, trace) {
        decals.write(DecalRequest {
            kind: DecalKind::Blood,
            target,
            at: stain,
            normal,
            direction,
        });
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

/// Moves impact spray along its flight and shrinks it as it ages; removes
/// it when its life is up.
fn fly_spray(
    mut commands: Commands,
    time: Res<Time>,
    mut spray: Query<(Entity, &mut Spray, &mut Transform, &mut FacesCamera)>,
) {
    let dt = time.delta_secs();
    for (entity, mut spray, mut transform, mut faces) in &mut spray {
        spray.age += dt;
        if spray.age >= spray.life {
            commands.entity(entity).try_despawn();
            continue;
        }
        spray.velocity.y += spray.gravity * dt;
        spray.velocity *= (-SPRAY_DRAG * dt).exp();
        transform.translation += spray.velocity * dt;
        let t = (spray.age / spray.life).clamp(0.0, 1.0);
        let size = spray.size.0 + (spray.size.1 - spray.size.0) * t;
        transform.scale = Vec3::new(size, size, 1.0);
        faces.roll += spray.spin * dt;
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

/// Sizes and fades the flashes on struck surfaces along their curves.
fn animate_flashes(
    time: Res<Time>,
    mut flashes: Query<(&mut SurfaceFlash, &mut Transform)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (mut flash, mut transform) in &mut flashes {
        flash.age += time.delta_secs();
        let t = (flash.age / flash.life).clamp(0.0, 1.0);
        let half = flash.size.0 + (flash.size.1 - flash.size.0) * bias(t, flash.size_bias);
        transform.scale = Vec3::new(2.0 * half, 2.0 * half, 1.0);
        let alpha = flash.alpha.0 + (flash.alpha.1 - flash.alpha.0) * bias(t, flash.alpha_bias);
        if let Some(mut material) = materials.get_mut(&flash.material) {
            material.base_color = Color::linear_rgba(1.0, 1.0, 1.0, alpha);
        }
    }
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
    fn the_bias_curve_lags_below_one_half_and_runs_ahead_above_it() {
        assert_eq!(bias(0.0, 0.75), 0.0);
        assert_eq!(bias(1.0, 0.75), 1.0);
        assert!(
            (bias(0.5, 0.75) - 0.75).abs() < 1e-5,
            "bias b maps one half to b"
        );
        assert!((bias(0.5, 0.4) - 0.4).abs() < 1e-5);
        assert!(
            (bias(0.3, 0.5) - 0.3).abs() < 1e-5,
            "one half is the identity"
        );
    }

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
        assert!(names.contains(&"effects/fleck_cement1"));
        assert!(names.contains(&"effects/blood"));
        assert!(data.effects.bullet_impact("metal").is_some());
        assert!(data.effects.bullet_impact("nonsense").is_some(), "default");
    }
}
