//! The weapon list: scrolling the mouse wheel moves a cursor through the
//! carried weapons without changing weapon, and a click confirms the pick,
//! as in Half-Life 2.
//!
//! The list is open while the wheel keeps turning and for [`HOLD_SECS`]
//! after the last notch, then fades over [`FADE_SECS`]. Once the hold is
//! over the pick is forgotten and the fire button fires again. The click
//! that confirms a pick does not fire: [`WeaponSelector::swallows_attack`]
//! masks the fire button until it is released.

use bevy::prelude::*;
use rl_sim::weapons::{Arsenal, Inventory, WeaponId};

/// Seconds the list stays up after the last scroll.
pub(crate) const HOLD_SECS: f32 = 1.0;
/// Seconds the list takes to fade away once the hold is over.
pub(crate) const FADE_SECS: f32 = 0.2;

/// Something that happened to the list, for its sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cue {
    /// The cursor moved.
    Step,
    /// A weapon was picked.
    Pick,
    /// The hold ran out with no pick.
    Close,
    /// A slot key was pressed for a slot with nothing to select.
    Deny,
}

/// Where the player is in picking a weapon with the wheel.
#[derive(Resource, Debug, Clone, PartialEq)]
pub(crate) struct WeaponSelector {
    /// The weapon the cursor is on; kept after the hold for the fade.
    cursor: Option<WeaponId>,
    /// Seconds since the last scroll.
    idle: f32,
    /// A click just confirmed a pick and the button is still down.
    swallow_attack: bool,
    /// What happened since the sounds last looked.
    cues: Vec<Cue>,
}

impl Default for WeaponSelector {
    fn default() -> Self {
        Self {
            cursor: None,
            idle: f32::INFINITY,
            swallow_attack: false,
            cues: Vec::new(),
        }
    }
}

impl WeaponSelector {
    /// Lets `secs` pass.
    pub(crate) fn advance(&mut self, secs: f32) {
        let was_open = self.is_open();
        self.idle += secs;
        if was_open && !self.is_open() {
            self.cues.push(Cue::Close);
        }
    }

    /// Records a sound for something done outside the list (a slot key).
    pub(crate) fn cue(&mut self, cue: Cue) {
        self.cues.push(cue);
    }

    /// What happened to the list since this was last called.
    pub(crate) fn take_cues(&mut self) -> Vec<Cue> {
        std::mem::take(&mut self.cues)
    }

    /// Whether a pick is under way: the fire button confirms it instead of
    /// firing.
    pub(crate) fn is_open(&self) -> bool {
        self.cursor.is_some() && self.idle < HOLD_SECS
    }

    /// How opaque the list is drawn, from 0 (hidden) to 1.
    pub(crate) fn opacity(&self) -> f32 {
        if self.cursor.is_none() {
            return 0.0;
        }
        (1.0 - (self.idle - HOLD_SECS) / FADE_SECS).clamp(0.0, 1.0)
    }

    /// The weapon the cursor is on, while the list is showing.
    pub(crate) fn cursor(&self) -> Option<WeaponId> {
        self.cursor.filter(|_| self.opacity() > 0.0)
    }

    /// Moves the cursor one weapon on (`forward`) or back, opening the list
    /// on the weapon in hand if it was closed.
    pub(crate) fn scroll(&mut self, inventory: &Inventory, arsenal: &Arsenal, forward: bool) {
        let from = if self.is_open() {
            self.cursor
        } else {
            inventory.active()
        };
        self.cues.push(Cue::Step);
        self.cursor = inventory.cycle(arsenal, from, forward).or(from);
        self.idle = 0.0;
    }

    /// Takes the pick: closes the list and returns the weapon to switch to.
    /// The fire button stays masked until released.
    pub(crate) fn confirm(&mut self) -> Option<WeaponId> {
        if !self.is_open() {
            return None;
        }
        self.swallow_attack = true;
        self.cues.push(Cue::Pick);
        self.dismiss()
    }

    /// Closes the list without a pick, returning what the cursor was on.
    pub(crate) fn dismiss(&mut self) -> Option<WeaponId> {
        self.idle = f32::INFINITY;
        self.cursor.take()
    }

    /// Whether the fire button must be ignored this tick: it is the click
    /// that confirmed a pick, held on. `held` is whether the button is
    /// down and `pressed` whether it went down this frame. Ticks run before
    /// the frame's input is latched, so the click that confirms is seen
    /// here, with the list still open, before [`Self::confirm`] takes it.
    pub(crate) fn swallows_attack(&mut self, held: bool, pressed: bool) -> bool {
        self.swallow_attack |= pressed && self.is_open();
        self.swallow_attack &= held;
        self.swallow_attack
    }
}

