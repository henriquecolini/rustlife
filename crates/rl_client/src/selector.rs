//! The weapon list: scrolling the mouse wheel or pressing a slot key moves a
//! cursor through the carried weapons without changing weapon, and a click
//! (either fire button) confirms the pick, as in Half-Life 2
//! (`hud_fastswitch 0`, its default).
//!
//! The list stays up for [`HOLD_SECS`] after the last scroll or slot key,
//! then fades over [`FADE_SECS`]; it can still be picked from while it
//! fades, and a scroll brings it back. The click that confirms a pick does
//! not fire: [`WeaponSelector::swallows_fire`] masks both fire buttons
//! until they are released.
//!
//! Its sounds are HL2DM's `CHudWeaponSelection`'s:
//!
//! | Action | Sound |
//! |--------|-------|
//! | A scroll that finds a weapon (opening the list or within it) | [`Cue::Step`] |
//! | A scroll that finds none | none |
//! | A slot key, even for an empty slot | [`Cue::Step`] |
//! | A click on a weapon that cannot be used | [`Cue::Deny`]; the list stays up |
//! | A click that picks | [`Cue::Pick`], then [`Cue::Close`] a frame later |
//! | `Esc` with the list up | [`Cue::Close`] |
//! | The list timing out, the last-weapon and Gravity Gun keys | none |
//!
//! HL2DM never plays its "selection open" sound. The pick's close is the
//! `cancelselect` command the pick queues, which the console runs on the
//! next frame.

use bevy::prelude::*;
use rl_sim::weapons::{Arsenal, Inventory, WeaponId};

/// Seconds the list stays up after the last scroll or slot key
/// (`SELECTION_TIMEOUT_THRESHOLD`).
pub(crate) const HOLD_SECS: f32 = 0.5;
/// Seconds the list takes to fade away once the hold is over
/// (`SELECTION_FADEOUT_TIME`).
pub(crate) const FADE_SECS: f32 = 0.75;

/// Something that happened to the list, for its sounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cue {
    /// The cursor moved, or a slot key was pressed.
    Step,
    /// A weapon was picked.
    Pick,
    /// The list was closed: after a pick, or cancelled.
    Close,
    /// A click on a weapon that cannot be selected.
    Deny,
}

/// Where the player is in picking a weapon with the wheel.
#[derive(Resource, Debug, Clone, PartialEq)]
pub(crate) struct WeaponSelector {
    /// The weapon the cursor is on, while the list is up.
    cursor: Option<WeaponId>,
    /// Seconds since the last scroll or slot key.
    idle: f32,
    /// A click just confirmed a pick and a fire button is still down.
    swallow_fire: bool,
    /// What happened since the sounds last looked, each with whether it
    /// waits a frame (a queued console command).
    cues: Vec<(Cue, bool)>,
}

impl Default for WeaponSelector {
    fn default() -> Self {
        Self {
            cursor: None,
            idle: f32::INFINITY,
            swallow_fire: false,
            cues: Vec::new(),
        }
    }
}

impl WeaponSelector {
    /// Lets `secs` pass; once the list has faded it closes, silently.
    pub(crate) fn advance(&mut self, secs: f32) {
        self.idle += secs;
        if self.idle >= HOLD_SECS + FADE_SECS {
            self.cursor = None;
        }
    }

