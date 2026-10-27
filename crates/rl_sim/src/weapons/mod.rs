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

/// Seconds a trigger pulled on an empty weapon waits before it clicks again.
const EMPTY_CLICK_WAIT: f32 = 0.5;

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
    /// A particular carried weapon (a pick from the weapon list).
    Weapon(WeaponId),
}

/// What the weapon in hand is doing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    /// Ready to fire (once the fire modes' intervals have passed).
    Ready,
    /// Being brought up after a switch; this many seconds left.
    Deploying(f32),
    /// Reloading; this many seconds until the clip (or the current step of
    /// a round-by-round reload) is done. See [`Inventory::reload_step`].
    Reloading(f32),
    /// Primed, a fire mode that [goes off on release](rl_data::FireConfig::on_release)
    /// held: a grenade drawn back, to be thrown when its button is let go.
    Priming(Mode),
    /// A shot on its way: the fire mode, and the seconds until it goes off
    /// (a [charge](rl_data::FireConfig::charge) after the press, or for
    /// a primed mode the time the arm takes after the button is let go).
    Charging(Mode, f32),
}

/// Which part of a reload is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReloadStep {
    /// The whole clip is swapped.
    Whole,
    /// A round-by-round reload getting started.
    Start,
    /// One round going in.
    Round,
    /// A round-by-round reload closing.
    Finish,
}

/// Something a weapon does that is heard (and seen in its animation) apart
/// from its shots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Noise {
    /// The trigger pulled on an empty weapon.
    Dry,
    /// The shotgun's pump.
    Pump,
    /// A delayed shot building up (the Pulse Rifle's ball).
    Charge,
}

/// A carried weapon and its clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Carried {
    weapon: WeaponId,
    clip: u32,
}

/// What a player's weapon did during a tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WeaponTick {
    /// The trigger was pulled: the shot of this weapon's fire mode goes
    /// off (its ammo is already spent).
    pub fired: Option<(WeaponId, Mode)>,
    /// A different weapon was brought up.
    pub switched: bool,
    /// A reload began.
    pub reloading: bool,
    /// A fire mode that goes off on release began to be held.
    pub primed: Option<(WeaponId, Mode)>,
    /// Something besides a shot that is heard.
    pub noise: Option<Noise>,
    /// A consumable weapon that ran out was put away.
    pub retired: Option<WeaponId>,
    /// The scope was turned on or off: the new state.
    pub zoom: Option<bool>,
    /// The accuracy penalty the shot fired was subject to, seconds.
    pub penalty: f32,
}

