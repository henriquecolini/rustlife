//! What the Gravity Gun does when it throws something: `PhyscannonImpact`.
//!
//! The game's client notices a launch (a held prop let fly, or a prop
//! punted) and dispatches the effect at the prop's centre
//! (`C_WeaponPhysCannon::DoEffectLaunch`). The effect, from `client.so`:
//!
//! - for the player who fired, a beam from the view model's muzzle to the
//!   prop: `sprites/orangelight1`, a tenth of a second, 12 units wide at the
//!   muzzle and 4 at the prop, straight (no noise), sixteen segments, at full
//!   brightness until it goes (`BeamInfo_t` of the callback);
//! - sparks (`FX_GaussExplosion`, see [`crate::trail_sparks`]): at the prop,
//!   thrown back along the beam towards the shooter; for everyone else's
//!   gun, at its muzzle, thrown forward;
//! - the muzzle's blast sprite, which [`crate::gravity_gun`] draws: it grows
//!   from 8 to 64 units in a tenth of a second while fading out over a fifth;
//! - for the shooter, the screen flashes white for a tenth of a second
//!   ([`crate::screen_fade`]) and the view is kicked up
//!   ([`crate::view_punch`]).

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rl_assets::{SOURCE, SpriteAsset, coords};
use rl_sim::simulation::PlayerId;
use rl_sim::tools::manipulator::ToolEvent;

use crate::AppState;
use crate::camera::FlyCam;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::effects::Scene;
use crate::lightning::{self, BeamShape};
use crate::net::Game;
use crate::player::{LocalPlayer, Ticked};
use crate::screen_fade::ScreenFade;
use crate::trail_sparks::{Burst, Sparks};
use crate::world::MapScoped;

/// How long the beam lasts, seconds (`m_flLife`).
const BEAM_LIFE: f32 = 0.1;

/// The beam's look: 12 units across at the muzzle, 4 at the prop, straight
/// (an amplitude of zero), sixteen points, its texture standing still.
const BEAM: BeamShape = BeamShape {
    widths: (12.0, 4.0),
    amplitude: 0.0,
    segments: 16,
    speed: 0.0,
};

/// The shooter's screen flashes white as the gun goes off: a fade of
/// (245, 245, 255) at alpha 32 out of 255 that runs out in a tenth of a
/// second (`PrimaryFireEffect`).
const FLASH: ScreenFade = ScreenFade {
    color: [245.0 / 255.0, 245.0 / 255.0, 1.0],
    alpha: 32.0 / 255.0,
    seconds: 0.1,
};

/// A prop was thrown: where it was (Bevy space, metres), who threw it and
/// whether that is the player this client plays.
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub(crate) struct PhyscannonImpact {
    pub player: PlayerId,
    pub at: Vec3,
    pub local: bool,
}

/// Which way the sparks fly, a Source-space unit vector: the shooter's own
/// back along the beam, from the prop towards the muzzle; anyone else's
/// forward from their muzzle towards where the prop was.
pub(crate) fn spark_direction(muzzle: Vec3, prop: Vec3, local: bool) -> Vec3 {
    let forward = (prop - muzzle).normalize_or_zero();
    if local { -forward } else { forward }
}

/// Where the sparks start (Bevy space): the prop for the shooter, the gun's
/// muzzle for everyone else.
pub(crate) fn spark_origin(muzzle: Vec3, prop: Vec3, local: bool) -> Vec3 {
    if local { prop } else { muzzle }
}

/// Draws the Gravity Gun's launch effect.
pub(crate) struct PhyscannonImpactPlugin;

