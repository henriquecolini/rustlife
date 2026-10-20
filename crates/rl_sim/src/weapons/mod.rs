//! Weapons: what each player carries and when they fire.
//!
//! - [`Arsenal`] is every weapon and ammo type of the game data, numbered:
//!   [`WeaponId`] and [`AmmoId`] are indices into it, the same on every
//!   machine running the same data.
//! - [`Inventory`] is one player's weapons, clips and carried ammo, and the
//!   state of the weapon in hand: deploying after a switch, reloading, or
//!   ready. [`Inventory::tick`] turns a tick's buttons into at most one
//!   trigger pull ([`WeaponTick`]); what the shot then does — bullets, a
//!   swing, a projectile — is the simulation's business, from the fire
//!   mode's [`Shot`].
//! - [`projectile`]: projectiles in flight.
//!
//! The Manipulator is a weapon too, but a tool: its fire modes
//! ([`Shot::Manipulator`]) are run by `tools::manipulator`, which needs to
//! see the buttons every tick (holding, pulling). The inventory only tells
//! the simulation when it is in hand and ready ([`Inventory::tool`]).
//!
//! Like movement, everything here is deterministic, so a client can predict
//! its own weapon.

use rl_data::{
    AmmoType, FireAmmo, FireConfig, GameData, LoadoutConfig, Reload, Shot, WeaponConfig,
};
use serde::{Deserialize, Serialize};

use crate::movement::{Buttons, UserCmd};
use crate::tools::manipulator::Intervals;

pub mod projectile;

/// A weapon of the [`Arsenal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WeaponId(u16);

impl WeaponId {
    /// The weapon's index in the arsenal (weapons sorted by id).
    #[must_use]
    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    /// The weapon at `index`.
    #[must_use]
    pub fn from_index(index: usize) -> Self {
        Self(u16::try_from(index).unwrap_or(u16::MAX))
    }
}

/// An ammo type of the [`Arsenal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AmmoId(u16);

impl AmmoId {
    /// The type's index in the arsenal (types sorted by id).
    #[must_use]
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Primary or secondary fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// Primary fire.
    Primary,
    /// Secondary fire.
    Secondary,
}

/// Every weapon and ammo type, numbered.
#[derive(Debug, Clone)]
pub struct Arsenal {
    weapons: Vec<(String, WeaponConfig)>,
    ammo: Vec<(String, AmmoType)>,
}

impl Arsenal {
    /// The weapons and ammo types of `data`, sorted by id.
    #[must_use]
    pub fn new(data: &GameData) -> Self {
        Self {
            weapons: data
                .weapons
                .iter()
                .map(|(id, weapon)| (id.clone(), weapon.clone()))
                .collect(),
            ammo: data
                .ammo
                .types
                .iter()
                .map(|(id, ammo)| (id.clone(), ammo.clone()))
                .collect(),
        }
    }

    /// A weapon's data.
    #[must_use]
    pub fn weapon(&self, weapon: WeaponId) -> Option<&WeaponConfig> {
        self.weapons.get(weapon.index()).map(|(_, config)| config)
    }

    /// A weapon's id string (its file name).
    #[must_use]
    pub fn weapon_name(&self, weapon: WeaponId) -> Option<&str> {
        self.weapons.get(weapon.index()).map(|(id, _)| id.as_str())
    }

    /// The weapon with id string `id`.
    #[must_use]
    pub fn find_weapon(&self, id: &str) -> Option<WeaponId> {
        self.weapons
            .binary_search_by(|(name, _)| name.as_str().cmp(id))
            .ok()
            .map(WeaponId::from_index)
    }

    /// Every weapon, in id order.
    pub fn weapons(&self) -> impl Iterator<Item = (WeaponId, &WeaponConfig)> {
        self.weapons
            .iter()
            .enumerate()
            .map(|(index, (_, config))| (WeaponId::from_index(index), config))
    }

    /// One fire mode of a weapon.
    #[must_use]
    pub fn fire(&self, weapon: WeaponId, mode: Mode) -> Option<&FireConfig> {
        let config = self.weapon(weapon)?;
        match mode {
            Mode::Primary => config.primary.as_ref(),
            Mode::Secondary => config.secondary.as_ref(),
        }
    }

