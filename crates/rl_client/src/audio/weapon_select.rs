//! The weapon list's sounds: moving the cursor, picking, closing, denying.
//! They play at the listener, as the local player's own actions do.
//!
//! HL2DM plays them all on the local player's `CHAN_ITEM` channel: a new
//! one cuts off the one before. Their volume (0.32) and pitch are the
//! soundscript's.

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

/// The emitter of the list's sound last started: its channel.
#[derive(Default)]
struct Channel(Option<Entity>);

fn play(
    mut selector: ResMut<WeaponSelector>,
    gameplay: Res<Gameplay>,
    mut player: SoundPlayer,
    mut channel: Local<Channel>,
) {
    let sounds = &gameplay.0.sounds.weapon_select;
    for cue in selector.take_cues() {
        let name = match cue {
            Cue::Step => &sounds.step,
            Cue::Pick => &sounds.pick,
            Cue::Close => &sounds.close,
            Cue::Deny => &sounds.deny,
        };
        if let Some(previous) = channel.0.take() {
            player.stop(previous);
        }
        channel.0 = player.play(name, None, 1.0);
    }
}