/// What the weapon in hand is waiting on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Latches {
    /// It has to work its action before it fires again.
    pump_due: bool,
    /// The trigger was pulled on an empty weapon and not let go since.
    fire_on_empty: bool,
    /// A rocket it fired is still in the air.
    projectile_out: bool,
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
    /// Seconds until each fire mode may go off (`<= 0`: ready), primary
    /// then secondary. A shot adds its interval to what is left, so the
    /// little time a tick overshot a shot's moment carries over (at most
    /// one tick's worth).
    next: [f32; 2],
    /// Seconds since the weapon in hand last fired.
    since_shot: f32,
    /// Buttons held last tick, for semi-automatic fire and reload presses.
    old_buttons: Buttons,
    /// The step of the reload in progress.
    reload: ReloadStep,
    /// The pistol's accuracy penalty, seconds (see [`rl_data::AccuracyConfig`]).
    penalty: f32,
    /// Seconds until an emptied clip reloads of its own accord.
    reload_due: Option<f32>,
    /// Seconds until the trigger may click again.
    empty_wait: f32,
    /// Conditions the weapon in hand is waiting on.
    latch: Latches,
    /// The scope is on.
    zoomed: bool,
    /// The laser is on.
    laser: bool,
    /// Counts clicks and pumps, for views to notice them: wraps.
    dry_count: u8,
    pump_count: u8,
    reload_count: u8,
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
            next: [0.0; 2],
            since_shot: f32::MAX,
            old_buttons: Buttons::NONE,
            reload: ReloadStep::Whole,
            penalty: 0.0,
            latch: Latches::default(),
            reload_due: None,
            empty_wait: 0.0,
            zoomed: false,
            laser: true,
            dry_count: 0,
            pump_count: 0,
            reload_count: 0,
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

    /// The step of the reload in progress (or the last one, once over).
    #[must_use]
    pub fn reload_step(&self) -> ReloadStep {
        self.reload
    }

    /// How many reload steps have begun, wrapping: a view compares it with
    /// what it saw to notice a new step.
    #[must_use]
    pub fn reload_count(&self) -> u8 {
        self.reload_count
    }

    /// How many times the trigger has clicked on an empty weapon, wrapping.
    #[must_use]
    pub fn dry_count(&self) -> u8 {
        self.dry_count
    }

    /// How many times the weapon has worked its action, wrapping.
    #[must_use]
    pub fn pump_count(&self) -> u8 {
        self.pump_count
    }

    /// Whether the laser is on.
    #[must_use]
    pub fn laser(&self) -> bool {
        self.laser
    }

    /// Whether the scope is on.
    #[must_use]
    pub fn zoomed(&self) -> bool {
        self.zoomed
    }

    /// The accuracy penalty, seconds: how much firing has widened the
    /// weapon's cone.
    #[must_use]
    pub fn penalty(&self) -> f32 {
        self.penalty
    }

    /// Seconds until a fire mode may go off (zero or less when ready).
    #[must_use]
    pub fn until_ready(&self, mode: Mode) -> f32 {
        self.next[mode as usize]
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
        self.put_away();
        self.phase = if deploy > 0.0 {
            Phase::Deploying(deploy)
        } else {
            Phase::Ready
        };
        self.next = [deploy; 2];
        true
    }

    /// Forgets what the weapon in hand was in the middle of.
    fn put_away(&mut self) {
        self.since_shot = f32::MAX;
        self.reload = ReloadStep::Whole;
        self.penalty = 0.0;
        self.latch.pump_due = false;
        self.reload_due = None;
        self.latch.fire_on_empty = false;
        self.zoomed = false;
        self.laser = true;
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
                self.cycle(arsenal, self.active, choice == Choice::Next)
            }
            Choice::Weapon(weapon) => self.has(weapon).then_some(weapon),
        };
        chosen.filter(|&weapon| Some(weapon) != self.active)
    }

    /// The usable weapon after (`forward`) or before `from` in slot order,
    /// wrapping round; `from` itself counts as usable so it has a place in
    /// the order. Starts at the first weapon if `from` is not carried.
    #[must_use]
    pub fn cycle(
        &self,
        arsenal: &Arsenal,
        from: Option<WeaponId>,
        forward: bool,
    ) -> Option<WeaponId> {
        let order = |weapon: &WeaponId| {
            arsenal
                .weapon(*weapon)
                .map_or((u8::MAX, u8::MAX), |config| (config.slot, config.position))
        };
        let mut usable: Vec<WeaponId> = self
            .weapons()
            .filter(|&weapon| self.usable(arsenal, weapon) || Some(weapon) == from)
            .collect();
        usable.sort_by_key(order);
        let at = from.and_then(|from| usable.iter().position(|&weapon| weapon == from));
        let len = usable.len().max(1);
        let index = match (forward, at) {
            (true, Some(at)) => (at + 1) % len,
            (false, Some(at)) => (at + len - 1) % len,
            (_, None) => 0,
        };
        usable.get(index).copied()
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
    ///
    /// The order is HL2's `ItemPostFrame`: a weapon that has to work its
    /// action (a pump) does that first; the alternate fire has priority over
    /// the primary; a trigger pulled on an empty weapon clicks, then reloads;
    /// with no button down an empty weapon reloads (or, with nothing left to
    /// fire, gives way to the best weapon with ammo).
    pub fn tick(&mut self, arsenal: &Arsenal, cmd: &UserCmd, dt: f32) -> WeaponTick {
        let reloads = self.reload_count;
        let mut result = self.run(arsenal, cmd, dt);
        // Every step of a reload counts as a reload begun.
        result.reloading |= self.reload_count != reloads;
        result
    }

    fn run(&mut self, arsenal: &Arsenal, cmd: &UserCmd, dt: f32) -> WeaponTick {
        let mut result = WeaponTick::default();
        let pressed = Buttons::from_bits(cmd.buttons.bits() & !self.old_buttons.bits());
        let held = cmd.buttons;
        self.old_buttons = held;
        for next in &mut self.next {
            *next = (*next - dt).max(-dt);
        }
        self.since_shot += dt;
        self.empty_wait = (self.empty_wait - dt).max(0.0);
        if let Some(weapon) = cmd.select {
            result.switched = self.select(arsenal, weapon);
        }
        self.advance_phase(arsenal, dt, &mut result);
        if let Some(retired) = self.retire_spent(arsenal) {
            result.retired = Some(retired);
            result.switched = true;
        }

        let Some((weapon, config)) = self
            .active
            .and_then(|weapon| Some((weapon, arsenal.weapon(weapon)?)))
        else {
            return result;
        };
        if is_tool(config) || matches!(self.phase, Phase::Deploying(_)) {
            return result;
        }
        self.drain_penalty(config, held, dt);
        if let Phase::Priming(mode) = self.phase {
            return self.release(arsenal, weapon, mode, cmd, result);
        }
        if matches!(self.phase, Phase::Charging(..)) {
            return result;
        }
        if pressed.contains(Buttons::ATTACK2) {
            if config.zoom.is_some() {
                self.zoomed = !self.zoomed;
                result.zoom = Some(self.zoomed);
            }
            if config.laser {
                self.laser = !self.laser;
            }
        }
        if let Some(due) = self.reload_due.as_mut() {
            *due -= dt;
            if *due <= 0.0 {
                self.reload_due = None;
                if self.phase == Phase::Ready {
                    result.reloading = self.start_reload(arsenal);
                }
            }
        }
        self.interrupt_reload(arsenal, weapon, held);
        if self.phase != Phase::Ready {
            return result;
        }
        if self.latch.pump_due && self.next[0] <= 0.0 {
            return self.pump(config, result);
        }
        // The pistol fires as fast as it can be clicked.
        if let Some(click) = config.primary.as_ref().and_then(|fire| fire.click_interval)
            && !held.contains(Buttons::ATTACK)
            && self.since_shot >= click
        {
            self.next[0] = self.next[0].min(0.0);
        }
        for (mode, button) in [
            (Mode::Secondary, Buttons::ATTACK2),
            (Mode::Primary, Buttons::ATTACK),
        ] {
            let Some(fire) = arsenal.fire(weapon, mode) else {
                continue;
            };
            let wanted = held.contains(button) && (fire.automatic || pressed.contains(button));
            if !wanted || self.next[mode as usize] > 0.0 {
                continue;
            }
            return self.pull_trigger(arsenal, weapon, mode, result);
        }
        if !held.contains(Buttons::ATTACK) && !held.contains(Buttons::ATTACK2) {
            self.latch.fire_on_empty = false;
        }
        let nothing_held = !held.contains(Buttons::ATTACK)
            && !held.contains(Buttons::ATTACK2)
            && !held.contains(Buttons::RELOAD);
        if pressed.contains(Buttons::RELOAD) {
            result.reloading = self.start_reload(arsenal);
        } else if nothing_held {
            self.reload_or_switch(arsenal, weapon, &mut result);
        }
        result
    }

    /// The trigger of `mode` is pulled and the mode is ready: fire, prime,
    /// start charging, or click.
    fn pull_trigger(
        &mut self,
        arsenal: &Arsenal,
        weapon: WeaponId,
        mode: Mode,
        mut result: WeaponTick,
    ) -> WeaponTick {
        let Some(fire) = arsenal.fire(weapon, mode) else {
            return result;
        };
        // A launcher that has a rocket out fires nothing until it is gone.
        if self.latch.projectile_out
            && arsenal
                .weapon(weapon)
                .is_some_and(|config| config.reload_after_projectile.is_some())
        {
            return result;
        }
        // An alternate fire that wants more rounds than are left fires the
        // single shot instead (the shotgun's last shell).
        if mode == Mode::Secondary
            && matches!(fire.ammo, FireAmmo::Clip(_))
            && !self.affords(arsenal, weapon, mode)
            && self.affords(arsenal, weapon, Mode::Primary)
            && self.clip(weapon).is_some_and(|clip| clip > 0)
            && self.next[0] <= 0.0
        {
            return self.pull_trigger(arsenal, weapon, Mode::Primary, result);
        }
        if !self.affords(arsenal, weapon, mode) {
            self.fire_empty(arsenal, weapon, mode, &mut result);
            return result;
        }
        self.latch.fire_on_empty = false;
        if fire.on_release {
            self.phase = Phase::Priming(mode);
            result.primed = Some((weapon, mode));
        } else if fire.charge > 0.0 {
            self.phase = Phase::Charging(mode, fire.charge);
            result.noise = Some(Noise::Charge);
        } else {
            self.shoot(arsenal, weapon, mode, &mut result);
        }
        result
    }

    /// The trigger of `mode` pulled with too little ammo for it. HL2: the
    /// first pull clicks; the next tries to reload (or gives way to another
    /// weapon). A mode that draws on carried ammo clicks and waits half a
    /// second.
    fn fire_empty(
        &mut self,
        arsenal: &Arsenal,
        weapon: WeaponId,
        mode: Mode,
        result: &mut WeaponTick,
    ) {
        let from_clip = arsenal
            .fire(weapon, mode)
            .is_some_and(|fire| matches!(fire.ammo, FireAmmo::Clip(_)));
        if from_clip && self.latch.fire_on_empty {
            self.reload_or_switch(arsenal, weapon, result);
            return;
        }
        if self.empty_wait <= 0.0 {
            result.noise = Some(Noise::Dry);
            self.dry_count = self.dry_count.wrapping_add(1);
            self.empty_wait = EMPTY_CLICK_WAIT;
        }
        if from_clip || mode == Mode::Primary {
            self.latch.fire_on_empty = true;
        } else {
            self.next[mode as usize] = EMPTY_CLICK_WAIT;
        }
    }

    /// An empty weapon with a free moment reloads; one with nothing at all
    /// left to fire gives way to the best weapon that has some.
    fn reload_or_switch(&mut self, arsenal: &Arsenal, weapon: WeaponId, result: &mut WeaponTick) {
        if self.phase != Phase::Ready || self.next.iter().any(|&next| next > 0.0) {
            return;
        }
        if self.usable(arsenal, weapon) {
            if self.clip(weapon) == Some(0) && !self.latch.pump_due {
                result.reloading = self.start_reload(arsenal);
            }
        } else if let Some(best) = self.best_with_ammo(arsenal, weapon) {
            result.switched = self.select(arsenal, best);
        }
    }

    /// The weapon HL2 would bring up when `weapon` runs dry: the heaviest
    /// other one that has ammo and may be switched to automatically.
    fn best_with_ammo(&self, arsenal: &Arsenal, weapon: WeaponId) -> Option<WeaponId> {
        self.weapons()
            .filter(|&other| other != weapon && self.usable(arsenal, other))
            .filter_map(|other| Some((other, arsenal.weapon(other)?)))
            .filter(|(_, config)| config.auto_switch_to)
            .max_by_key(|(_, config)| config.weight)
            .map(|(other, _)| other)
    }

    /// Spends the ammo of a shot of `mode` and sets the beats: the mode's
    /// own interval and the other's. Reports the shot in `result`.
    fn shoot(&mut self, arsenal: &Arsenal, weapon: WeaponId, mode: Mode, result: &mut WeaponTick) {
        let (Some(fire), Some(config)) = (arsenal.fire(weapon, mode), arsenal.weapon(weapon))
        else {
            return;
        };
        if !self.spend(arsenal, weapon, mode) {
            return;
        }
        result.penalty = self.penalty;
        let carried = self.next[mode as usize].min(0.0);
        self.next[mode as usize] = carried + fire.interval;
        if fire.other_interval > 0.0 {
            self.next[1 - mode as usize] = carried + fire.other_interval;
        }
        self.since_shot = 0.0;
        if let Shot::Hitscan(bullets) = &fire.shot
            && let Some(accuracy) = &bullets.accuracy
        {
            self.penalty += accuracy.per_shot;
        }
        if config.pump.is_some() {
            self.latch.pump_due = true;
        }
        if let Some(auto) = config.auto_reload
            && self.clip(weapon) == Some(0)
        {
            self.reload_due = Some(auto);
        }
        if config.reload_after_projectile.is_some() {
            self.latch.projectile_out = true;
        }
        result.fired = Some((weapon, mode));
    }

    /// The rocket `weapon` fired is gone (burst, expired, or its owner
    /// died): a launcher in hand with rockets left reloads before it can
    /// fire again.
    pub fn projectile_ended(&mut self, arsenal: &Arsenal, weapon: WeaponId) {
        let Some(reload) = arsenal
            .weapon(weapon)
            .and_then(|config| config.reload_after_projectile)
        else {
            return;
        };
        self.latch.projectile_out = false;
        let rockets_left = arsenal
            .fire(weapon, Mode::Primary)
            .is_some_and(|_| self.usable(arsenal, weapon));
        if self.active == Some(weapon) && self.phase == Phase::Ready && rockets_left {
            self.begin_reload_step(ReloadStep::Whole, reload);
        }
    }

    /// The weapon works its action: nothing fires for a moment.
    fn pump(&mut self, config: &WeaponConfig, mut result: WeaponTick) -> WeaponTick {
        self.latch.pump_due = false;
        if let Some(pump) = config.pump {
            let carried = self.next[0].min(0.0);
            self.next = [carried + pump; 2];
        }
        self.pump_count = self.pump_count.wrapping_add(1);
        result.noise = Some(Noise::Pump);
        result
    }

    /// The pistol's penalty drains while the trigger is up and the weapon
    /// is ready again.
    fn drain_penalty(&mut self, config: &WeaponConfig, held: Buttons, dt: f32) {
        let soonest = config
            .primary
            .as_ref()
            .and_then(|fire| fire.click_interval)
            .unwrap_or(0.0);
        if !held.contains(Buttons::ATTACK) && self.since_shot >= soonest {
            self.penalty = (self.penalty - dt).max(0.0);
        }
    }

    /// A round-by-round reload gives way to a shot: if the trigger is held
    /// and enough rounds are in, the reload ends here (its current step
    /// still has to finish before anything fires).
    fn interrupt_reload(&mut self, arsenal: &Arsenal, weapon: WeaponId, held: Buttons) {
        if !matches!(self.phase, Phase::Reloading(_)) || self.reload == ReloadStep::Whole {
            return;
        }
        let clip = self.clip(weapon).unwrap_or(0);
        let wants = |mode: Mode, button: Buttons| {
            held.contains(button)
                && arsenal
                    .fire(weapon, mode)
                    .is_some_and(|fire| match fire.ammo {
                        FireAmmo::Clip(count) => clip >= count,
                        _ => true,
                    })
        };
        if wants(Mode::Primary, Buttons::ATTACK) || wants(Mode::Secondary, Buttons::ATTACK2) {
            self.phase = Phase::Ready;
            self.reload = ReloadStep::Whole;
        }
    }

    /// Whether the ammo for one shot of `mode` is there.
    fn affords(&self, arsenal: &Arsenal, weapon: WeaponId, mode: Mode) -> bool {
        let Some(fire) = arsenal.fire(weapon, mode) else {
            return false;
        };
        match &fire.ammo {
            FireAmmo::Unlimited => true,
            FireAmmo::Clip(count) => self.clip(weapon).is_some_and(|clip| clip >= *count),
            FireAmmo::Reserve { ammo, count } => arsenal
                .find_ammo(ammo)
                .is_some_and(|ammo| self.ammo(ammo) >= *count),
        }
    }

    /// A primed weapon: while the button of `mode` is held nothing happens;
    /// let go, the shot goes off (after the fire mode's `charge`: the time
    /// the arm takes to let go of it).
    fn release(
        &mut self,
        arsenal: &Arsenal,
        weapon: WeaponId,
        mode: Mode,
        cmd: &UserCmd,
        mut result: WeaponTick,
    ) -> WeaponTick {
        let button = match mode {
            Mode::Primary => Buttons::ATTACK,
            Mode::Secondary => Buttons::ATTACK2,
        };
        if cmd.buttons.contains(button) {
            return result;
        }
        let delay = arsenal.fire(weapon, mode).map_or(0.0, |fire| fire.charge);
        if delay > 0.0 {
            self.phase = Phase::Charging(mode, delay);
            return result;
        }
        self.phase = Phase::Ready;
        self.shoot(arsenal, weapon, mode, &mut result);
        result
    }

    /// Puts away the weapon in hand if it is a
    /// [consumable](rl_data::WeaponConfig::consumable) that has nothing
    /// left to throw and is done throwing, taking up the heaviest weapon
    /// with ammo (see [`WeaponConfig::weight`]). Returns the weapon put away.
    fn retire_spent(&mut self, arsenal: &Arsenal) -> Option<WeaponId> {
        let weapon = self.active?;
        let config = arsenal.weapon(weapon)?;
        if !config.consumable
            || self.phase != Phase::Ready
            || self.next.iter().any(|&next| next > 0.0)
            || self.usable(arsenal, weapon)
        {
            return None;
        }
        self.weapons.retain(|carried| carried.weapon != weapon);
        self.active = None;
        self.last = None;
        if let Some(best) = self.best_with_ammo(arsenal, weapon) {
            self.select(arsenal, best);
        }
        Some(weapon)
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
        if self.phase != Phase::Ready
            || self.latch.pump_due
            || loaded >= clip.size
            || self.ammo(ammo) == 0
        {
            return false;
        }
        let (step, time) = match clip.reload {
            Reload::Whole { time } => (ReloadStep::Whole, time),
            Reload::PerRound { start, .. } => (ReloadStep::Start, start),
        };
        self.begin_reload_step(step, time);
        true
    }

    /// Enters a reload step that takes `time` seconds, which nothing fires
    /// during.
    fn begin_reload_step(&mut self, step: ReloadStep, time: f32) {
        self.reload = step;
        self.reload_count = self.reload_count.wrapping_add(1);
        self.reload_due = None;
        self.phase = if step == ReloadStep::Finish {
            Phase::Ready
        } else {
            Phase::Reloading(time)
        };
        self.next = [time; 2];
    }

    /// Counts down deploying, reloading and charging, loading rounds as
    /// reloads complete and sending off charged shots.
    fn advance_phase(&mut self, arsenal: &Arsenal, dt: f32, result: &mut WeaponTick) {
        match self.phase {
            Phase::Ready | Phase::Priming(_) => {}
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
            Phase::Charging(mode, left) => {
                if left > dt {
                    self.phase = Phase::Charging(mode, left - dt);
                } else if let Some(weapon) = self.active {
                    self.phase = Phase::Ready;
                    self.next[mode as usize] = 0.0;
                    self.shoot(arsenal, weapon, mode, result);
                }
            }
        }
    }

    /// A reload step's timer ran out (`overshoot` seconds ago, ≤ 0): load
    /// the clip, or put one round in and time the next step.
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
        let step = self.reload;
        let Some(carried) = self.carried_mut(weapon) else {
            return;
        };
        let room = clip.size.saturating_sub(carried.clip);
        match clip.reload {
            Reload::Whole { .. } => {
                let loaded = room.min(reserve);
                carried.clip += loaded;
                if let Some(carried) = self.ammo.get_mut(ammo.index()) {
                    *carried -= loaded;
                }
                self.reload = ReloadStep::Whole;
            }
            Reload::PerRound { each, finish, .. } => {
                if step == ReloadStep::Finish {
                    self.reload = ReloadStep::Whole;
                } else if room > 0 && reserve > 0 {
                    carried.clip += 1;
                    if let Some(carried) = self.ammo.get_mut(ammo.index()) {
                        *carried -= 1;
                    }
                    self.begin_reload_step(ReloadStep::Round, (each + overshoot).max(0.0));
                } else {
                    self.begin_reload_step(ReloadStep::Finish, (finish + overshoot).max(0.0));
                }
            }
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