    /// The ammo type with id string `id`.
    #[must_use]
    pub fn find_ammo(&self, id: &str) -> Option<AmmoId> {
        self.ammo
            .binary_search_by(|(name, _)| name.as_str().cmp(id))
            .ok()
            .map(|index| AmmoId(u16::try_from(index).unwrap_or(u16::MAX)))
    }

    /// An ammo type's data.
    #[must_use]
    pub fn ammo(&self, ammo: AmmoId) -> Option<&AmmoType> {
        self.ammo.get(ammo.index()).map(|(_, config)| config)
    }

    /// The number of ammo types.
    #[must_use]
    pub fn ammo_types(&self) -> usize {
        self.ammo.len()
    }

    /// The ammo type a weapon's clip holds.
    fn clip_ammo(&self, weapon: WeaponId) -> Option<AmmoId> {
        self.find_ammo(&self.weapon(weapon)?.clip.as_ref()?.ammo)
    }
}

/// Whether a weapon is a tool run by `tools::manipulator`.
fn is_tool(config: &WeaponConfig) -> bool {
    config
        .fire_modes()
        .any(|fire| matches!(fire.shot, Shot::Manipulator))
}

/// A player's request for another weapon, from a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The weapons of a slot (a number key): the first, or the next after
    /// the one in hand if it is in that slot.
    Slot(u8),
    /// The next usable weapon after the one in hand, by slot and position.
    Next,
    /// The usable weapon before the one in hand.
    Previous,
    /// The weapon in hand before this one.
    Last,
}

/// What the weapon in hand is doing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    /// Ready to fire (once the fire interval has passed).
    Ready,
    /// Being brought up after a switch; this many seconds left.
    Deploying(f32),
    /// Reloading; this many seconds until the clip (or the next round)
    /// goes in.
    Reloading(f32),
}

/// A carried weapon and its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Carried {
    weapon: WeaponId,
    clip: u32,
}

/// What a player's weapon did during a tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WeaponTick {
    /// The trigger was pulled: the shot of this weapon's fire mode goes
    /// off (its ammo is already spent).
    pub fired: Option<(WeaponId, Mode)>,
    /// A different weapon was brought up.
    pub switched: bool,
    /// A reload began.
    pub reloading: bool,
}

/// One player's weapons and ammo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inventory {
    /// Carried weapons, in arsenal order.
    weapons: Vec<Carried>,
    /// Carried rounds by ammo type.
    ammo: Vec<u32>,
    active: Option<WeaponId>,
    /// The weapon in hand before the current one.
    last: Option<WeaponId>,
    phase: Phase,
    /// Seconds until the weapon in hand may fire again.
    cooldown: f32,
    /// Buttons held last tick, for semi-automatic fire and reload presses.
    old_buttons: Buttons,
}

impl Inventory {
    /// Nothing at all.
    #[must_use]
    pub fn empty(arsenal: &Arsenal) -> Self {
        Self {
            weapons: Vec::new(),
            ammo: vec![0; arsenal.ammo_types()],
            active: None,
            last: None,
            phase: Phase::Ready,
            cooldown: 0.0,
            old_buttons: Buttons::NONE,
        }
    }

    /// What a player spawns with: the loadout's weapons with full clips,
    /// its ammo, and its active weapon in hand, ready.
    #[must_use]
    pub fn loadout(arsenal: &Arsenal, loadout: &LoadoutConfig) -> Self {
        let mut inventory = Self::empty(arsenal);
        for id in &loadout.weapons {
            if let Some(weapon) = arsenal.find_weapon(id) {
                inventory.give_weapon(arsenal, weapon);
            }
        }
        for (id, &count) in &loadout.ammo {
            if let Some(ammo) = arsenal.find_ammo(id) {
                inventory.give_ammo(arsenal, ammo, count);
            }
        }
        inventory.active = arsenal.find_weapon(&loadout.active);
        inventory.last = None;
        inventory.phase = Phase::Ready;
        inventory
    }

    /// The weapon in hand.
    #[must_use]
    pub fn active(&self) -> Option<WeaponId> {
        self.active
    }

