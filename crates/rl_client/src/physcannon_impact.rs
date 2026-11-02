//! What the Gravity Gun does when it throws something: `PhyscannonImpact`.
//!
//! The game's client notices a launch (a held prop let fly, or a prop
//! punted) and dispatches the effect at the prop's centre
//! (`C_WeaponPhysCannon::DoEffectLaunch`). A punt that smashes something fixed
//! to the world, which the game has not got, looks the same, at the point it
//! struck. The effect, from `client.so`:
//!
//! - for the player who fired, a beam from the view model's muzzle to the
//!   prop: `sprites/orangelight1`, 12 units wide at the muzzle and 4 at the prop,
//!   straight (no noise), sixteen segments, at full brightness until it goes
//!   (`BeamInfo_t` of the callback). Unlike the game's, which is gone in a tenth
//!   of a second, it lasts [`BEAM_LIFE`], thinning away over the last third, and
//!   stays on both of them all the while: on the muzzle as it moves with the
//!   view, on the prop's centre as the prop is drawn in flight;
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
use rl_sim::physics::{BodyId, BodyPose};
use rl_sim::simulation::PlayerId;
use rl_sim::tools::manipulator::ToolEvent;

use crate::AppState;
use crate::camera::FlyCam;
use crate::effect_material::{EffectMaterial, effect_material};
use crate::effects::Scene;
use crate::lightning::{self, BeamShape};
use crate::net::{Drawn, Game};
use crate::player::{LocalPlayer, Ticked};
use crate::screen_fade::ScreenFade;
use crate::trail_sparks::{Burst, Sparks};
use crate::world::MapScoped;

/// How long the beam lasts, seconds. The game's `m_flLife` is 0.1, too short to
/// see where it went.
const BEAM_LIFE: f32 = 0.3;

/// How much of its life, seconds, the beam takes to thin away at the end.
const BEAM_FADE_SECS: f32 = BEAM_LIFE / 3.0;

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

/// A prop was thrown, or something fixed to the world was smashed: where it was
/// (Bevy space, metres), who did it, whether that is the player this client plays,
/// and the prop thrown with where its centre is in its own space (Source units).
#[derive(Message, Debug, Clone, Copy, PartialEq)]
pub(crate) struct PhyscannonImpact {
    pub player: PlayerId,
    pub at: Vec3,
    pub local: bool,
    pub body: Option<(BodyId, Vec3)>,
}

/// Where the centre of a prop that has its origin at `pose` is, given as `centre`
/// in the same (Source) space, in the prop's own space.
fn centre_in_body(pose: BodyPose, centre: Vec3) -> Vec3 {
    pose.rotation.inverse() * (centre - pose.position)
}

/// Where a prop's centre, `body_centre` in its own space, is with the prop at
/// `pose`, Source space.
fn centre_at(pose: BodyPose, body_centre: Vec3) -> Vec3 {
    pose.position + pose.rotation * body_centre
}

/// How wide the beam is, of its full width, with `left` seconds to go.
fn beam_width(left: f32) -> f32 {
    (left / BEAM_FADE_SECS).clamp(0.0, 1.0)
}

/// The beam's look with `left` seconds to go.
fn beam_shape(left: f32) -> BeamShape {
    let width = beam_width(left);
    BeamShape {
        widths: (BEAM.widths.0 * width, BEAM.widths.1 * width),
        ..BEAM
    }
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
/// the launched prop's centre, and each smash into one where the punt struck.
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
            let body = match event {
                ToolEvent::Launched(body) | ToolEvent::Punted(body) => body,
                ToolEvent::SmashedProp(_, point) | ToolEvent::SmashedBrush(_, point) => {
                    impacts.write(PhyscannonImpact {
                        player,
                        at: coords::position(point.to_array()),
                        local: Some(player) == local,
                        body: None,
                    });
                    continue;
                }
                _ => continue,
            };
            let physics = sim.physics();
            let Some(pose) = physics.pose(body) else {
                continue;
            };
            let centre = physics.center_of_mass(body).unwrap_or(pose.position);
            debug!(?player, ?centre, "gravity gun launch");
            impacts.write(PhyscannonImpact {
                player,
                at: coords::position(centre.to_array()),
                local: Some(player) == local,
                body: Some((body, centre_in_body(pose, centre))),
            });
        }
    }
}

