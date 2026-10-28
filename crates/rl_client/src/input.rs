//! Turning keys and mouse buttons into the player's commands.
//!
//! Input is sampled every frame but the simulation ticks at a fixed rate,
//! so a press shorter than a tick could fall between two ticks. Presses are
//! therefore latched until the next tick takes them: buttons count as held
//! for that tick, and a weapon choice (the last-weapon and Gravity Gun keys,
//! a pick from the weapon list) waits to be resolved against the inventory.
//!
//! Which key does what is the player's choice (the [`Bindings`], by
//! default:)
//!
//! | Input | Does |
//! |-------|------|
//! | `W` `A` `S` `D` | Move |
//! | Space | Jump |
//! | Left Ctrl | Crouch |
//! | Left Shift | Walk |
//! | `E` | Use (ladders, chargers) |
//! | Left / right mouse | Primary / secondary fire |
//! | `R` | Reload |
//! | `Q` | Last weapon |
//! | `G` | Gravity Gun, or back to the weapon held before it |
//! | `1`–`9` | Weapons of that slot (fixed) |
//! | Mouse wheel | Moves through the weapon list (fixed) |
//! | Either fire button, with the list up | Switches to the weapon picked in it |
//! | `Esc`, with the list up | Closes it (else the pause menu) |

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use rl_sim::movement::{Buttons, UserCmd};
use rl_sim::weapons::Choice;

use crate::net::Game;
use crate::player::ViewAngles;
use crate::selector::WeaponSelector;
use crate::settings::Bindings;

/// Actions that hold a command button down.
const ACTION_BUTTONS: [(&str, Buttons); 8] = [
    ("jump", Buttons::JUMP),
    ("duck", Buttons::DUCK),
    ("sprint", Buttons::SPRINT),
    ("walk", Buttons::WALK),
    ("use", Buttons::USE),
    ("reload", Buttons::RELOAD),
    ("attack", Buttons::ATTACK),
    ("attack2", Buttons::ATTACK2),
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
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    bindings: Res<Bindings>,
    game: Game,
    mut selector: ResMut<WeaponSelector>,
    mut latched: ResMut<Latched>,
) {
    selector.advance(time.delta_secs());
    let carried = game
        .sim()
        .zip(game.local())
        .and_then(|(sim, local)| Some((&sim.player(local)?.inventory, sim.arsenal())));
    // Either fire button picks from the list instead of firing, as HL2's
    // `CBaseHudWeaponSelection::ProcessInput` has it.
    let picking = selector.is_open()
        && ["attack", "attack2"]
            .iter()
            .any(|action| bindings.just_pressed(action, &keys, mouse.as_deref()));
    if picking {
        let chosen = match carried {
            Some((inventory, arsenal)) => selector.confirm(inventory, arsenal),
            None => selector.dismiss().and(None),
        };
        if let Some(weapon) = chosen {
            latched.choice = Some(Choice::Weapon(weapon));
        }
    }
    for (action, button) in ACTION_BUTTONS {
        let fire = button == Buttons::ATTACK || button == Buttons::ATTACK2;
        if !(picking && fire) && bindings.just_pressed(action, &keys, mouse.as_deref()) {
            latched.buttons = latched.buttons | button;
        }
    }
    // A slot key opens the list on that slot (a click picks), as HL2DM's
    // `hud_fastswitch 0` does.
    for (key, slot) in SLOT_KEYS {
        if keys.just_pressed(key)
            && let Some((inventory, arsenal)) = carried
        {
            selector.select_slot(inventory, arsenal, slot);
        }
    }
    // These switch at once and silently, leaving the list (HL2's `lastinv`).
    if bindings.just_pressed("last_weapon", &keys, mouse.as_deref()) {
        selector.dismiss();
        latched.choice = Some(Choice::Last);
    }
    if bindings.just_pressed("gravity_gun", &keys, mouse.as_deref()) {
        selector.dismiss();
        latched.choice = Some(Choice::ToggleTool);
    }
    // Wheel up goes back through the list, as in most shooters. It only
    // moves the list's cursor; a click picks.
    let forward = match scroll.map_or(0.0, |scroll| scroll.delta.y) {
        delta if delta > 0.0 => false,
        delta if delta < 0.0 => true,
        _ => return,
    };
    if let Some((inventory, arsenal)) = carried {
        selector.scroll(inventory, arsenal, forward);
    }
}

/// The command for a tick from the keys and mouse buttons held now plus
/// latched presses. The weapon choice and the command's number are the
/// caller's to fill in.
pub(crate) fn user_cmd(
    keys: &ButtonInput<KeyCode>,
    mouse: Option<&ButtonInput<MouseButton>>,
    bindings: &Bindings,
    latched: Buttons,
    angles: ViewAngles,
) -> UserCmd {
    let held = |action: &str| bindings.pressed(action, keys, mouse);
    let axis = |positive: &str, negative: &str| {
        f32::from(u8::from(held(positive))) - f32::from(u8::from(held(negative)))
    };
    let mut buttons = latched;
    for (action, button) in ACTION_BUTTONS {
        if held(action) {
            buttons = buttons | button;
        }
    }
    UserCmd {
        forward: axis("forward", "back"),
        side: axis("right", "left"),
        pitch: angles.pitch,
        yaw: angles.yaw,
        buttons,
        ..UserCmd::default()
    }
}

