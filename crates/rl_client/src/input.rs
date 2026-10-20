//! Turning keys and mouse buttons into the player's commands.
//!
//! Input is sampled every frame but the simulation ticks at a fixed rate,
//! so a press shorter than a tick could fall between two ticks. Presses are
//! therefore latched until the next tick takes them: buttons
//! count as held for that tick, and a weapon choice (number keys, the mouse
//! wheel, `Q` for the last weapon) waits to be resolved against the
//! inventory.
//!
//! | Input | Does |
//! |-------|------|
//! | `W` `A` `S` `D` | Move |
//! | Space | Jump |
//! | Left Ctrl, `C` | Crouch |
//! | Left Shift | Walk |
//! | `E` | Use (ladders) |
//! | Left / right mouse | Primary / secondary fire |
//! | `R` | Reload |
//! | `1`–`9` | Weapons of that slot |
//! | Mouse wheel | Next / previous weapon |
//! | `Q` | Last weapon |

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::weapons::Choice;

use crate::player::ViewAngles;

/// Keys and the buttons they press.
const BUTTON_KEYS: [(KeyCode, Buttons); 6] = [
    (KeyCode::Space, Buttons::JUMP),
    (KeyCode::ControlLeft, Buttons::DUCK),
    (KeyCode::KeyC, Buttons::DUCK),
    (KeyCode::ShiftLeft, Buttons::WALK),
    (KeyCode::KeyE, Buttons::USE),
    (KeyCode::KeyR, Buttons::RELOAD),
];

/// Mouse buttons and the buttons they press.
const BUTTON_MOUSE: [(MouseButton, Buttons); 2] = [
    (MouseButton::Left, Buttons::ATTACK),
    (MouseButton::Right, Buttons::ATTACK2),
];

/// Number keys and the weapon slot each selects.
const SLOT_KEYS: [(KeyCode, u8); 9] = [
    (KeyCode::Digit1, 1),
    (KeyCode::Digit2, 2),
    (KeyCode::Digit3, 3),
    (KeyCode::Digit4, 4),
    (KeyCode::Digit5, 5),
    (KeyCode::Digit6, 6),
    (KeyCode::Digit7, 7),
    (KeyCode::Digit8, 8),
    (KeyCode::Digit9, 9),
];

/// Input pressed since the last tick, held for it even if already
/// released.
#[derive(Resource, Debug, Default)]
pub(crate) struct Latched {
    /// Buttons pressed.
    pub(crate) buttons: Buttons,
    /// The latest weapon choice.
    pub(crate) choice: Option<Choice>,
}

/// Latches button presses and weapon choices until the next tick takes
/// them.
pub(crate) fn latch_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    mut latched: ResMut<Latched>,
) {
    for (key, button) in BUTTON_KEYS {
        if keys.just_pressed(key) {
            latched.buttons = latched.buttons | button;
        }
    }
    for (key, button) in BUTTON_MOUSE {
        if mouse.as_ref().is_some_and(|mouse| mouse.just_pressed(key)) {
            latched.buttons = latched.buttons | button;
        }
    }
    for (key, slot) in SLOT_KEYS {
        if keys.just_pressed(key) {
            latched.choice = Some(Choice::Slot(slot));
        }
    }
    if keys.just_pressed(KeyCode::KeyQ) {
        latched.choice = Some(Choice::Last);
    }
    // Wheel up goes back through the list, as in most shooters.
    match scroll.map_or(0.0, |scroll| scroll.delta.y) {
        delta if delta > 0.0 => latched.choice = Some(Choice::Previous),
        delta if delta < 0.0 => latched.choice = Some(Choice::Next),
        _ => {}
    }
}

/// The command for a tick from the keys and mouse buttons held now plus
/// latched presses. The weapon choice and the command's number are the
/// caller's to fill in.
pub(crate) fn user_cmd(
    keys: &ButtonInput<KeyCode>,
    mouse: Option<&ButtonInput<MouseButton>>,
    latched: Buttons,
    angles: ViewAngles,
) -> UserCmd {
    let axis = |positive: KeyCode, negative: KeyCode| {
        f32::from(u8::from(keys.pressed(positive))) - f32::from(u8::from(keys.pressed(negative)))
    };
    let mut buttons = latched;
    for (key, button) in BUTTON_KEYS {
        if keys.pressed(key) {
            buttons = buttons | button;
        }
    }
    for (key, button) in BUTTON_MOUSE {
        if mouse.is_some_and(|mouse| mouse.pressed(key)) {
            buttons = buttons | button;
        }
    }
    UserCmd {
        forward: axis(KeyCode::KeyW, KeyCode::KeyS),
        side: axis(KeyCode::KeyD, KeyCode::KeyA),
        pitch: angles.pitch,
        yaw: angles.yaw,
        buttons,
        ..UserCmd::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_combine_held_keys_mouse_and_latched_presses() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyR);
        let mut mouse = ButtonInput::<MouseButton>::default();
        mouse.press(MouseButton::Right);
        let cmd = user_cmd(&keys, Some(&mouse), Buttons::JUMP, ViewAngles::default());
        assert_eq!((cmd.forward, cmd.side), (1.0, -1.0));
        assert!(
            cmd.buttons
                .contains(Buttons::JUMP | Buttons::WALK | Buttons::ATTACK2 | Buttons::RELOAD)
        );
        assert!(!cmd.buttons.contains(Buttons::DUCK));
        assert!(!cmd.buttons.contains(Buttons::ATTACK));
    }
}