    /// What the weapon in hand is doing.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Every carried weapon, in arsenal order.
    pub fn weapons(&self) -> impl Iterator<Item = WeaponId> + '_ {
        self.weapons.iter().map(|carried| carried.weapon)
    }

    /// Whether `weapon` is carried.
    #[must_use]
    pub fn has(&self, weapon: WeaponId) -> bool {
        self.carried(weapon).is_some()
    }

    /// Rounds in a carried weapon's clip (`None` if not carried).
    #[must_use]
    pub fn clip(&self, weapon: WeaponId) -> Option<u32> {
        self.carried(weapon).map(|carried| carried.clip)
    }

    /// Carried rounds of an ammo type.
    #[must_use]
    pub fn ammo(&self, ammo: AmmoId) -> u32 {
        self.ammo.get(ammo.index()).copied().unwrap_or(0)
    }

    /// Adds `weapon` with a full clip; `false` if it was already carried.
    /// With nothing in hand, it is taken in hand.
    pub fn give_weapon(&mut self, arsenal: &Arsenal, weapon: WeaponId) -> bool {
        let Some(config) = arsenal.weapon(weapon) else {
            return false;
        };
        if self.has(weapon) {
            return false;
        }
        let clip = config.clip.as_ref().map_or(0, |clip| clip.size);
        let at = self
            .weapons
            .partition_point(|carried| carried.weapon < weapon);
        self.weapons.insert(at, Carried { weapon, clip });
        if self.active.is_none() {
            self.active = Some(weapon);
        }
        true
    }

    /// Adds up to `count` rounds of `ammo`, up to the type's maximum;
    /// returns how many were taken.
    pub fn give_ammo(&mut self, arsenal: &Arsenal, ammo: AmmoId, count: u32) -> u32 {
        let (Some(config), Some(carried)) = (arsenal.ammo(ammo), self.ammo.get_mut(ammo.index()))
        else {
            return 0;
        };
        let taken = count.min(config.max.saturating_sub(*carried));
        *carried += taken;
        taken
    }

    /// Takes `weapon` in hand, if carried and not already in hand; it then
    /// deploys. Returns whether it switched.
    pub fn select(&mut self, arsenal: &Arsenal, weapon: WeaponId) -> bool {
        if self.active == Some(weapon) || !self.has(weapon) {
            return false;
        }
        let deploy = arsenal
            .weapon(weapon)
            .map_or(0.0, |config| config.deploy_time);
        self.last = self.active;
        self.active = Some(weapon);
        self.phase = if deploy > 0.0 {
            Phase::Deploying(deploy)
        } else {
            Phase::Ready
        };
        self.cooldown = 0.0;
        true
    }

    /// The weapon in hand before the current one, if still carried.
    #[must_use]
    pub fn last(&self) -> Option<WeaponId> {
        self.last.filter(|&weapon| self.has(weapon))
    }

    /// The weapon `choice` asks for, if any differs from the one in hand.
    #[must_use]
    pub fn choose(&self, arsenal: &Arsenal, choice: Choice) -> Option<WeaponId> {
        let order = |weapon: &WeaponId| {
            arsenal
                .weapon(*weapon)
                .map_or((u8::MAX, u8::MAX), |config| (config.slot, config.position))
        };
        let mut carried: Vec<WeaponId> = self.weapons().collect();
        carried.sort_by_key(order);
        let chosen = match choice {
            Choice::Last => self.last(),
            Choice::Slot(slot) => {
                let in_slot: Vec<WeaponId> = carried
                    .iter()
                    .copied()
                    .filter(|weapon| order(weapon).0 == slot)
                    .collect();
                let at = self
                    .active
                    .and_then(|active| in_slot.iter().position(|&weapon| weapon == active));
                at.map_or(in_slot.first(), |at| in_slot.get((at + 1) % in_slot.len()))
                    .copied()
            }
            Choice::Next | Choice::Previous => {
                let usable: Vec<WeaponId> = carried
                    .iter()
                    .copied()
                    .filter(|&weapon| self.usable(arsenal, weapon) || Some(weapon) == self.active)
                    .collect();
                let at = self
                    .active
                    .and_then(|active| usable.iter().position(|&weapon| weapon == active));
                let len = usable.len().max(1);
                let index = match (choice, at) {
                    (Choice::Next, Some(at)) => (at + 1) % len,
                    (_, Some(at)) => (at + len - 1) % len,
                    (_, None) => 0,
                };
                usable.get(index).copied()
            }
        };
        chosen.filter(|&weapon| Some(weapon) != self.active)
    }

    /// Whether `weapon` has something to fire: a mode that needs no ammo,
    /// rounds in its clip, or carried rounds for it.
    #[must_use]
    pub fn usable(&self, arsenal: &Arsenal, weapon: WeaponId) -> bool {
        let Some(config) = arsenal.weapon(weapon) else {
            return false;
        };
        let clip = self.clip(weapon).unwrap_or(0);
        config.fire_modes().any(|fire| match &fire.ammo {
            FireAmmo::Unlimited => true,
            FireAmmo::Clip(count) => {
                clip >= *count
                    || arsenal
                        .clip_ammo(weapon)
                        .is_some_and(|ammo| self.ammo(ammo) > 0)
            }
            FireAmmo::Reserve { ammo, count } => arsenal
                .find_ammo(ammo)
                .is_some_and(|ammo| self.ammo(ammo) >= *count),
        })
    }

    /// When the weapon in hand is a tool (the Manipulator) and deployed:
    /// the rest after each of its actions, for `tools::manipulator`.
    #[must_use]
    pub fn tool(&self, arsenal: &Arsenal) -> Option<Intervals> {
        let config = arsenal.weapon(self.active?)?;
        if !is_tool(config) || matches!(self.phase, Phase::Deploying(_)) {
            return None;
        }
        let interval = |fire: &Option<FireConfig>| fire.as_ref().map_or(0.0, |fire| fire.interval);
        Some(Intervals {
            primary: interval(&config.primary),
            secondary: interval(&config.secondary),
        })
    }

    /// Advances the weapon in hand by one tick of `dt` seconds under `cmd`:
    /// switching, deploying, reloading and pulling the trigger.
    pub fn tick(&mut self, arsenal: &Arsenal, cmd: &UserCmd, dt: f32) -> WeaponTick {
        let mut result = WeaponTick::default();
        let pressed = Buttons::from_bits(cmd.buttons.bits() & !self.old_buttons.bits());
        self.old_buttons = cmd.buttons;
        self.cooldown = (self.cooldown - dt).max(0.0);
        if let Some(weapon) = cmd.select {
            result.switched = self.select(arsenal, weapon);
        }
        self.advance_phase(arsenal, dt);

        let Some((weapon, config)) = self
            .active
            .and_then(|weapon| Some((weapon, arsenal.weapon(weapon)?)))
        else {
            return result;
        };
        if is_tool(config) || matches!(self.phase, Phase::Deploying(_)) {
            return result;
        }
        let wanted = |fire: &Option<FireConfig>, button: Buttons| {
            fire.as_ref().is_some_and(|fire| {
                cmd.buttons.contains(button) && (fire.automatic || pressed.contains(button))
            })
        };
        let primary = wanted(&config.primary, Buttons::ATTACK);
        let secondary = wanted(&config.secondary, Buttons::ATTACK2);

        // Firing interrupts a round-by-round reload with what is loaded.
        if matches!(self.phase, Phase::Reloading(_))
            && matches!(
                config.clip.as_ref().map(|clip| clip.reload),
                Some(Reload::PerRound { .. })
            )
            && (primary || secondary)
            && self.clip(weapon).is_some_and(|clip| clip > 0)
        {
            self.phase = Phase::Ready;
        }
        if self.phase != Phase::Ready || self.cooldown > 0.0 {
            return result;
        }

        for (mode, wanted) in [(Mode::Primary, primary), (Mode::Secondary, secondary)] {
            if !wanted {
                continue;
            }
            if self.spend(arsenal, weapon, mode) {
                if let Some(fire) = arsenal.fire(weapon, mode) {
                    self.cooldown = fire.interval;
                }
                result.fired = Some((weapon, mode));
            } else {
                result.reloading = self.start_reload(arsenal);
            }
            return result;
        }
        if pressed.contains(Buttons::RELOAD) || self.clip(weapon) == Some(0) {
            result.reloading = self.start_reload(arsenal);
        }
        result
    }

    /// Spends the ammo of one shot of `mode`; `false` (spending nothing)
    /// if there is not enough.
    fn spend(&mut self, arsenal: &Arsenal, weapon: WeaponId, mode: Mode) -> bool {
        let Some(fire) = arsenal.fire(weapon, mode) else {
            return false;
        };
        match &fire.ammo {
            FireAmmo::Unlimited => true,
            FireAmmo::Clip(count) => match self.carried_mut(weapon) {
                Some(carried) if carried.clip >= *count => {
                    carried.clip -= count;
                    true
                }
                _ => false,
            },
            FireAmmo::Reserve { ammo, count } => {
                match arsenal
                    .find_ammo(ammo)
                    .and_then(|ammo| self.ammo.get_mut(ammo.index()))
                {
                    Some(carried) if *carried >= *count => {
                        *carried -= count;
                        true
                    }
                    _ => false,
                }
            }
        }
    }

    /// Starts reloading the weapon in hand if its clip is not full and
    /// rounds are carried for it; returns whether it started.
    fn start_reload(&mut self, arsenal: &Arsenal) -> bool {
        let Some(weapon) = self.active else {
            return false;
        };
        let (Some(clip), Some(ammo)) = (
            arsenal
                .weapon(weapon)
                .and_then(|config| config.clip.as_ref()),
            arsenal.clip_ammo(weapon),
        ) else {
            return false;
        };
        let loaded = self.clip(weapon).unwrap_or(0);
        if self.phase != Phase::Ready || loaded >= clip.size || self.ammo(ammo) == 0 {
            return false;
        }
        self.phase = Phase::Reloading(match clip.reload {
            Reload::Whole { time } => time,
            Reload::PerRound { start, .. } => start,
        });
        true
    }

    /// Counts down deploying and reloading, loading rounds as reloads
    /// complete.
    fn advance_phase(&mut self, arsenal: &Arsenal, dt: f32) {
        match self.phase {
            Phase::Ready => {}
            Phase::Deploying(left) => {
                self.phase = if left > dt {
                    Phase::Deploying(left - dt)
                } else {
                    Phase::Ready
                };
            }
            Phase::Reloading(left) => {
                if left > dt {
                    self.phase = Phase::Reloading(left - dt);
                } else {
                    self.finish_reload_step(arsenal, left - dt);
                }
            }
        }
    }

    /// A reload's timer ran out (`overshoot` seconds ago, ≤ 0): load the
    /// clip, or one round and time the next.
    fn finish_reload_step(&mut self, arsenal: &Arsenal, overshoot: f32) {
        self.phase = Phase::Ready;
        let Some(weapon) = self.active else {
            return;
        };
        let (Some(clip), Some(ammo)) = (
            arsenal
                .weapon(weapon)
                .and_then(|config| config.clip.clone()),
            arsenal.clip_ammo(weapon),
        ) else {
            return;
        };
        let reserve = self.ammo(ammo);
        let Some(carried) = self.carried_mut(weapon) else {
            return;
        };
        let room = clip.size.saturating_sub(carried.clip);
        let loaded = match clip.reload {
            Reload::Whole { .. } => room.min(reserve),
            Reload::PerRound { .. } => room.min(reserve).min(1),
        };
        carried.clip += loaded;
        let full = carried.clip >= clip.size;
        if let Some(carried) = self.ammo.get_mut(ammo.index()) {
            *carried -= loaded;
        }
        if let Reload::PerRound { each, .. } = clip.reload
            && !full
            && reserve > loaded
        {
            self.phase = Phase::Reloading((each + overshoot).max(0.0));
        }
    }

    fn carried(&self, weapon: WeaponId) -> Option<&Carried> {
        self.weapons
            .binary_search_by(|carried| carried.weapon.cmp(&weapon))
            .ok()
            .map(|index| &self.weapons[index])
    }

    fn carried_mut(&mut self, weapon: WeaponId) -> Option<&mut Carried> {
        self.weapons
            .binary_search_by(|carried| carried.weapon.cmp(&weapon))
            .ok()
            .map(|index| &mut self.weapons[index])
    }
}

#[cfg(test)]
mod tests;
