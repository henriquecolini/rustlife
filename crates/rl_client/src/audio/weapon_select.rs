//! The weapon list's sounds: moving the cursor, picking, closing.
//! They play at the listener, as the local player's own actions do.

use bevy::prelude::*;

use super::SoundPlayer;
use crate::AppState;
use crate::player::Gameplay;
use crate::selector::{Cue, WeaponSelector};

pub(super) struct WeaponSelectSoundsPlugin;

impl Plugin for WeaponSelectSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, play.run_if(in_state(AppState::InGame)));
    }
}

fn play(mut selector: ResMut<WeaponSelector>, gameplay: Res<Gameplay>, mut player: SoundPlayer) {
    let sounds = &gameplay.0.sounds.weapon_select;
    for cue in selector.take_cues() {
        let name = match cue {
            Cue::Step => &sounds.step,
            Cue::Pick => &sounds.pick,
            Cue::Close => &sounds.close,
        };
        player.play(name, None, 1.0);
    }
}