impl Plugin for PhyscannonImpactPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<PhyscannonImpact>()
            .init_resource::<BeamSprite>()
            .add_systems(Startup, load_sprite)
            .add_systems(
                Update,
                (detect_launches, start_effects)
                    .chain()
                    .after(crate::camera::CameraPlaced)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                PostUpdate,
                draw_beams
                    .in_set(crate::camera::AttachedEffects)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The beam's texture.
#[derive(Resource, Default)]
struct BeamSprite(Handle<SpriteAsset>);

fn load_sprite(mut sprite: ResMut<BeamSprite>, assets: Res<AssetServer>) {
    sprite.0 = assets.load(format!("{SOURCE}://materials/sprites/orangelight1.vmt"));
}

/// Turns each launch the tick events report into a [`PhyscannonImpact`] at
/// the launched prop's centre.
fn detect_launches(
    mut ticks: MessageReader<Ticked>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    mut impacts: MessageWriter<PhyscannonImpact>,
) {
    let Some(sim) = game.sim() else {
        ticks.clear();
        return;
    };
    let local = local.map(|local| local.id);
    for Ticked(events) in ticks.read() {
        for &(player, event) in &events.tools {
            let (ToolEvent::Launched(body) | ToolEvent::Punted(body)) = event else {
                continue;
            };
            let physics = sim.physics();
            let Some(centre) = physics
                .center_of_mass(body)
                .or_else(|| physics.pose(body).map(|pose| pose.position))
            else {
                continue;
            };
            debug!(?player, ?centre, "gravity gun launch");
            impacts.write(PhyscannonImpact {
                player,
                at: coords::position(centre.to_array()),
                local: Some(player) == local,
            });
        }
    }
}

/// A beam alive.
#[derive(Component)]
struct Beam {
    player: PlayerId,
    /// Where it ends, Bevy space.
    to: Vec3,
    /// When it goes, seconds of app time.
    until: f32,
    mesh: Handle<Mesh>,
}

/// Starts the beam of the shooter's launch and throws the sparks of all.
fn start_effects(
    mut commands: Commands,
    mut impacts: MessageReader<PhyscannonImpact>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    scene: Scene,
    sprite: Res<BeamSprite>,
    sprites: Res<Assets<SpriteAsset>>,
    time: Res<Time>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<EffectMaterial>>,
    mut sparks: MessageWriter<Sparks>,
    mut fades: MessageWriter<ScreenFade>,
    mut material: Local<Option<Handle<EffectMaterial>>>,
) {
    let Some(sim) = game.sim() else {
        impacts.clear();
        return;
    };
    let local = local.map(|local| local.id);
    for impact in impacts.read() {
        let muzzle = scene.muzzle_position(sim, impact.player, local);
        let source = |at: Vec3| Vec3::from(coords::to_source_position(at));
        sparks.write(Sparks {
            at: spark_origin(muzzle, impact.at, impact.local),
            direction: spark_direction(source(muzzle), source(impact.at), impact.local),
            burst: Burst::Gauss,
        });
        if !impact.local {
            continue;
        }
        fades.write(FLASH);
        let Some(beam) = sprites.get(&sprite.0) else {
            debug!("the beam's sprite is not loaded");
            continue;
        };
        let material = material
            .get_or_insert_with(|| {
                // The beam's texture is tiled along it.
                if let Some(mut image) = images.get_mut(&beam.image) {
                    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                        address_mode_v: ImageAddressMode::Repeat,
                        ..ImageSamplerDescriptor::linear()
                    });
                }
                materials.add(effect_material(StandardMaterial {
                    base_color_texture: Some(beam.image.clone()),
                    unlit: true,
                    double_sided: true,
                    cull_mode: None,
                    alpha_mode: AlphaMode::Add,
                    ..default()
                }))
            })
            .clone();
        let mesh = meshes.add(Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        ));
        commands.spawn((
            Beam {
                player: impact.player,
                to: impact.at,
                until: time.elapsed_secs() + BEAM_LIFE,
                mesh: mesh.clone(),
            },
            Mesh3d(mesh),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            bevy::camera::visibility::NoFrustumCulling,
            MapScoped,
        ));
    }
}

/// Redraws each beam from the muzzle (which moves with the view) to where
/// the prop was, and removes the spent.
fn draw_beams(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    scene: Scene,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    beams: Query<(Entity, &Beam)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(sim), Ok(camera)) = (game.sim(), camera.single()) else {
        return;
    };
    let local = local.map(|local| local.id);
    let now = time.elapsed_secs();
    let eye = camera.translation();
    let unit = coords::METERS_PER_UNIT;
    for (entity, beam) in &beams {
        if now >= beam.until {
            commands.entity(entity).try_despawn();
            meshes.remove(&beam.mesh);
            continue;
        }
        let Some(mut mesh) = meshes.get_mut(&beam.mesh) else {
            continue;
        };
        let start = scene.muzzle_position(sim, beam.player, local) / unit;
        let end = beam.to / unit;
        let toward_camera = |at: Vec3| (eye - at * unit).normalize_or_zero();
        let points = lightning::points(
            start,
            end,
            toward_camera(start.midpoint(end)),
            &BEAM,
            &[],
            now,
        );
        let (positions, uvs, indices) = lightning::ribbon(&points, toward_camera);
        let positions: Vec<[f32; 3]> = positions
            .into_iter()
            .map(|p| (Vec3::from(p) * unit).to_array())
            .collect();
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![[0.0, 0.0, 1.0]; positions.len()],
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
        mesh.insert_indices(Indices::U32(indices));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shooters_sparks_come_back_along_the_beam() {
        let direction = spark_direction(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), true);
        assert!((direction - Vec3::NEG_X).length() < 1e-6, "{direction}");
        assert_eq!(
            spark_origin(Vec3::ZERO, Vec3::X, true),
            Vec3::X,
            "at the prop"
        );
    }

    #[test]
    fn another_players_sparks_leave_their_muzzle_forward() {
        let muzzle = Vec3::new(1.0, 2.0, 3.0);
        let direction = spark_direction(muzzle, muzzle + Vec3::new(0.0, 8.0, 0.0), false);
        assert!((direction - Vec3::Y).length() < 1e-6, "{direction}");
        assert_eq!(spark_origin(muzzle, Vec3::ZERO, false), muzzle);
    }

    #[test]
    fn a_prop_at_the_muzzle_gives_no_direction_rather_than_nan() {
        let direction = spark_direction(Vec3::ONE, Vec3::ONE, true);
        assert_eq!(direction, Vec3::ZERO);
    }

    #[test]
    fn the_beam_is_the_games_shape() {
        assert_eq!(BEAM.widths, (12.0, 4.0));
        assert_eq!((BEAM.amplitude, BEAM.segments, BEAM.speed), (0.0, 16, 0.0));
        assert!((BEAM_LIFE - 0.1).abs() < f32::EPSILON);
    }

    #[test]
    fn an_untouched_beam_runs_straight_and_tapers() {
        let points = lightning::points(
            Vec3::ZERO,
            Vec3::new(200.0, 0.0, 0.0),
            Vec3::Z,
            &BEAM,
            &[],
            0.0,
        );
        assert_eq!(points.len(), 16);
        assert!(
            points
                .iter()
                .all(|p| p.position.y == 0.0 && p.position.z == 0.0)
        );
        assert!((points[0].width - 12.0).abs() < 1e-5);
        assert!((points[15].width - 4.0).abs() < 1e-5);
    }
}
