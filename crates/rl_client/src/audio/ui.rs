//! The menus' sounds: Half-Life 2's own button sounds (`sound/ui/`), played
//! for the widgets' [`UiSound`]s in the main menu and in game alike.
//!
//! They are files, not soundscript entries, since the soundscripts load
//! with the first map; they are loaded at startup so the first rollover is
//! not late. They follow the master volume only, and a silent run mutes
//! them with everything else.

use bevy::prelude::*;
use rl_data::UiSounds;

use super::SoundPlayer;
use crate::player::Gameplay;
use crate::ui::UiSound;

pub(super) struct UiSoundsPlugin;

impl Plugin for UiSoundsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<UiSound>()
            .add_systems(Startup, preload)
            .add_systems(Update, play);
    }
}

/// The file `sounds` names for `sound`.
fn file(sounds: &UiSounds, sound: UiSound) -> &str {
    match sound {
        UiSound::Rollover => &sounds.rollover,
        UiSound::Click => &sounds.click,
        UiSound::Release => &sounds.release,
    }
}

fn preload(gameplay: Option<Res<Gameplay>>, mut player: SoundPlayer) {
    let Some(gameplay) = gameplay else {
        return;
    };
    for sound in [UiSound::Rollover, UiSound::Click, UiSound::Release] {
        player.preload(file(&gameplay.0.sounds.ui, sound));
    }
}

fn play(
    mut heard: MessageReader<UiSound>,
    gameplay: Option<Res<Gameplay>>,
    mut player: SoundPlayer,
) {
    let Some(gameplay) = gameplay else {
        heard.clear();
        return;
    };
    for &sound in heard.read() {
        player.play_ui(file(&gameplay.0.sounds.ui, sound), 1.0);
    }
}