/// A command that does nothing but keep the view where it is: what a
/// player sends while a menu is open.
pub(crate) fn idle_cmd(angles: ViewAngles) -> UserCmd {
    UserCmd {
        pitch: angles.pitch,
        yaw: angles.yaw,
        ..UserCmd::default()
    }
}

#[cfg(test)]
mod tests {
    use rl_data::GameData;
    use rl_sim::weapons::{Arsenal, Inventory};

    use super::*;

    /// An app that runs only [`latch_input`], with no game joined.
    fn latching_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time<Real>>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Bindings>()
            .init_resource::<WeaponSelector>()
            .init_resource::<Latched>()
            .add_systems(Update, latch_input);
        app
    }

    /// One frame in which `key` goes down.
    fn tap(app: &mut App, key: KeyCode) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release_all();
        keys.clear();
        keys.press(key);
        app.update();
    }

    #[test]
    fn the_gravity_gun_key_latches_the_toggle_and_closes_the_list() {
        let mut app = latching_app();
        // The weapon list is up when G goes down.
        let arsenal = Arsenal::new(&GameData::builtin().expect("builtin"));
        let inventory = Inventory::loadout(
            &arsenal,
            &GameData::builtin().expect("builtin").rules.loadout,
        );
        app.world_mut()
            .resource_mut::<WeaponSelector>()
            .scroll(&inventory, &arsenal, true);
        tap(&mut app, KeyCode::KeyG);
        assert_eq!(
            app.world().resource::<Latched>().choice,
            Some(Choice::ToggleTool)
        );
        assert!(!app.world().resource::<WeaponSelector>().is_open());

        app.world_mut().resource_mut::<Latched>().choice = None;
        tap(&mut app, KeyCode::KeyQ);
        assert_eq!(app.world().resource::<Latched>().choice, Some(Choice::Last));
    }

    #[test]
    fn alternate_fire_with_the_list_up_picks_instead_of_firing() {
        let mut app = latching_app();
        app.init_resource::<ButtonInput<MouseButton>>();
        let arsenal = Arsenal::new(&GameData::builtin().expect("builtin"));
        let inventory = Inventory::loadout(
            &arsenal,
            &GameData::builtin().expect("builtin").rules.loadout,
        );
        app.world_mut()
            .resource_mut::<WeaponSelector>()
            .scroll(&inventory, &arsenal, true);
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);
        app.update();
        assert!(!app.world().resource::<WeaponSelector>().is_open());
        assert_eq!(app.world().resource::<Latched>().buttons, Buttons::NONE);

        // With the list down, the same press fires.
        let mut mouse = app.world_mut().resource_mut::<ButtonInput<MouseButton>>();
        mouse.release_all();
        mouse.clear();
        mouse.press(MouseButton::Right);
        app.update();
        assert_eq!(app.world().resource::<Latched>().buttons, Buttons::ATTACK2);
    }

    #[test]
    fn commands_combine_held_keys_mouse_and_latched_presses() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyR);
        let mut mouse = ButtonInput::<MouseButton>::default();
        mouse.press(MouseButton::Right);
        let bindings = Bindings::default();
        let cmd = user_cmd(
            &keys,
            Some(&mouse),
            &bindings,
            Buttons::JUMP,
            ViewAngles::default(),
        );
        assert_eq!((cmd.forward, cmd.side), (1.0, -1.0));
        assert!(
            cmd.buttons
                .contains(Buttons::JUMP | Buttons::SPRINT | Buttons::ATTACK2 | Buttons::RELOAD)
        );
        assert!(!cmd.buttons.contains(Buttons::DUCK));
        assert!(!cmd.buttons.contains(Buttons::ATTACK));
    }

    #[test]
    fn rebound_keys_drive_the_commands_and_idle_commands_do_nothing() {
        let mut controls = rl_config::ControlSettings::default();
        controls.rebind("forward", "ArrowUp");
        let bindings = Bindings::from_controls(&controls);
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyW);
        let cmd = user_cmd(&keys, None, &bindings, Buttons::NONE, ViewAngles::default());
        assert_eq!(cmd.forward, 0.0, "W no longer moves forward");
        keys.press(KeyCode::ArrowUp);
        let cmd = user_cmd(&keys, None, &bindings, Buttons::NONE, ViewAngles::default());
        assert_eq!(cmd.forward, 1.0);

        let angles = ViewAngles {
            pitch: 12.0,
            yaw: 34.0,
        };
        let idle = idle_cmd(angles);
        assert_eq!((idle.pitch, idle.yaw), (12.0, 34.0));
        assert_eq!(idle.forward, 0.0);
        assert_eq!(idle.buttons, Buttons::NONE);
    }
}
