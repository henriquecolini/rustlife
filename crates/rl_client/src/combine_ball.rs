//! The Pulse Rifle's energy ball bouncing and bursting, as HL2DM shows it.
//!
//! The server reports both ([`ProjectileEvent`]s of a projectile whose
//! effect is [`ProjectileEffect::CombineBall`]); what they look like is
//! `effects.ron`'s `combine_ball`, read from the game:
//!
//! - a **bounce** is the server's `cball_bounce` (`DoImpactEffect`) just off
//!   the surface, whose client callback lays two quads on it (the first
//!   `effects/combinemuzzle1_nocull` at ten times the radius of 16 the server
//!   gives, shrinking away in a quarter of a second; the second
//!   `effects/combinemuzzle2_nocull` growing from twice to four times it in
//!   half a second, both turned at random and fading out along their bias
//!   curves) and throws electric sparks off it (`FX_ElectricSpark`, magnitude
//!   2, trail 1). See [`crate::effects::FlatFlash`] and [`crate::trail_sparks`];
//! - a **burst** (`DoExplosion`, when its four seconds are up) is
//!   `cball_explode` (electric sparks of magnitude 4 thrown up), two
//!   `te->BeamRingPoint`s spreading level from the ball's radius to 1024 units
//!   while they fade (drawn here as the engine's `DrawRing` draws a beam ring,
//!   read from client.so: a ribbon around the level circle, turned to face the
//!   camera, reaching the beam's width either side of it, its texture running
//!   round it once every 800 units), and `UTIL_ScreenShake` for those near it.
//!
//! The sounds (`NPC_CombineBall.Impact` and `.Explosion`) are the weapon's
//! `bounce` and `burst` ([`crate::audio`]).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_data::{BeamRingConfig, ProjectileEffect, Shot};
use rl_sim::simulation::{ProjectileEvent, ProjectileEventKind, Simulation};

use crate::AppState;
use crate::camera::FlyCam;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::effects::FlatFlash;
use crate::lightning::{self, Point};
use crate::net::Game;
use crate::particles::Shake;
use crate::player::Ticked;
use crate::trail_sparks::{Burst, Sparks};
use crate::world::MapScoped;

/// How far off the surface the bounce's effect is drawn, units: the server
/// dispatches it at the trace's end moved one unit along the normal.
const BOUNCE_LIFT: f32 = 1.0;
/// How many points the ring is cut into.
const RING_POINTS: usize = 64;
/// How far along its texture a ring runs, all the way round, per unit of
/// radius: `DrawRing` (client.so) takes the ring's "length" as its diameter
/// times 0.03141593 and runs the texture an eighth of that round it, so it
/// repeats every 800 units of the circle.
const RING_TEXTURE_PER_RADIUS: f32 = 2.0 * 0.031_415_93 * 0.125;

/// Draws the energy ball's bounces and bursts.
pub(crate) struct CombineBallPlugin;

impl Plugin for CombineBallPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (start_effects, draw_rings)
                .chain()
                .after(crate::camera::CameraPlaced)
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// A ring spreading out.
#[derive(Component)]
struct Ring {
    config: BeamRingConfig,
    /// Its centre, Source units.
    center: Vec3,
    /// Seconds since it began.
    age: f32,
    /// How many frames its sprite has side by side.
    frames: u32,
    mesh: Handle<Mesh>,
    material: Handle<EffectMaterial>,
}

/// Whether `event` is of a projectile drawn as the energy ball.
fn is_combine_ball(sim: &Simulation, event: &ProjectileEvent) -> bool {
    matches!(
        sim.arsenal().fire(event.weapon, event.mode).map(|fire| &fire.shot),
        Some(Shot::Projectile(projectile))
            if projectile.effect == Some(ProjectileEffect::CombineBall)
    )
}

/// How hard the view shakes, `0..=1`, at `distance` units from a burst whose
/// shake is `strength` right at it and nothing at `radius`: the engine's
/// `UTIL_ScreenShake` fades its amplitude evenly out to its radius.
fn shake_at(strength: f32, radius: f32, distance: f32) -> f32 {
    strength * (1.0 - distance / radius.max(f32::EPSILON)).clamp(0.0, 1.0)
}

