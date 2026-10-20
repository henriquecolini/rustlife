//! The map's movers on screen: doors, buttons, switched brushes and
//! breakables, drawn where the server's logic has them.
//!
//! Each brush entity is spawned by `world` as a root placed by its origin
//! and angles ([`BrushEntity`]). For those the map's logic runs (movers),
//! this module moves the root each frame to the mover's pose — its position
//! interpolated between snapshots with everything else ([`Drawn`]),
//! carried by its parent mover — and shows or hides it.

use bevy::prelude::*;
use rl_assets::coords;

use crate::AppState;
use crate::net::{Drawn, Game};
use crate::world::BrushEntity;

/// Moves and shows the map's movers while in [`AppState::InGame`].
pub struct MoversPlugin;

impl Plugin for MoversPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, place_movers.run_if(in_state(AppState::InGame)));
    }
}

/// Places every mover's brush entity where it is drawn this frame.
fn place_movers(
    drawn: Res<Drawn>,
    game: Game,
    mut entities: Query<(&BrushEntity, &mut Transform, &mut Visibility)>,
) {
    let Some(sim) = game.sim() else {
        return;
    };
    let logic = sim.logic();
    let drawn_movers = drawn.0.as_ref().map(|world| world.movers.as_slice());
    let position = |index: usize| {
        drawn_movers
            .and_then(|movers| movers.get(index))
            .map(|mover| mover.position)
            .or_else(|| logic.movers().get(index).map(|mover| mover.position))
            .unwrap_or(0.0)
    };
    for (entity, mut transform, mut visibility) in &mut entities {
        let Some(index) = logic.mover_index(entity.model) else {
            continue;
        };
        let Some((origin, rotation)) = logic.mover_pose_with(index, &position) else {
            continue;
        };
        let visible = drawn_movers
            .and_then(|movers| movers.get(index))
            .map_or_else(|| logic.movers()[index].visible(), |mover| mover.visible);
        let translation = coords::position(origin.to_array());
        let rotation = coords::rotation_from_source(rotation);
        if transform.translation != translation || transform.rotation != rotation {
            transform.translation = translation;
            transform.rotation = rotation;
        }
        let wanted = if visible {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        visibility.set_if_neq(wanted);
    }
}