#[cfg(test)]
mod tests {
    use rl_data::GameData;

    use super::*;

    fn arsenal() -> Arsenal {
        Arsenal::new(&GameData::builtin().expect("builtin"))
    }

    /// The loadout: Manipulator, bar, sidearm, SMG in hand, frag.
    fn loadout(arsenal: &Arsenal) -> Inventory {
        Inventory::loadout(
            arsenal,
            &GameData::builtin().expect("builtin").rules.loadout,
        )
    }

    #[test]
    fn it_starts_closed_and_hidden() {
        let selector = WeaponSelector::default();
        assert!(!selector.is_open());
        assert_eq!(selector.opacity(), 0.0);
        assert_eq!(selector.cursor(), None);
    }

    #[test]
    fn scrolling_opens_on_the_weapon_after_the_one_in_hand_and_keeps_moving() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        assert!(selector.is_open());
        assert_eq!(selector.opacity(), 1.0);
        let first = selector.cursor();
        assert_ne!(first, inventory.active(), "moved off the weapon in hand");
        selector.scroll(&inventory, &arsenal, true);
        assert_ne!(selector.cursor(), first);
        selector.scroll(&inventory, &arsenal, false);
        assert_eq!(selector.cursor(), first);
    }

    #[test]
    fn the_list_holds_then_fades_and_forgets_the_pick() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        selector.advance(HOLD_SECS - 0.01);
        assert!(selector.is_open());
        assert_eq!(selector.opacity(), 1.0);
        selector.advance(0.02 + FADE_SECS / 2.0);
        assert!(!selector.is_open(), "firing is back while it fades");
        assert!((selector.opacity() - 0.5).abs() < 0.1);
        assert_eq!(selector.confirm(), None, "a click now is not a pick");
        selector.advance(FADE_SECS);
        assert_eq!(selector.opacity(), 0.0);
        // The next scroll starts over from the weapon in hand.
        selector.scroll(&inventory, &arsenal, true);
        let mut again = WeaponSelector::default();
        again.scroll(&inventory, &arsenal, true);
        assert_eq!(selector.cursor(), again.cursor());
    }

    #[test]
    fn a_scroll_renews_the_hold() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        selector.advance(0.9);
        selector.scroll(&inventory, &arsenal, true);
        selector.advance(0.9);
        assert!(selector.is_open());
    }

    #[test]
    fn confirming_returns_the_pick_and_swallows_the_click_until_released() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        let picked = selector.cursor();
        assert_eq!(selector.confirm(), picked);
        assert!(!selector.is_open());
        assert!(
            selector.swallows_attack(true, false),
            "still the same click"
        );
        assert!(selector.swallows_attack(true, false));
        assert!(!selector.swallows_attack(false, false), "released");
        assert!(!selector.swallows_attack(true, true), "a new press fires");
    }

    #[test]
    fn the_list_cues_its_sounds() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        selector.scroll(&inventory, &arsenal, true);
        assert_eq!(selector.take_cues(), [Cue::Step, Cue::Step]);
        assert!(selector.take_cues().is_empty(), "taken");
        selector.confirm();
        selector.advance(HOLD_SECS + 1.0);
        assert_eq!(selector.take_cues(), [Cue::Pick], "no close after a pick");

        selector.scroll(&inventory, &arsenal, true);
        selector.take_cues();
        selector.advance(HOLD_SECS + 0.1);
        selector.advance(0.1);
        assert_eq!(
            selector.take_cues(),
            [Cue::Close],
            "once, when the hold ends"
        );
    }

    #[test]
    fn dismissing_closes_without_a_pick() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        assert!(selector.dismiss().is_some());
        assert!(!selector.is_open());
        assert_eq!(selector.opacity(), 0.0);
        assert!(!selector.swallows_attack(true, false));
    }

    #[test]
    fn a_tick_before_the_click_is_latched_already_swallows_it() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        assert!(selector.swallows_attack(true, true), "the confirming press");
        selector.confirm();
        assert!(selector.swallows_attack(true, false), "and its later ticks");
        assert!(!selector.swallows_attack(false, false));
    }

    #[test]
    fn a_press_without_a_list_fires() {
        let mut selector = WeaponSelector::default();
        assert!(!selector.swallows_attack(true, true));
    }
}