    /// The sounds due this frame: what happened since this was last
    /// called, but for what waits a frame, which is due next time.
    pub(crate) fn take_cues(&mut self) -> Vec<Cue> {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.cues)
            .into_iter()
            .partition(|&(_, waits)| !waits);
        self.cues = later.into_iter().map(|(cue, _)| (cue, false)).collect();
        due.into_iter().map(|(cue, _)| cue).collect()
    }

    /// Whether a pick is under way (the list is up, fading or not): a fire
    /// button confirms it instead of firing.
    pub(crate) fn is_open(&self) -> bool {
        self.cursor.is_some()
    }

    /// How opaque the list is drawn, from 0 (hidden) to 1.
    pub(crate) fn opacity(&self) -> f32 {
        if self.cursor.is_none() {
            return 0.0;
        }
        (1.0 - (self.idle - HOLD_SECS) / FADE_SECS).clamp(0.0, 1.0)
    }

    /// The weapon the cursor is on, while the list is up.
    pub(crate) fn cursor(&self) -> Option<WeaponId> {
        self.cursor
    }

    /// Moves the cursor one usable weapon on (`forward`) or back, opening
    /// the list from the weapon in hand if it was closed. Finding none
    /// changes nothing and makes no sound.
    pub(crate) fn scroll(&mut self, inventory: &Inventory, arsenal: &Arsenal, forward: bool) {
        let from = self.cursor.or(inventory.active());
        let Some(next) = inventory
            .cycle(arsenal, from, forward)
            .filter(|&weapon| inventory.usable(arsenal, weapon))
        else {
            return;
        };
        self.cursor = Some(next);
        self.idle = 0.0;
        self.cues.push((Cue::Step, false));
    }

    /// Puts the cursor on the first usable weapon of `slot`, or the next
    /// one there if the cursor is already in it, wrapping round, and opens
    /// the list. A slot with nothing usable in it changes nothing, but
    /// sounds the same.
    pub(crate) fn select_slot(&mut self, inventory: &Inventory, arsenal: &Arsenal, slot: u8) {
        let order = |weapon: &WeaponId| {
            arsenal
                .weapon(*weapon)
                .map_or((u8::MAX, u8::MAX), |config| (config.slot, config.position))
        };
        let mut in_slot: Vec<WeaponId> = inventory
            .weapons()
            .filter(|weapon| order(weapon).0 == slot)
            .filter(|&weapon| inventory.usable(arsenal, weapon))
            .collect();
        in_slot.sort_by_key(order);
        let at = self
            .cursor
            .and_then(|cursor| in_slot.iter().position(|&weapon| weapon == cursor));
        let pick = at.and_then(|at| in_slot.get(at + 1)).or(in_slot.first());
        if let Some(&pick) = pick {
            self.cursor = Some(pick);
        }
        self.idle = 0.0;
        self.cues.push((Cue::Step, false));
    }

    /// A fire button pressed with the list up: returns the weapon to switch
    /// to and closes the list, or denies a weapon that cannot be used and
    /// leaves the list up. Either way the fire buttons stay masked until
    /// released. `None`, and nothing done, with the list down.
    pub(crate) fn confirm(&mut self, inventory: &Inventory, arsenal: &Arsenal) -> Option<WeaponId> {
        let cursor = self.cursor?;
        self.swallow_fire = true;
        if !inventory.has(cursor) {
            // Gone from the inventory: HL2's pick of nothing just cancels.
            self.cursor = None;
            self.cues.push((Cue::Close, true));
            return None;
        }
        if !inventory.usable(arsenal, cursor) {
            self.cues.push((Cue::Deny, false));
            return None;
        }
        self.cursor = None;
        self.cues.push((Cue::Pick, false));
        self.cues.push((Cue::Close, true));
        Some(cursor)
    }

    /// `Esc`: closes the list with its sound. Returns whether it was up
    /// (else `Esc` does what it otherwise does).
    pub(crate) fn cancel(&mut self) -> bool {
        let open = self.dismiss().is_some();
        if open {
            self.cues.push((Cue::Close, false));
        }
        open
    }

    /// Closes the list without a pick or a sound, returning what the cursor
    /// was on.
    pub(crate) fn dismiss(&mut self) -> Option<WeaponId> {
        self.idle = f32::INFINITY;
        self.cursor.take()
    }

    /// Whether the fire buttons must be ignored this tick: a click that
    /// confirmed (or was denied) a pick, held on. `held` is whether either
    /// button is down and `pressed` whether one went down this frame. Ticks
    /// run before the frame's input is latched, so the click that confirms
    /// is seen here, with the list still up, before [`Self::confirm`] takes
    /// it. A wheel pulse counts for this tick without a held button.
    pub(crate) fn swallows_fire(&mut self, held: bool, pressed: bool) -> bool {
        self.swallow_fire |= pressed && self.is_open();
        self.swallow_fire &= held || pressed;
        self.swallow_fire
    }
}

#[cfg(test)]
mod tests {
    use rl_data::GameData;

    use super::*;

    fn arsenal() -> Arsenal {
        Arsenal::new(&GameData::builtin().expect("builtin"))
    }