/// Starts the effects of every bounce and burst heard this frame.
#[allow(clippy::too_many_arguments)]
fn start_effects(
    mut commands: Commands,
    mut ticks: MessageReader<Ticked>,
    game: Game,
    asset_server: Res<AssetServer>,
    sprites: Res<Assets<SpriteAsset>>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
    mut flashes: MessageWriter<FlatFlash>,
    mut sparks: MessageWriter<Sparks>,
    mut shake: ResMut<Shake>,
    mut loaded: Local<HashMap<String, Handle<SpriteAsset>>>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    let Some(config) = sim.data().effects.combine_ball.as_ref() else {
        ticks.clear();
        return;
    };
    // The rings' sprites start loading with the first game.
    for ring in &config.rings {
        loaded.entry(ring.sprite.clone()).or_insert_with(|| {
            asset_server.load(format!("{SOURCE}://materials/{}.vmt", ring.sprite))
        });
    }
    for Ticked(events) in ticks.read() {
        for event in events
            .projectiles
            .iter()
            .filter(|e| is_combine_ball(sim, e))
        {
            debug!(kind = ?event.kind, position = ?event.position, "energy ball");
            let normal = coords::direction(event.normal.to_array());
            match event.kind {
                ProjectileEventKind::Bounce => {
                    let lifted = event.position + event.normal * BOUNCE_LIFT;
                    let at = coords::position(lifted.to_array());
                    for flash in &config.bounce_flashes {
                        flashes.write(FlatFlash {
                            flash: flash.clone(),
                            at,
                            normal,
                        });
                    }
                    let (magnitude, trail) = config.bounce_sparks;
                    sparks.write(Sparks {
                        at,
                        direction: event.normal,
                        burst: Burst::Electric { magnitude, trail },
                    });
                }
                ProjectileEventKind::Burst => {
                    let at = coords::position(event.position.to_array());
                    let (magnitude, trail) = config.burst_sparks;
                    sparks.write(Sparks {
                        at,
                        direction: Vec3::Z,
                        burst: Burst::Electric { magnitude, trail },
                    });
                    if let Ok(camera) = camera.single() {
                        let distance = camera.translation().distance(at) / coords::METERS_PER_UNIT;
                        shake.add(shake_at(config.shake, config.shake_radius, distance));
                    }
                    for ring in &config.rings {
                        let Some(sprite) = loaded.get(&ring.sprite).and_then(|h| sprites.get(h))
                        else {
                            debug!(sprite = ring.sprite, "the ring's sprite is not loaded");
                            continue;
                        };
                        // The texture is tiled around the ring.
                        if let Some(mut image) = images.get_mut(&sprite.image) {
                            image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                                address_mode_v: ImageAddressMode::Repeat,
                                ..ImageSamplerDescriptor::linear()
                            });
                        }
                        let material = materials.add(effect_material(StandardMaterial {
                            base_color: Color::NONE,
                            base_color_texture: Some(sprite.image.clone()),
                            unlit: true,
                            double_sided: true,
                            cull_mode: None,
                            alpha_mode: AlphaMode::Add,
                            ..default()
                        }));
                        let mesh = meshes.add(Mesh::new(
                            PrimitiveTopology::TriangleList,
                            RenderAssetUsages::default(),
                        ));
                        commands.spawn((
                            Ring {
                                config: ring.clone(),
                                center: event.position,
                                age: 0.0,
                                frames: sprite.frames.max(1),
                                mesh: mesh.clone(),
                                material: material.clone(),
                            },
                            Mesh3d(mesh),
                            MeshMaterial3d(material),
                            Transform::IDENTITY,
                            bevy::camera::visibility::NoFrustumCulling,
                            MapScoped,
                        ));
                    }
                }
            }
        }
    }
}

/// How far a ring has spread at `age` seconds, units: from its first radius
/// to its last over its life, evenly.
fn ring_radius(ring: &BeamRingConfig, age: f32) -> f32 {
    let t = (age / ring.life).clamp(0.0, 1.0);
    ring.radius.0 + (ring.radius.1 - ring.radius.0) * t
}

/// How bright a ring is at `age` seconds, `0..=1`: its alpha at birth,
/// fading to nothing at the end of its life (`FBEAM_FADEOUT`).
fn ring_alpha(ring: &BeamRingConfig, age: f32) -> f32 {
    ring.alpha * (1.0 - age / ring.life).clamp(0.0, 1.0)
}

/// The points of a ring of `radius` around `center`, level, closed (the
/// last point is the first), each as wide as the beam reaches either side.
/// Units in, units out.
fn ring_points(center: Vec3, radius: f32, width: f32) -> Vec<Point> {
    (0..=RING_POINTS)
        .map(|i| {
            let fraction = i as f32 / RING_POINTS as f32;
            let angle = fraction * std::f32::consts::TAU;
            Point {
                position: center + Vec3::new(angle.cos(), angle.sin(), 0.0) * radius,
                width: 2.0 * width,
                v: fraction * radius * RING_TEXTURE_PER_RADIUS,
            }
        })
        .collect()
}

