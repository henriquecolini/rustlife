//! The room effect: which `dsp_presets.txt` preset the mixer runs the
//! room's share of every sound through.
//!
//! The soundscape playing decides, as its `dsp` says: 0 none (a preset
//! that passes sound through), a fixed room type (17 small concrete, 20
//! small outside...), or 1, automatic — which most of HL2DM's soundscapes
//! ask for. rustlife takes automatic where no soundscape says.
//!
//! The automatic effect listens to the shape of the place
//! (`rl_audio::dsp::room`): every quarter second, once the listener has
//! moved 48 units from where it last listened, it traces the twenty probes
//! from the eye through the map (and its props, to tell how cluttered the
//! place is), reads what each hit — the sky, or a surface whose
//! `audioreflectivity` it looks up — and, if the place came out different,
//! cross-fades to the preset for the new shape.

use bevy::prelude::*;
use rl_assets::coords;
use rl_audio::dsp::room::{PROBE_REACH, PROBES, Probe, RoomShape, automatic_preset};
use rl_sim::collision::contents::{MASK_PROP_SOLID, MASK_SHOT};
use rl_sim::collision::{CollisionQuery, Hull};
use rl_sim::scene::Scene;
use rl_sim::simulation::Simulation;
use source_formats::dsp_presets::{DspPresets, PresetDef};

use super::output::SetListener;
use super::props::Surfaces;
use crate::camera::FlyCam;
use crate::net::Game;
use crate::surfaces::MapSurfaces;
use crate::{AppState, GameVfs};

use super::MixerHandle;

/// Seconds between looks at the place (`DAS_CheckNewRoomDSP`'s rate).
const PROBE_INTERVAL: f32 = 0.25;
/// Units the listener moves before the place is measured again.
const MOVE_TO_REMEASURE: f32 = 48.0;
/// The `dsp` that asks for the automatic effect.
const AUTOMATIC: u32 = 1;

/// Chooses and sets the room effect.
pub(super) struct RoomPlugin;

impl Plugin for RoomPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RoomPresets>()
            .init_resource::<RoomChoice>()
            .init_resource::<Listening>()
            .add_systems(OnEnter(AppState::InGame), load_presets)
            .add_systems(OnExit(AppState::InGame), leave)
            .add_systems(
                Update,
                choose_room
                    .after(SetListener)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// The game's `dsp_presets.txt`.
#[derive(Resource, Default)]
pub struct RoomPresets(pub DspPresets);

/// The room effect the soundscape playing asks for (its `dsp`), if any.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RoomChoice {
    /// The playing soundscape's `dsp`; `None` before one plays or when it
    /// names none.
    pub soundscape: Option<u32>,
}

/// What the automatic effect last heard.
#[derive(Resource, Debug, Default)]
struct Listening {
    /// The preset number set, or `None` while automatic.
    fixed: Option<u32>,
    /// Where it last measured the place.
    measured_at: Option<Vec3>,
    /// When it next looks.
    next_look: f32,
}

fn load_presets(vfs: Res<GameVfs>, mut presets: ResMut<RoomPresets>) {
    let Some(bytes) = vfs.0.read("scripts/dsp_presets.txt").ok().flatten() else {
        warn!("no scripts/dsp_presets.txt: no room effects");
        presets.0 = DspPresets::default();
        return;
    };
    presets.0 = DspPresets::parse(&String::from_utf8_lossy(&bytes));
    info!("{} room effect presets", presets.0.len());
}

fn leave(
    mixer: Res<MixerHandle>,
    mut choice: ResMut<RoomChoice>,
    mut listening: ResMut<Listening>,
) {
    mixer.with(|mixer| mixer.set_room(None));
    *choice = RoomChoice::default();
    *listening = Listening::default();
}

/// Sets the preset the soundscape names, or the automatic one for the
/// place around the listener.
fn choose_room(
    time: Res<Time>,
    presets: Res<RoomPresets>,
    choice: Res<RoomChoice>,
    mut listening: ResMut<Listening>,
    mixer: Res<MixerHandle>,
    game: Game,
    map_surfaces: Res<MapSurfaces>,
    surfaces: Res<Surfaces>,
    cameras: Query<&GlobalTransform, With<FlyCam>>,
) {
    let wanted = choice.soundscape.unwrap_or(AUTOMATIC);
    if wanted != AUTOMATIC {
        if listening.fixed != Some(wanted) {
            listening.fixed = Some(wanted);
            listening.measured_at = None;
            let preset = presets.0.get(wanted as usize).cloned();
            mixer.with(|mixer| mixer.set_room(preset));
        }
        return;
    }
    let now = time.elapsed_secs();
    let automatic = listening.fixed.is_none();
    if automatic && now < listening.next_look {
        return;
    }
    listening.next_look = now + PROBE_INTERVAL;
    let (Some(sim), Some(camera)) = (game.sim(), cameras.iter().next()) else {
        return;
    };
    let eye = Vec3::from_array(coords::to_source_position(camera.translation()));
    let moved = listening
        .measured_at
        .is_none_or(|at| at.truncate().distance(eye.truncate()) > MOVE_TO_REMEASURE);
    if automatic && !moved {
        return;
    }
    let probes = probe(sim, eye, &map_surfaces, &surfaces.0);
    let Some(shape) = RoomShape::measure(&probes) else {
        return;
    };
    listening.fixed = None;
    listening.measured_at = Some(eye);
    let preset = automatic_preset(&presets.0, &shape);
    set_if_new(&mixer, preset, &shape);
}

/// Cross-fades to `preset` unless it is the one playing.
fn set_if_new(mixer: &MixerHandle, preset: Option<PresetDef>, shape: &RoomShape) {
    mixer.with(|mixer| {
        if mixer.room() != preset.as_ref() {
            debug!(?shape, "room effect changes");
            mixer.set_room(preset);
        }
    });
}

/// Traces [`PROBES`] from `eye` (Source space) through the map.
fn probe(
    sim: &Simulation,
    eye: Vec3,
    map_surfaces: &MapSurfaces,
    table: &source_formats::surfaceprops::SurfaceTable,
) -> [Probe; 20] {
    let scene = Scene {
        world: sim.world(),
        physics: sim.physics(),
        ignore: None,
        others: &[],
    };
    PROBES.map(|direction| {
        let end = eye + direction;
        let wall = sim.world().trace_ray(eye, end, MASK_SHOT);
        if !wall.hit() || wall.start_solid {
            return Probe::OPEN;
        }
        let surface = map_surfaces.get(wall.material);
        let sky = surface.is_some_and(|info| info.sky);
        let reflectivity = match surface {
            _ if sky => 0.0,
            Some(info) => table.sounds(&info.prop).reflectivity,
            None => source_formats::surfaceprops::DEFAULT_REFLECTIVITY,
        };
        let first = scene.trace_hull(eye, end, &Hull::POINT, MASK_PROP_SOLID);
        Probe {
            distance: if sky {
                PROBE_REACH
            } else {
                wall.fraction * direction.length()
            },
            reflectivity,
            sky,
            cluttered: first.body.is_some() && first.fraction < wall.fraction,
        }
    })
}
