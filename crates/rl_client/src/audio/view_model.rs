//! The sounds the view model's sequences call for themselves: the events
//! along a sequence that play a sound (`AE_CL_PLAYSOUND`), such as the Pulse
//! Rifle's `Reload_Rotate` and `Reload_Push` as its reload turns and pushes
//! the cell in, or the magnum's loader and cylinder. HL2DM plays them on the
//! client as the view model's sequence passes them; they are the local
//! player's own, so they play at the listener.

use bevy::prelude::*;
use source_formats::anim::SequenceEvent;

use super::SoundPlayer;
use crate::AppState;
use crate::animation::{Animated, SequencePlayer};
use crate::viewmodel::ViewModel;

pub(super) struct ViewModelSoundsPlugin;

impl Plugin for ViewModelSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, play.run_if(in_state(AppState::InGame)));
    }
}

/// Plays the sounds of the events the view model's sequence passed (the
/// sequence player notes them once listened to).
fn play(
    mut views: Query<(&Animated, &mut SequencePlayer), With<ViewModel>>,
    mut player: SoundPlayer,
) {
    for (animated, mut sequences) in &mut views {
        sequences.listen();
        for (sequence, event) in sequences.take_events() {
            let sound = animated
                .animation
                .sequences
                .get(sequence)
                .and_then(|sequence| sequence.events.get(event))
                .and_then(SequenceEvent::sound);
            if let Some(name) = sound {
                player.play(name, None, 1.0);
            }
        }
    }
}
