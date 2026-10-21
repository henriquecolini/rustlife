//! Fog: the map's distance fog, the 3D skybox's own, and water's.
//!
//! A map sets its fog with an `env_fog_controller` (`fogenable`,
//! `fogcolor`, `fogstart`, `fogend`, `fogmaxdensity`), and the 3D skybox
//! with the same keys on its `sky_camera` (distances in the map's units,
//! scaled down to the skybox's). Under water, the view fogs to the
//! water's colour over the water material's distances instead. Each becomes
//! a linear [`DistanceFog`] on its camera; distances are in units, turned to
//! metres.

use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;
use rl_assets::{MapAsset, SourceMaterial, coords};
use rl_sim::collision::contents::MASK_WATER;
use source_formats::entities::MapEntity;

use crate::AppState;
use crate::camera::FlyCam;
use crate::net::Game;
use crate::world::MapHandle;

/// Fogs the views.
pub struct FogPlugin;

impl Plugin for FogPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, apply_fog.run_if(in_state(AppState::InGame)))
            .add_systems(OnExit(AppState::InGame), clear_fog);
    }
}

/// A linear fog, units.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fog {
    /// Linear RGB.
    color: [f32; 3],
    start: f32,
    end: f32,
    /// How thick it gets, `0..=1`.
    density: f32,
}

impl Fog {
    fn component(self) -> DistanceFog {
        let [red, green, blue] = self.color;
        DistanceFog {
            color: Color::linear_rgba(red, green, blue, self.density),
            falloff: FogFalloff::Linear {
                start: self.start * coords::METERS_PER_UNIT,
                end: self.end.max(self.start + 1.0) * coords::METERS_PER_UNIT,
            },
            ..default()
        }
    }
}

/// The fog an entity's keys describe (`fogenable` and the rest), if it
/// enables one.
fn fog_of(entity: &MapEntity) -> Option<Fog> {
    if entity.property("fogenable") != Some("1") {
        return None;
    }
    let color = entity
        .property("fogcolor")
        .and_then(|text| {
            let mut parts = text.split_whitespace().map(str::parse::<f32>);
            Some([
                parts.next()?.ok()?,
                parts.next()?.ok()?,
                parts.next()?.ok()?,
            ])
        })
        .map(|bytes| bytes.map(|byte| srgb_to_linear(byte / 255.0)))?;
    Some(Fog {
        color,
        start: entity.number("fogstart").unwrap_or(0.0).max(0.0),
        end: entity.number("fogend").unwrap_or(4096.0).max(1.0),
        density: entity
            .number("fogmaxdensity")
            .unwrap_or(1.0)
            .clamp(0.0, 1.0),
    })
}

fn srgb_to_linear(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// The sky camera (not the main one).
type SkyCameras = (With<Camera3d>, Without<FlyCam>, With<crate::sky::SkyView>);

/// The fog the camera should have now: water's when the eyes are in water,
/// else the map's.
fn apply_fog(
    mut commands: Commands,
    handle: Option<Res<MapHandle>>,
    maps: Res<Assets<MapAsset>>,
    materials: Res<Assets<SourceMaterial>>,
    game: Game,
    main: Query<(Entity, &GlobalTransform, Option<&DistanceFog>), With<FlyCam>>,
    sky: Query<(Entity, Option<&DistanceFog>), SkyCameras>,
) {
    let Some(map) = handle.and_then(|handle| maps.get(&handle.0)) else {
        return;
    };
    let Ok((camera, eye, current)) = main.single() else {
        return;
    };
    let entities = &map.entities;
    let map_fog = entities
        .iter()
        .find(|entity| entity.classname == "env_fog_controller")
        .and_then(fog_of);
    let eye = Vec3::from(coords::to_source_position(eye.translation()));
    let underwater = game
        .sim()
        .is_some_and(|sim| sim.world().point_contents(eye) & MASK_WATER != 0);
    let water_fog = underwater
        .then(|| {
            materials
                .iter()
                .find_map(|(_, material)| material.water_fog)
                .map(|fog| Fog {
                    color: fog.color,
                    start: fog.start,
                    end: fog.end,
                    density: 1.0,
                })
        })
        .flatten();
    set_fog(&mut commands, camera, current, water_fog.or(map_fog));
    // The skybox's fog is given in the map's units; the sky camera sees the
    // skybox's, so many times smaller.
    let scale = map.sky_camera.map_or(1.0, |camera| camera.scale);
    let sky_fog = entities
        .iter()
        .find(|entity| entity.classname == "sky_camera")
        .and_then(fog_of)
        .map(|fog| Fog {
            start: fog.start / scale,
            end: fog.end / scale,
            ..fog
        });
    for (camera, current) in &sky {
        set_fog(&mut commands, camera, current, sky_fog);
    }
}

/// Gives `camera` the fog `wanted` (or none), touching it only on change.
fn set_fog(
    commands: &mut Commands,
    camera: Entity,
    current: Option<&DistanceFog>,
    wanted: Option<Fog>,
) {
    match (current, wanted) {
        (None, None) => {}
        (Some(_), None) => {
            commands.entity(camera).remove::<DistanceFog>();
        }
        (current, Some(fog)) => {
            let component = fog.component();
            let same = current.is_some_and(|current| {
                current.color == component.color && current.falloff_eq(&component.falloff)
            });
            if !same {
                commands.entity(camera).insert(component);
            }
        }
    }
}

/// Comparing falloffs, which Bevy's type does not do itself.
trait FalloffEq {
    fn falloff_eq(&self, other: &FogFalloff) -> bool;
}

impl FalloffEq for DistanceFog {
    fn falloff_eq(&self, other: &FogFalloff) -> bool {
        match (&self.falloff, other) {
            (
                FogFalloff::Linear { start, end },
                FogFalloff::Linear {
                    start: other_start,
                    end: other_end,
                },
            ) => start == other_start && end == other_end,
            _ => false,
        }
    }
}

/// No fog on the main camera between maps.
fn clear_fog(mut commands: Commands, main: Query<Entity, (With<FlyCam>, With<DistanceFog>)>) {
    for camera in &main {
        commands.entity(camera).remove::<DistanceFog>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fog_controllers_say_their_fog() {
        let controller = MapEntity::new("env_fog_controller", [0.0; 3])
            .with("fogenable", "1")
            .with("fogcolor", "255 0 128")
            .with("fogstart", "512")
            .with("fogend", "2048")
            .with("fogmaxdensity", "0.5");
        let fog = fog_of(&controller).expect("fog");
        assert_eq!(fog.color[0], 1.0);
        assert_eq!(fog.color[1], 0.0);
        assert!((fog.color[2] - 0.2158).abs() < 1e-3);
        assert_eq!((fog.start, fog.end, fog.density), (512.0, 2048.0, 0.5));
        let off = MapEntity::new("env_fog_controller", [0.0; 3]).with("fogcolor", "1 2 3");
        assert_eq!(fog_of(&off), None);
    }
}