    /// The loadout: Crowbar, Stun Baton, Gravity Gun (in hand), pistol,
    /// SMG, frag.
    fn loadout(arsenal: &Arsenal) -> Inventory {
        Inventory::loadout(
            arsenal,
            &GameData::builtin().expect("builtin").rules.loadout,
        )
    }

    /// The loadout's weapons with no ammo at all.
    fn unloaded(arsenal: &Arsenal) -> Inventory {
        let mut empty = Inventory::empty(arsenal);
        for weapon in loadout(arsenal).weapons() {
            empty.give_weapon(arsenal, weapon);
        }
        empty
    }

    fn id(arsenal: &Arsenal, name: &str) -> WeaponId {
        arsenal.find_weapon(name).expect("weapon")
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
    fn the_list_holds_fades_while_still_picking_then_closes() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        selector.advance(HOLD_SECS - 0.01);
        assert_eq!(selector.opacity(), 1.0);
        selector.advance(0.01 + FADE_SECS / 2.0);
        assert!((selector.opacity() - 0.5).abs() < 0.05);
        assert!(selector.is_open(), "a click while it fades still picks");
        let mut fading = selector.clone();
        assert!(fading.confirm(&inventory, &arsenal).is_some());
        selector.advance(FADE_SECS / 2.0);
        assert!(!selector.is_open());
        assert_eq!(selector.opacity(), 0.0);
        assert_eq!(selector.confirm(&inventory, &arsenal), None);
        // The next scroll starts over from the weapon in hand.
        selector.scroll(&inventory, &arsenal, true);
        let mut again = WeaponSelector::default();
        again.scroll(&inventory, &arsenal, true);
        assert_eq!(selector.cursor(), again.cursor());
    }