/// A beam alive.
#[derive(Component)]
struct Beam {
    player: PlayerId,
    /// The prop it ends on, if any, and where on it, in the prop's own space.
    body: Option<(BodyId, Vec3)>,
    /// Where it ended last, Bevy space: where it stays if the prop breaks.
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
                body: impact.body,
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

/// Redraws each beam from the muzzle (which moves with the view) to the
/// prop's centre, and removes the spent. The centre goes by the pose the prop is
/// drawn at: the replica's own has it a tenth of a second further on, which at
/// the speed of a punt is hundreds of units past the prop on the screen.
fn draw_beams(
    mut commands: Commands,
    time: Res<Time>,
    game: Game,
    drawn: Res<Drawn>,
    local: Option<Res<LocalPlayer>>,
    scene: Scene,
    camera: Query<&GlobalTransform, With<FlyCam>>,
    mut beams: Query<(Entity, &mut Beam)>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(sim), Ok(camera)) = (game.sim(), camera.single()) else {
        return;
    };
    let local = local.map(|local| local.id);
    let now = time.elapsed_secs();
    let eye = camera.translation();
    let unit = coords::METERS_PER_UNIT;
    for (entity, mut beam) in &mut beams {
        if now >= beam.until {
            commands.entity(entity).try_despawn();
            meshes.remove(&beam.mesh);
            continue;
        }
        let Some(mut mesh) = meshes.get_mut(&beam.mesh) else {
            continue;
        };
        if let Some((body, centre)) = beam.body
            && !sim.physics().is_broken(body)
            && let Some(pose) = drawn.0.as_ref().and_then(|drawn| drawn.props.get(&body))
        {
            beam.to = coords::position(centre_at(*pose, centre).to_array());
        }
        let start = scene.muzzle_position(sim, beam.player, local) / unit;
        let end = beam.to / unit;
        let toward_camera = |at: Vec3| (eye - at * unit).normalize_or_zero();
        let points = lightning::points(
            start,
            end,
            toward_camera(start.midpoint(end)),
            &beam_shape(beam.until - now),
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
    fn the_beam_is_the_games_shape_for_three_times_as_long() {
        assert_eq!(BEAM.widths, (12.0, 4.0));
        assert_eq!((BEAM.amplitude, BEAM.segments, BEAM.speed), (0.0, 16, 0.0));
        assert!((BEAM_LIFE - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn the_beam_thins_away_over_the_last_third_of_its_life() {
        assert_eq!(beam_width(BEAM_LIFE), 1.0);
        assert_eq!(
            beam_width(BEAM_LIFE * 2.0 / 3.0),
            1.0,
            "full width till then"
        );
        assert!((beam_width(BEAM_LIFE / 6.0) - 0.5).abs() < 1e-5);
        assert_eq!(beam_width(0.0), 0.0);
        assert_eq!(beam_width(-1.0), 0.0, "and not past nothing");
        let half = beam_shape(BEAM_LIFE / 6.0);
        assert!((half.widths.0 - 6.0).abs() < 1e-4 && (half.widths.1 - 2.0).abs() < 1e-4);
        assert_eq!(beam_shape(BEAM_LIFE).widths, BEAM.widths);
    }

    #[test]
    fn the_beam_ends_on_the_prop_however_it_tumbles() {
        let launch = BodyPose {
            position: Vec3::new(100.0, 0.0, 0.0),
            rotation: Quat::from_rotation_z(0.7),
        };
        // A prop with its origin off its centre, as a model's often is.
        let centre = Vec3::new(100.0, 10.0, 20.0);
        let body_centre = centre_in_body(launch, centre);
        assert!(centre_at(launch, body_centre).distance(centre) < 1e-4);

        // Half a turn about Z later and 500 units on: the part of the offset in
        // the plane of the turn points the other way.
        let later = BodyPose {
            position: launch.position + Vec3::new(500.0, 0.0, 0.0),
            rotation: Quat::from_rotation_z(std::f32::consts::PI) * launch.rotation,
        };
        let end = centre_at(later, body_centre);
        assert!(end.distance(Vec3::new(600.0, -10.0, 20.0)) < 1e-4, "{end}");
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