/// Spreads and fades the rings, and removes the spent.
fn draw_rings(
    mut commands: Commands,
    time: Res<Time>,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut rings: Query<(Entity, &mut Ring)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
) {
    let Ok(camera) = camera.single() else {
        return;
    };
    let eye = camera.translation();
    let unit = coords::METERS_PER_UNIT;
    for (entity, mut ring) in &mut rings {
        ring.age += time.delta_secs();
        if ring.age >= ring.config.life {
            commands.entity(entity).try_despawn();
            meshes.remove(&ring.mesh);
            materials.remove(&ring.material);
            continue;
        }
        let radius = ring_radius(&ring.config, ring.age);
        // Bevy space, metres, from here on.
        let points: Vec<Point> = ring_points(ring.center, radius, ring.config.width)
            .into_iter()
            .map(|point| Point {
                position: coords::position(point.position.to_array()),
                width: point.width * unit,
                v: point.v,
            })
            .collect();
        let toward_camera = |at: Vec3| (eye - at).normalize_or_zero();
        let (positions, uvs, indices) = lightning::ribbon(&points, toward_camera);
        if let Some(mut mesh) = meshes.get_mut(&ring.mesh) {
            mesh.insert_attribute(
                Mesh::ATTRIBUTE_NORMAL,
                vec![[0.0, 0.0, 1.0]; positions.len()],
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            mesh.insert_indices(Indices::U32(indices));
        }
        let alpha = ring_alpha(&ring.config, ring.age);
        let frame = (ring.age * ring.config.framerate) as u32 % ring.frames;
        let [r, g, b] = ring.config.color;
        if let Some(mut material) = materials.get_mut(&ring.material) {
            material.base.base_color = Color::linear_rgba(r, g, b, alpha);
            let frames = ring.frames as f32;
            material.base.uv_transform = Affine2::from_scale_angle_translation(
                Vec2::new(1.0 / frames, 1.0),
                0.0,
                Vec2::new(frame as f32 / frames, 0.0),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> BeamRingConfig {
        BeamRingConfig {
            sprite: "sprites/lgtning".to_owned(),
            radius: (10.0, 1024.0),
            width: 64.0,
            color: [1.0, 1.0, 0.88],
            alpha: 0.25,
            life: 0.5,
            framerate: 2.0,
        }
    }

    #[test]
    fn a_ring_spreads_from_the_balls_radius_to_its_last_evenly() {
        let ring = ring();
        assert_eq!(ring_radius(&ring, 0.0), 10.0);
        assert!((ring_radius(&ring, 0.25) - 517.0).abs() < 1e-3);
        assert_eq!(ring_radius(&ring, 0.5), 1024.0);
        assert_eq!(ring_radius(&ring, 9.0), 1024.0, "no further");
    }

    #[test]
    fn a_ring_fades_out_over_its_life() {
        let ring = ring();
        assert_eq!(ring_alpha(&ring, 0.0), 0.25);
        assert!((ring_alpha(&ring, 0.25) - 0.125).abs() < 1e-6);
        assert_eq!(ring_alpha(&ring, 0.5), 0.0);
        assert_eq!(ring_alpha(&ring, 1.0), 0.0, "not below nothing");
    }

    #[test]
    fn a_ring_lies_level_around_its_centre_and_closes() {
        let center = Vec3::new(100.0, -50.0, 20.0);
        let points = ring_points(center, 300.0, 64.0);
        assert_eq!(points.len(), RING_POINTS + 1);
        for point in &points {
            assert!((point.position.distance(center) - 300.0).abs() < 1e-3);
            assert!((point.position.z - center.z).abs() < 1e-4, "level");
            assert_eq!(point.width, 128.0, "the width either side");
        }
        assert!(points[0].position.distance(points[RING_POINTS].position) < 1e-3);
        assert_eq!(points[0].v, 0.0);
        // 300 units of radius is 1885 round: the texture runs 2.36 times.
        assert!((points[RING_POINTS].v - 2.356).abs() < 1e-3);
    }

    #[test]
    fn the_shake_fades_evenly_to_nothing_at_its_radius() {
        assert_eq!(shake_at(0.68, 1250.0, 0.0), 0.68);
        assert!((shake_at(0.68, 1250.0, 625.0) - 0.34).abs() < 1e-6);
        assert_eq!(shake_at(0.68, 1250.0, 1250.0), 0.0);
        assert_eq!(shake_at(0.68, 1250.0, 5000.0), 0.0);
    }

    #[test]
    fn the_energy_ball_is_drawn_by_its_data() {
        let data = rl_data::GameData::builtin().expect("data");
        let ball = data.effects.combine_ball.as_ref().expect("combine_ball");
        assert_eq!(ball.bounce_flashes.len(), 2);
        assert_eq!(ball.rings.len(), 2);
        let rifle = data.weapons.get("rifle").expect("rifle");
        assert!(matches!(
            rifle.secondary.as_ref().map(|fire| &fire.shot),
            Some(Shot::Projectile(ball)) if ball.effect == Some(ProjectileEffect::CombineBall)
        ));
        assert!(rifle.sounds.contains_key("bounce") && rifle.sounds.contains_key("burst"));
    }
}