    #[test]
    fn a_scroll_renews_the_hold_even_while_fading() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        selector.advance(HOLD_SECS + FADE_SECS - 0.05);
        selector.scroll(&inventory, &arsenal, true);
        assert_eq!(selector.opacity(), 1.0);
        selector.advance(HOLD_SECS + FADE_SECS - 0.05);
        assert!(selector.is_open());
    }

    #[test]
    fn confirming_returns_the_pick_and_swallows_the_click_until_released() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        let picked = selector.cursor();
        assert_eq!(selector.confirm(&inventory, &arsenal), picked);
        assert!(!selector.is_open());
        assert!(selector.swallows_fire(true, false), "still the same click");
        assert!(selector.swallows_fire(true, false));
        assert!(!selector.swallows_fire(false, false), "released");
        assert!(!selector.swallows_fire(true, true), "a new press fires");
    }

    #[test]
    fn a_fire_pulse_without_a_held_button_confirms_instead_of_firing() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        assert!(selector.swallows_fire(false, true));
        assert!(selector.confirm(&inventory, &arsenal).is_some());
        assert!(!selector.swallows_fire(false, false));
        assert!(!selector.swallows_fire(false, true), "a new pulse fires");
    }

    #[test]
    fn the_list_cues_its_sounds_as_hl2dm() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        // Opening the list is a step, not an "open" sound.
        selector.scroll(&inventory, &arsenal, true);
        selector.scroll(&inventory, &arsenal, true);
        assert_eq!(selector.take_cues(), [Cue::Step, Cue::Step]);
        assert!(selector.take_cues().is_empty(), "taken");
        // A pick, and the close its queued `cancelselect` makes a frame on.
        selector.confirm(&inventory, &arsenal);
        assert_eq!(selector.take_cues(), [Cue::Pick]);
        assert_eq!(selector.take_cues(), [Cue::Close]);
        assert!(selector.take_cues().is_empty());
        // Timing out is silent.
        selector.scroll(&inventory, &arsenal, true);
        selector.take_cues();
        selector.advance(HOLD_SECS + FADE_SECS + 0.1);
        assert!(!selector.is_open());
        assert!(selector.take_cues().is_empty(), "no sound on the timeout");
        // Esc closes it with the close sound, and only while it is up.
        selector.scroll(&inventory, &arsenal, true);
        selector.take_cues();
        assert!(selector.cancel());
        assert_eq!(selector.take_cues(), [Cue::Close]);
        assert!(!selector.cancel(), "Esc with the list down pauses");
        assert!(selector.take_cues().is_empty());
        // The last-weapon key closes it silently.
        selector.scroll(&inventory, &arsenal, true);
        selector.take_cues();
        selector.dismiss();
        assert!(selector.take_cues().is_empty());
    }

    #[test]
    fn a_scroll_that_finds_nothing_is_silent() {
        let arsenal = arsenal();
        let mut selector = WeaponSelector::default();
        selector.scroll(&Inventory::empty(&arsenal), &arsenal, true);
        // The tools need no ammo; take a list with nothing usable.
        let mut dry = Inventory::empty(&arsenal);
        dry.give_weapon(&arsenal, id(&arsenal, "frag"));
        selector.scroll(&dry, &arsenal, true);
        assert!(!selector.is_open());
        assert!(selector.take_cues().is_empty());
    }

    #[test]
    fn a_weapon_that_cannot_be_used_is_denied_and_the_list_stays_up() {
        let arsenal = arsenal();
        let mut selector = WeaponSelector::default();
        selector.select_slot(&loadout(&arsenal), &arsenal, 5);
        assert_eq!(selector.cursor(), Some(id(&arsenal, "frag")));
        selector.take_cues();
        // The last grenade went before the click.
        assert_eq!(selector.confirm(&unloaded(&arsenal), &arsenal), None);
        assert_eq!(selector.take_cues(), [Cue::Deny]);
        assert!(selector.is_open());
        assert!(
            selector.swallows_fire(true, false),
            "the click does not fire"
        );
        // A weapon no longer carried just closes the list.
        assert_eq!(
            selector.confirm(&Inventory::empty(&arsenal), &arsenal),
            None
        );
        assert!(!selector.is_open());
        selector.take_cues();
        assert_eq!(selector.take_cues(), [Cue::Close]);
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
        assert!(!selector.swallows_fire(true, false));
    }

    #[test]
    fn a_tick_before_the_click_is_latched_already_swallows_it() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.scroll(&inventory, &arsenal, true);
        assert!(selector.swallows_fire(true, true), "the confirming press");
        selector.confirm(&inventory, &arsenal);
        assert!(selector.swallows_fire(true, false), "and its later ticks");
        assert!(!selector.swallows_fire(false, false));
    }

    #[test]
    fn a_slot_key_opens_the_list_on_that_slots_first_weapon() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        // The Gravity Gun is in hand, last in slot 1; the list still opens
        // on the slot's first weapon, as HL2's buckets do.
        selector.select_slot(&inventory, &arsenal, 1);
        assert!(selector.is_open());
        assert_eq!(selector.cursor(), Some(id(&arsenal, "bar")));
        assert_eq!(selector.take_cues(), [Cue::Step]);
        selector.select_slot(&inventory, &arsenal, 2);
        assert_eq!(selector.cursor(), Some(id(&arsenal, "sidearm")));
        assert_eq!(
            selector.confirm(&inventory, &arsenal),
            Some(id(&arsenal, "sidearm")),
            "a click picks it"
        );
    }

    #[test]
    fn repeating_a_slot_key_steps_through_the_slot_and_wraps() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        let slot_one = ["bar", "stunstick", "manipulator", "bar"];
        for name in slot_one {
            selector.select_slot(&inventory, &arsenal, 1);
            assert_eq!(selector.cursor(), Some(id(&arsenal, name)));
        }
    }

    #[test]
    fn an_empty_slot_still_sounds_a_step_and_moves_nothing() {
        let arsenal = arsenal();
        let inventory = loadout(&arsenal);
        let mut selector = WeaponSelector::default();
        selector.select_slot(&inventory, &arsenal, 9);
        assert!(!selector.is_open());
        assert_eq!(selector.take_cues(), [Cue::Step], "never a denial");
        selector.select_slot(&inventory, &arsenal, 2);
        selector.select_slot(&inventory, &arsenal, 4);
        assert_eq!(selector.cursor(), Some(id(&arsenal, "sidearm")));
        assert_eq!(selector.take_cues(), [Cue::Step, Cue::Step]);
    }

    #[test]
    fn a_press_without_a_list_fires() {
        let mut selector = WeaponSelector::default();
        assert!(!selector.swallows_fire(true, true));
    }
}
