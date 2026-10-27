//! The scope: the player's field of view narrows while the weapon in hand is
//! zoomed (the Crossbow's alternate button), as HL2 does it.
//!
//! The simulation only says whether the scope is on
//! ([`Inventory::zoomed`](rl_sim::weapons::Inventory::zoomed)); here the
//! field of view eases towards the weapon's [`ZoomConfig`]
//! over its zoom-in time and back over its zoom-out time, and the mouse
//! turns the view less in step, as Source scales the sensitivity by the
//! field of view.

use bevy::prelude::*;
use rl_data::ZoomConfig;

use crate::camera::FlyCam;
use crate::net::Game;
use crate::player::LocalPlayer;
use crate::settings::{Settings, apply_camera};
use crate::viewmodel::ViewModel;

/// How far the scope is in, and what that does to the view.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// `0` unzoomed to `1` fully zoomed.
    pub blend: f32,
    /// What the mouse's turning is scaled by: the zoomed field of view over
    /// the usual one.
    pub sensitivity: f32,
    /// The scope that was last on, kept for zooming out.
    config: Option<ZoomConfig>,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            blend: 0.0,
            sensitivity: 1.0,
            config: None,
        }
    }
}

/// Eases the field of view in and out with the scope.
pub struct ZoomPlugin;

impl Plugin for ZoomPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Zoom>()
            .add_systems(Startup, spawn_scope)
            .add_systems(
                Update,
                (update_zoom.after(apply_camera), show_scope).chain(),
            );
    }
}

/// The vertical field of view, radians, of a Source field of view given
/// across a 4:3 screen (which is what HL2's `fov` numbers are).
#[must_use]
pub fn vertical_from_four_by_three(horizontal_degrees: f32) -> f32 {
    2.0 * ((horizontal_degrees.to_radians() / 2.0).tan() * 0.75).atan()
}

/// The scope overlay's colour.
const SCOPE_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.85);
/// How far in the zoom is when the view model goes away and the scope's
/// reticle shows.
const SCOPED_AT: f32 = 0.5;

/// The reticle drawn over a zoomed view.
#[derive(Component)]
struct Scope;

/// Builds the reticle: a ring with a cross through it, hidden until the
/// scope is on.
fn spawn_scope(mut commands: Commands) {
    let line = |horizontal: bool| Node {
        position_type: PositionType::Absolute,
        left: Val::Percent(if horizontal { 30.0 } else { 50.0 }),
        top: Val::Percent(if horizontal { 50.0 } else { 20.0 }),
        width: if horizontal {
            Val::Percent(40.0)
        } else {
            Val::Px(1.0)
        },
        height: if horizontal {
            Val::Px(1.0)
        } else {
            Val::Percent(60.0)
        },
        ..default()
    };
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            Visibility::Hidden,
            GlobalZIndex(-1),
            Scope,
        ))
        .with_children(|parent| {
            for horizontal in [true, false] {
                parent.spawn((line(horizontal), BackgroundColor(SCOPE_COLOR)));
            }
            parent.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Percent(50.0),
                    top: Val::Percent(50.0),
                    width: Val::Vh(60.0),
                    height: Val::Vh(60.0),
                    margin: UiRect {
                        left: Val::Vh(-30.0),
                        top: Val::Vh(-30.0),
                        ..default()
                    },
                    border: UiRect::all(Val::Px(2.0)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BorderColor::all(SCOPE_COLOR),
            ));
        });
}

/// While scoped, the weapon's view model is put away and the reticle shows.
fn show_scope(
    zoom: Res<Zoom>,
    mut scope: Query<&mut Visibility, (With<Scope>, Without<ViewModel>)>,
    mut view_models: Query<&mut Visibility, (With<ViewModel>, Without<Scope>)>,
) {
    let scoped = zoom.blend >= SCOPED_AT;
    for mut visibility in &mut scope {
        *visibility = if scoped {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    for mut visibility in &mut view_models {
        *visibility = if scoped {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
    }
}

/// The scope of the local player's weapon in hand, if it is on.
fn scope_on(game: &Game, local: &LocalPlayer) -> Option<ZoomConfig> {
    let sim = game.sim()?;
    let player = sim.player(local.id).filter(|player| player.alive())?;
    let weapon = player.inventory.active()?;
    let config = sim.arsenal().weapon(weapon)?.zoom?;
    player.inventory.zoomed().then_some(config)
}

fn update_zoom(
    time: Res<Time>,
    game: Game,
    local: Option<Res<LocalPlayer>>,
    settings: Res<Settings>,
    mut zoom: ResMut<Zoom>,
    mut cameras: Query<&mut Projection, With<FlyCam>>,
) {
    let on = local.as_deref().and_then(|local| scope_on(&game, local));
    if let Some(config) = on {
        zoom.config = Some(config);
    }
    let Some(config) = zoom.config else {
        return;
    };
    let seconds = if on.is_some() {
        config.in_time
    } else {
        config.out_time
    };
    let step = if seconds > 0.0 {
        time.delta_secs() / seconds
    } else {
        1.0
    };
    let target = if on.is_some() { 1.0 } else { 0.0 };
    zoom.blend = if zoom.blend < target {
        (zoom.blend + step).min(target)
    } else {
        (zoom.blend - step).max(target)
    };
    if zoom.blend <= 0.0 {
        zoom.sensitivity = 1.0;
        zoom.config = None;
        return;
    }
    zoom.sensitivity = config.fov / settings.video.fov.max(1.0);
    let Ok(mut projection) = cameras.single_mut() else {
        return;
    };
    if let Projection::Perspective(perspective) = &mut *projection {
        let zoomed = vertical_from_four_by_three(config.fov);
        perspective.fov += (zoomed - perspective.fov) * zoom.blend;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_fields_of_view_are_horizontal_on_a_four_by_three_screen() {
        // 90 degrees across 4:3 is 73.7 degrees tall.
        let tall = vertical_from_four_by_three(90.0).to_degrees();
        assert!((tall - 73.74).abs() < 0.01, "{tall}");
        // The crossbow's 20 degrees is 15.1 tall.
        let scoped = vertical_from_four_by_three(20.0).to_degrees();
        assert!((scoped - 15.06).abs() < 0.01, "{scoped}");
    }
}
