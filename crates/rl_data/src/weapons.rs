//! `weapons/<id>.ron`: what players carry and shoot with.
//!
//! A weapon is composed from data rather than coded: a clip (or none), up to
//! two fire modes, and for each mode the kind of shot it makes — bullets
//! ([`HitscanConfig`]), a swing ([`MeleeConfig`]), a projectile
//! ([`ProjectileConfig`]) or the Manipulator's grab and launch
//! ([`Shot::Manipulator`], tuned in `tools/manipulator.ron`). A new weapon is
//! a new file; its id is the file name without `.ron`.
//!
//! The code that runs weapons is `rl_sim::weapons`; `docs/data-driven.md`
//! walks through adding one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Highest weapon slot (the number key that selects it).
pub const MAX_SLOT: u8 = 9;

/// What a weapon's sounds are for:
///
/// - `fire`, `fire2`: the primary and secondary fire modes going off (a
///   swing, a throw, a shot);
/// - `hit`: a swing striking something;
/// - `reload`: a reload beginning;
/// - `deploy`: the weapon coming up;
/// - `pickup`, `drop`, `launch`, `punt`, `dryfire`: the Manipulator
///   picking a prop up, letting it go, throwing it, knocking one away, or
///   finding nothing to throw;
/// - `blip`: a thrown projectile's fuse ticking (see [`BlipConfig`]);
/// - `empty`: the trigger pulled on an empty clip;
/// - `pump`: the shotgun working its action;
/// - `charge`: a delayed shot ([`FireConfig::charge`]) beginning to build.
pub const WEAPON_SOUNDS: [&str; 14] = [
    "fire", "fire2", "hit", "reload", "deploy", "pickup", "drop", "launch", "punt", "dryfire",
    "blip", "empty", "pump", "charge",
];

/// For serde defaults.
fn yes() -> bool {
    true
}

/// One weapon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Weapon", deny_unknown_fields)]
pub struct WeaponConfig {
    /// Name shown on the HUD and in the kill feed.
    pub name: String,
    /// The number key that selects it, `1..=9`.
    pub slot: u8,
    /// Order among the weapons of the same slot, lowest first.
    pub position: u8,
    /// Model drawn in the player's hands, first person, if any.
    pub view_model: Option<String>,
    /// Model drawn for pickups and in other players' hands.
    pub world_model: String,
    /// The weapon's magazine, if it has one. Fire modes that use
    /// [`FireAmmo::Clip`] take rounds from it.
    pub clip: Option<ClipConfig>,
    /// What primary fire does.
    pub primary: Option<FireConfig>,
    /// What secondary fire does.
    pub secondary: Option<FireConfig>,
    /// Seconds after switching to the weapon before it can fire.
    pub deploy_time: f32,
    /// How the weapon is picked up from the map, if it can be.
    pub pickup: Option<WeaponPickupConfig>,
    /// Soundscript names (or files under `sound/`) by purpose, played by
    /// the client; see [`WEAPON_SOUNDS`] for the purposes.
    pub sounds: BTreeMap<String, String>,
    /// How players hold it: the suffix of their model's sequences while it
    /// is in hand (`smg1` for `run_smg1`); `player.ron`'s default if none.
    #[serde(default)]
    pub hold: Option<String>,
    /// The weapon is itself what it throws, like a grenade: it is carried
    /// only while rounds of its ammo are, and is put away for good with the
    /// last one.
    #[serde(default)]
    pub consumable: bool,
    /// Seconds after a shot that empties the clip before it is reloaded
    /// whatever the buttons do (the crossbow, which reloads once its firing
    /// animation is over); by default an empty clip reloads when the weapon
    /// next gets the chance.
    #[serde(default)]
    pub auto_reload: Option<f32>,
    /// Seconds the weapon spends working its action after a shot, once the
    /// shot's own interval is over, before anything fires again (the
    /// shotgun's pump).
    #[serde(default)]
    pub pump: Option<f32>,
    /// How HL2 ranks the weapon when one has to be chosen for the player:
    /// the heaviest weapon that has ammo is brought up when the weapon in
    /// hand has none left.
    #[serde(default)]
    pub weight: i32,
    /// Whether this weapon may be brought up by that automatic choice (the
    /// Gravity Gun never is).
    #[serde(default = "yes")]
    pub auto_switch_to: bool,
    /// Whether picking up a heavier weapon switches to it while this one is
    /// in hand (the Gravity Gun never does).
    #[serde(default = "yes")]
    pub auto_switch_from: bool,
    /// A zoom the alternate button toggles, if the weapon has one.
    #[serde(default)]
    pub zoom: Option<ZoomConfig>,
    /// The weapon holds one rocket in the air at a time (HL2's RPG): while
    /// the last one flies nothing fires, and once it is gone the launcher
    /// reloads for this many seconds before it can fire again.
    #[serde(default)]
    pub reload_after_projectile: Option<f32>,
    /// The alternate button toggles a laser (which steers the weapon's
    /// [missiles](MissileConfig)), on at first.
    #[serde(default)]
    pub laser: bool,
    /// The glyph of the weapon's icon in the HUD's icon font (see
    /// [`HudConfig::weapon_icon_font`](crate::HudConfig::weapon_icon_font));
    /// the weapon list shows only its name without one.
    #[serde(default)]
    pub icon: Option<char>,
}

/// A scope: the field of view narrows while the alternate button's toggle is
/// on. It ends when the weapon is put away or the player dies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZoomConfig {
    /// Vertical-equivalent field of view while zoomed, degrees (HL2's FOV is
    /// horizontal on a 4:3 screen; the client converts).
    pub fov: f32,
    /// Seconds to zoom in.
    pub in_time: f32,
    /// Seconds to zoom out.
    pub out_time: f32,
}

/// A weapon's magazine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipConfig {
    /// The ammo type loaded into it (a key of `ammo.ron`).
    pub ammo: String,
    /// Rounds it holds.
    pub size: u32,
    /// How it is refilled from the carried ammo.
    pub reload: Reload,
}

/// How a clip is refilled.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Reload {
    /// The whole clip at once after `time` seconds.
    Whole {
        /// Seconds to reload.
        time: f32,
    },
    /// One round at a time, as HL2's shotgun does: a `start` animation, then
    /// a round goes in at the beginning of every `each`-second step, then a
    /// `finish` animation once the clip is full or the ammo has run out.
    /// Firing during a step ends the reload as soon as that step is over,
    /// skipping the finish.
    PerRound {
        /// Seconds the reload takes to begin, before the first round goes in.
        start: f32,
        /// Seconds per round.
        each: f32,
        /// Seconds the closing animation takes; nothing fires during it.
        finish: f32,
    },
}

/// One fire mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FireConfig {
    /// Seconds before this fire mode can go off again (or, for the
    /// Manipulator, between actions). A mode that is held fires on this
    /// beat, exactly: the remainder of a tick carries over, so a gun
    /// firing faster than the tick fires more than one round in some
    /// ticks.
    pub interval: f32,
    /// Seconds before the *other* fire mode can go off after this one has
    /// (HL2's weapons set both attack times; 0 leaves the other alone).
    #[serde(default)]
    pub other_interval: f32,
    /// A fresh press may fire this soon after the last shot, though a held
    /// button waits for the whole `interval`: HL2's pistol, which fires as
    /// fast as it can be clicked.
    #[serde(default)]
    pub click_interval: Option<f32>,
    /// Seconds between the press and the shot going off, during which the
    /// weapon is busy (the Pulse Rifle's energy ball charges first).
    #[serde(default)]
    pub charge: f32,
    /// Keeps firing while the button is held; otherwise every shot needs a
    /// fresh press.
    pub automatic: bool,
    /// What a shot uses up.
    pub ammo: FireAmmo,
    /// What a shot is.
    pub shot: Shot,
    /// The shot goes off when the button is let go rather than pressed: the
    /// weapon is *primed* while it is held (a grenade's pin pulled, the arm
    /// drawn back) and nothing is used up until it is released.
    #[serde(default)]
    pub on_release: bool,
}

/// What a shot uses up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FireAmmo {
    /// Nothing: the mode never runs dry.
    Unlimited,
    /// This many rounds from the weapon's clip; the weapon needs a
    /// [`clip`](WeaponConfig::clip).
    Clip(u32),
    /// This many rounds straight from the carried ammo of a type.
    Reserve {
        /// The ammo type (a key of `ammo.ron`).
        ammo: String,
        /// Rounds per shot.
        count: u32,
    },
}

/// What a shot is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Shot {
    /// Instant bullets along the view.
    Hitscan(HitscanConfig),
    /// A short-range swing.
    Melee(MeleeConfig),
    /// A moving projectile that may explode.
    Projectile(ProjectileConfig),
    /// The Manipulator: primary fire launches or punts, secondary grabs or
    /// drops (`tools/manipulator.ron`).
    Manipulator,
}

/// Bullets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitscanConfig {
    /// Bullets per shot, each with its own spread.
    pub pellets: u32,
    /// Damage per bullet. Bullets lose none over distance, as in HL2.
    pub damage: f32,
    /// How far a bullet strays, as Source gives it: a vector cone whose
    /// value is the sine of half the cone's angle (`0.0436` is the
    /// engine's `VECTOR_CONE_5DEGREES`). Each bullet is turned sideways and
    /// upwards by a draw in `-cone..=cone` per axis, triangularly
    /// distributed (the sum of two uniform draws, so aimed shots cluster
    /// towards the middle).
    pub cone: f32,
    /// A cone that widens with firing, instead of `cone`, if set.
    #[serde(default)]
    pub accuracy: Option<AccuracyConfig>,
    /// Farthest a bullet reaches, units.
    pub range: f32,
    /// Impulse a bullet gives the prop it hits, kg·units/s.
    pub force: f32,
}

/// A cone that opens as shots are fired and closes as the trigger rests:
/// HL2's pistol. Each shot adds `per_shot` seconds of penalty (up to
/// `full` of them); the penalty drains at one second per second while the
/// button is up and the weapon is ready again; the cone is `min` with no
/// penalty and `max` with `full` seconds of it, in between linearly.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccuracyConfig {
    /// The cone of a rested weapon.
    pub min: f32,
    /// The cone at full penalty.
    pub max: f32,
    /// Seconds of penalty each shot adds.
    pub per_shot: f32,
    /// Seconds of penalty at which the cone is widest.
    pub full: f32,
}

impl AccuracyConfig {
    /// The cone at `penalty` seconds of penalty.
    #[must_use]
    pub fn cone(&self, penalty: f32) -> f32 {
        let widening = if self.full > 0.0 {
            (penalty / self.full).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.min + (self.max - self.min) * widening
    }
}

/// A swing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeleeConfig {
    /// Reach from the eyes, units.
    pub range: f32,
    /// Half the width of the box swept along the view when the line along it
    /// misses, units: a swing
    /// connects with things slightly off the crosshair.
    pub size: f32,
    /// Damage dealt.
    pub damage: f32,
    /// Impulse given to a prop hit, kg·units/s.
    pub force: f32,
}

/// A projectile: a small box flying from the eyes along the view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectileConfig {
    /// Model drawn for it.
    pub model: String,
    /// Launch speed, units/s.
    pub speed: f32,
    /// Extra launch speed upwards, units/s, for lobbed throws.
    pub lift: f32,
    /// Fraction of the world's gravity it falls under (0 flies straight).
    pub gravity: f32,
    /// Half the width of its collision box, units.
    pub size: f32,
    /// Bounces off what it hits, keeping this fraction of its speed;
    /// `None` stops (and explodes or sticks) at the first hit.
    pub bounce: Option<f32>,
    /// Explodes this many seconds after launch, if set; a projectile that
    /// neither bounces nor has a fuse explodes on impact.
    pub fuse: Option<f32>,
    /// Damage to a player hit directly, besides any explosion.
    pub hit_damage: f32,
    /// The blast when it goes off, if it explodes.
    pub explosion: Option<ExplosionConfig>,
    /// Seconds before it disappears if nothing else ends it.
    pub lifetime: f32,
    /// Whoever it kills dissolves (the Pulse Rifle's energy ball).
    #[serde(default)]
    pub dissolves: bool,
    /// A rocket that is steered by its owner's laser, if it is one.
    #[serde(default)]
    pub missile: Option<MissileConfig>,
    /// What a shot made while the shooter is crouching is instead (a
    /// grenade rolled along the floor, where standing it is lobbed).
    #[serde(default)]
    pub crouched: Option<Box<ProjectileConfig>>,
    /// Simulated as a rigid body like any prop — it tumbles, rolls, bounces
    /// off players and can be shoved by blasts — instead of flying as a
    /// small box. Grenades are.
    #[serde(default)]
    pub body: Option<ProjectileBodyConfig>,
}

/// A rocket in the manner of HL2's RPG: it leaves the launcher slowly,
/// lobbed a little upwards, lights its motor after `ignite_delay` seconds
/// and then flies at the projectile's `speed`, bending towards the laser
/// dot of whoever fired it, if their launcher is in hand and its laser on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissileConfig {
    /// Seconds the motor takes to light.
    pub ignite_delay: f32,
    /// Speed forwards before the motor lights, units/s...
    pub launch_forward: f32,
    /// ...and upwards.
    pub launch_up: f32,
    /// How far the heading turns towards the laser dot at each steering
    /// step: the fraction of the way.
    pub steer: f32,
    /// Seconds between steering steps.
    pub steer_interval: f32,
    /// Where it leaves the launcher relative to the eyes: forwards, right
    /// and up, units.
    pub muzzle: [f32; 3],
}

/// A projectile that is a physics body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectileBodyConfig {
    /// Radius of its collision cylinder, units. The cylinder stands along
    /// the model's own Z axis.
    pub radius: f32,
    /// Height of its collision cylinder, units.
    pub height: f32,
    /// The point of the model, in its own frame (Source units), that sits at
    /// the body's centre of mass: where the model's middle is, so that it
    /// is drawn where it collides.
    pub model_center: [f32; 3],
    /// The surface property it is made of (a key of the game's
    /// `surfaceproperties.txt`), which names the sounds of its impacts.
    pub surface: String,
    /// It leaves the hand at the feet, flat along the view, and rolls on its
    /// side instead of tumbling: a grenade rolled along the floor.
    #[serde(default)]
    pub rolled: bool,
    /// The fuse's ticking, if it ticks.
    #[serde(default)]
    pub blip: Option<BlipConfig>,
    /// Mass, kg.
    pub mass: f32,
    /// Friction coefficient of its surface.
    pub friction: f32,
    /// Spin it leaves the hand with, radians/s.
    pub spin: f32,
    /// Fraction of its speed it loses per second to the air.
    pub damping: f32,
}

/// A fuse's ticking: the sound and the red flash of a grenade, in step.
///
/// It blips as it leaves the hand, then every `interval` seconds, and every
/// `fast_interval` once no more than `warn_time` seconds of the fuse are
/// left. The schedule is a function of the grenade's age alone, so the sound
/// and the flash agree on every machine ([`BlipConfig::last`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlipConfig {
    /// Seconds between blips at first.
    pub interval: f32,
    /// Seconds between blips once the end is near.
    pub fast_interval: f32,
    /// How many seconds before the fuse runs out the blips quicken.
    pub warn_time: f32,
}

impl BlipConfig {
    /// The latest blip of a grenade `age` seconds old with a `fuse` seconds
    /// long: its number (the first is 0) and when it happened.
    #[must_use]
    pub fn last(&self, age: f32, fuse: f32) -> (u32, f32) {
        let (mut number, mut at) = (0, 0.0);
        loop {
            let step = if fuse - at <= self.warn_time {
                self.fast_interval
            } else {
                self.interval
            };
            if step <= 0.0 || at + step > age {
                return (number, at);
            }
            number += 1;
            at += step;
        }
    }
}

/// A blast: damage and push falling off linearly from its centre.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplosionConfig {
    /// Reach of the blast, units.
    pub radius: f32,
    /// Damage at the centre.
    pub damage: f32,
    /// Speed given to a player at the centre, units/s, away from it.
    pub knockback: f32,
    /// The most speed a player is given, units/s, wherever they are.
    /// HL2 pushes with a force of 4 units/s per point of damage that stops
    /// growing at 100: a blast pushes about as hard at 100 damage as at
    /// 150.
    #[serde(default)]
    pub knockback_cap: Option<f32>,
    /// Impulse given to a prop at the centre, kg·units/s.
    pub force: f32,
}

impl ExplosionConfig {
    /// The speed a player is pushed away at where the blast has `scale`
    /// strength: `knockback` at full strength, falling with it, but never
    /// more than `knockback_cap`.
    #[must_use]
    pub fn push_speed(&self, scale: f32) -> f32 {
        let speed = self.knockback * scale;
        self.knockback_cap.map_or(speed, |cap| speed.min(cap))
    }

    /// How strongly the blast reaches `distance` units from its centre:
    /// 1 at the centre, falling linearly to 0 at the radius.
    #[must_use]
    pub fn scale(&self, distance: f32) -> f32 {
        if self.radius <= 0.0 {
            return 0.0;
        }
        (1.0 - distance / self.radius).clamp(0.0, 1.0)
    }
}

/// Picking the weapon up from the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponPickupConfig {
    /// Carried ammo of the weapon's [main type](WeaponConfig::ammo_type)
    /// given with a weapon the player did not have (it also comes with a
    /// full clip).
    pub ammo: u32,
    /// The ammo given instead to a player who already has the weapon: HL2
    /// takes the picked-up weapon's clip off it, its `default_clip`.
    #[serde(default)]
    pub owned_ammo: u32,
    /// Seconds before the pickup comes back.
    pub respawn: f32,
    /// A player touches it when it is within this distance of their box,
    /// units.
    pub radius: f32,
}

impl WeaponConfig {
    /// The weapon's main ammo type: its clip's, else the first type a fire
    /// mode takes straight from the carried ammo.
    #[must_use]
    pub fn ammo_type(&self) -> Option<&str> {
        if let Some(clip) = &self.clip {
            return Some(&clip.ammo);
        }
        [&self.primary, &self.secondary]
            .into_iter()
            .flatten()
            .find_map(|fire| match &fire.ammo {
                FireAmmo::Reserve { ammo, .. } => Some(ammo.as_str()),
                _ => None,
            })
    }

    /// Every fire mode the weapon has, primary first.
    pub fn fire_modes(&self) -> impl Iterator<Item = &FireConfig> {
        [&self.primary, &self.secondary].into_iter().flatten()
    }

    /// Every ammo type the weapon refers to.
    pub fn ammo_types(&self) -> impl Iterator<Item = &str> {
        let clip = self.clip.as_ref().map(|clip| clip.ammo.as_str());
        let reserve = self.fire_modes().filter_map(|fire| match &fire.ammo {
            FireAmmo::Reserve { ammo, .. } => Some(ammo.as_str()),
            _ => None,
        });
        clip.into_iter().chain(reserve)
    }

    /// Checks every value for sense on its own; references to other files
    /// are checked by [`crate::GameData`].
    pub(crate) fn validate(&self, check: &mut Validator) {
        if self.name.trim().is_empty() {
            check.fail("name", "must not be empty");
        }
        if !(1..=MAX_SLOT).contains(&self.slot) {
            check.fail(
                "slot",
                &format!("must be within 1..={MAX_SLOT}, got {}", self.slot),
            );
        }
        if self.world_model.trim().is_empty() {
            check.fail("world_model", "must name a model");
        }
        check.non_negative("deploy_time", self.deploy_time);
        if let Some(auto) = self.auto_reload {
            check.non_negative("auto_reload", auto);
        }
        if let Some(pump) = self.pump {
            check.non_negative("pump", pump);
        }
        if let Some(reload) = self.reload_after_projectile {
            check.positive("reload_after_projectile", reload);
        }
        if let Some(zoom) = &self.zoom {
            check.range("zoom.fov", zoom.fov, 1.0, 179.0);
            check.non_negative("zoom.in_time", zoom.in_time);
            check.non_negative("zoom.out_time", zoom.out_time);
        }
        if let Some(clip) = &self.clip {
            if clip.size == 0 {
                check.fail("clip.size", "must hold at least one round");
            }
            match clip.reload {
                Reload::Whole { time } => check.non_negative("clip.reload.time", time),
                Reload::PerRound {
                    start,
                    each,
                    finish,
                } => {
                    check.non_negative("clip.reload.start", start);
                    check.positive("clip.reload.each", each);
                    check.non_negative("clip.reload.finish", finish);
                }
            }
        }
        if self.primary.is_none() && self.secondary.is_none() {
            check.fail("primary", "a weapon needs at least one fire mode");
        }
        if self.consumable && (self.clip.is_some() || self.ammo_type().is_none()) {
            check.fail(
                "consumable",
                "a weapon that is its own ammo has no clip and fires from carried ammo",
            );
        }
        for (field, fire) in [("primary", &self.primary), ("secondary", &self.secondary)] {
            if let Some(fire) = fire {
                self.validate_fire(check, field, fire);
            }
        }
        if let Some(pickup) = &self.pickup {
            check.non_negative("pickup.respawn", pickup.respawn);
            check.non_negative("pickup.radius", pickup.radius);
        }
        for purpose in self.sounds.keys() {
            if !WEAPON_SOUNDS.contains(&purpose.as_str()) {
                check.fail(
                    &format!("sounds.{purpose}"),
                    &format!("unknown purpose; known: {}", WEAPON_SOUNDS.join(", ")),
                );
            }
        }
    }

    fn validate_fire(&self, check: &mut Validator, field: &str, fire: &FireConfig) {
        check.positive(&format!("{field}.interval"), fire.interval);
        check.non_negative(&format!("{field}.other_interval"), fire.other_interval);
        check.non_negative(&format!("{field}.charge"), fire.charge);
        if let Some(click) = fire.click_interval {
            check.positive(&format!("{field}.click_interval"), click);
        }
        match &fire.ammo {
            FireAmmo::Unlimited => {}
            FireAmmo::Clip(count) => {
                if self.clip.is_none() {
                    check.fail(
                        &format!("{field}.ammo"),
                        "takes from a clip the weapon lacks",
                    );
                }
                if *count == 0 {
                    check.fail(&format!("{field}.ammo"), "must use at least one round");
                }
                if self.clip.as_ref().is_some_and(|clip| *count > clip.size) {
                    check.fail(
                        &format!("{field}.ammo"),
                        "uses more rounds than the clip holds",
                    );
                }
            }
            FireAmmo::Reserve { count, .. } => {
                if *count == 0 {
                    check.fail(&format!("{field}.ammo"), "must use at least one round");
                }
            }
        }
        let shot = format!("{field}.shot");
        match &fire.shot {
            Shot::Hitscan(hitscan) => validate_hitscan(check, &shot, hitscan),
            Shot::Melee(melee) => {
                check.positive(&format!("{shot}.range"), melee.range);
                check.non_negative(&format!("{shot}.size"), melee.size);
                check.non_negative(&format!("{shot}.damage"), melee.damage);
                check.non_negative(&format!("{shot}.force"), melee.force);
            }
            Shot::Projectile(projectile) => validate_projectile(check, &shot, projectile),
            Shot::Manipulator => {}
        }
    }
}

fn validate_hitscan(check: &mut Validator, field: &str, hitscan: &HitscanConfig) {
    if hitscan.pellets == 0 {
        check.fail(&format!("{field}.pellets"), "must fire at least one bullet");
    }
    check.non_negative(&format!("{field}.damage"), hitscan.damage);
    check.range(&format!("{field}.cone"), hitscan.cone, 0.0, 1.0);
    check.positive(&format!("{field}.range"), hitscan.range);
    check.non_negative(&format!("{field}.force"), hitscan.force);
    if let Some(accuracy) = &hitscan.accuracy {
        check.range(&format!("{field}.accuracy.min"), accuracy.min, 0.0, 1.0);
        check.range(&format!("{field}.accuracy.max"), accuracy.max, 0.0, 1.0);
        check.non_negative(&format!("{field}.accuracy.per_shot"), accuracy.per_shot);
        check.positive(&format!("{field}.accuracy.full"), accuracy.full);
    }
}

fn validate_projectile(check: &mut Validator, field: &str, projectile: &ProjectileConfig) {
    if projectile.model.trim().is_empty() {
        check.fail(&format!("{field}.model"), "must name a model");
    }
    check.positive(&format!("{field}.speed"), projectile.speed);
    check.non_negative(&format!("{field}.lift"), projectile.lift);
    check.non_negative(&format!("{field}.gravity"), projectile.gravity);
    check.non_negative(&format!("{field}.size"), projectile.size);
    if let Some(bounce) = projectile.bounce {
        check.range(&format!("{field}.bounce"), bounce, 0.0, 1.0);
    }
    if let Some(fuse) = projectile.fuse {
        check.positive(&format!("{field}.fuse"), fuse);
    }
    check.non_negative(&format!("{field}.hit_damage"), projectile.hit_damage);
    check.positive(&format!("{field}.lifetime"), projectile.lifetime);
    if let Some(body) = &projectile.body {
        check.positive(&format!("{field}.body.radius"), body.radius);
        check.positive(&format!("{field}.body.height"), body.height);
        if body.surface.trim().is_empty() {
            check.fail(&format!("{field}.body.surface"), "must name a surface");
        }
        if let Some(blip) = &body.blip {
            check.positive(&format!("{field}.body.blip.interval"), blip.interval);
            check.positive(
                &format!("{field}.body.blip.fast_interval"),
                blip.fast_interval,
            );
            check.non_negative(&format!("{field}.body.blip.warn_time"), blip.warn_time);
        }
        check.positive(&format!("{field}.body.mass"), body.mass);
        check.non_negative(&format!("{field}.body.friction"), body.friction);
        check.non_negative(&format!("{field}.body.spin"), body.spin);
        check.non_negative(&format!("{field}.body.damping"), body.damping);
        if projectile.bounce.is_none() {
            check.fail(
                &format!("{field}.body"),
                "a rigid body needs a bounce (its restitution)",
            );
        }
    }
    if let Some(missile) = &projectile.missile {
        let field = format!("{field}.missile");
        check.non_negative(&format!("{field}.ignite_delay"), missile.ignite_delay);
        check.non_negative(&format!("{field}.launch_forward"), missile.launch_forward);
        check.range(&format!("{field}.steer"), missile.steer, 0.0, 1.0);
        check.positive(&format!("{field}.steer_interval"), missile.steer_interval);
    }
    if let Some(crouched) = &projectile.crouched {
        validate_projectile(check, &format!("{field}.crouched"), crouched);
    }
    if let Some(explosion) = &projectile.explosion {
        check.positive(&format!("{field}.explosion.radius"), explosion.radius);
        check.non_negative(&format!("{field}.explosion.damage"), explosion.damage);
        check.non_negative(&format!("{field}.explosion.knockback"), explosion.knockback);
        check.non_negative(&format!("{field}.explosion.force"), explosion.force);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pistol() -> WeaponConfig {
        WeaponConfig {
            name: "Test".to_owned(),
            slot: 2,
            position: 0,
            view_model: None,
            world_model: "models/w.mdl".to_owned(),
            clip: Some(ClipConfig {
                ammo: "pistol".to_owned(),
                size: 12,
                reload: Reload::Whole { time: 1.0 },
            }),
            primary: Some(FireConfig {
                interval: 0.2,
                automatic: false,
                ammo: FireAmmo::Clip(1),
                shot: Shot::Hitscan(HitscanConfig {
                    pellets: 1,
                    damage: 10.0,
                    cone: 0.01,
                    accuracy: None,
                    range: 4096.0,
                    force: 100.0,
                }),
                other_interval: 0.0,
                click_interval: None,
                charge: 0.0,
                on_release: false,
            }),
            secondary: None,
            deploy_time: 0.5,
            pickup: None,
            sounds: BTreeMap::new(),
            hold: None,
            consumable: false,
            auto_reload: None,
            pump: None,
            reload_after_projectile: None,
            laser: false,
            weight: 0,
            auto_switch_to: true,
            auto_switch_from: true,
            zoom: None,
            icon: None,
        }
    }

    fn problems(weapon: &WeaponConfig) -> Vec<String> {
        let mut check = Validator::default();
        weapon.validate(&mut check);
        check.finish().into_iter().map(|p| p.field).collect()
    }

    #[test]
    fn a_sensible_weapon_passes() {
        assert!(problems(&pistol()).is_empty());
        assert_eq!(pistol().ammo_type(), Some("pistol"));
    }

    #[test]
    fn clip_fire_needs_a_clip_that_holds_enough() {
        let mut weapon = pistol();
        weapon.clip = None;
        assert_eq!(problems(&weapon), ["primary.ammo"]);
        let mut weapon = pistol();
        if let Some(fire) = &mut weapon.primary {
            fire.ammo = FireAmmo::Clip(13);
        }
        assert_eq!(problems(&weapon), ["primary.ammo"]);
    }

    #[test]
    fn nonsense_values_are_reported_by_field() {
        let mut weapon = pistol();
        weapon.slot = 0;
        if let Some(Shot::Hitscan(hitscan)) = weapon.primary.as_mut().map(|fire| &mut fire.shot) {
            hitscan.pellets = 0;
            hitscan.cone = 2.0;
        }
        assert_eq!(
            problems(&weapon),
            ["slot", "primary.shot.pellets", "primary.shot.cone"]
        );
    }

    #[test]
    fn a_weapon_without_fire_modes_is_rejected() {
        let mut weapon = pistol();
        weapon.primary = None;
        assert_eq!(problems(&weapon), ["primary"]);
    }

    #[test]
    fn accuracy_widens_with_penalty_up_to_its_limit() {
        let accuracy = AccuracyConfig {
            min: 0.01,
            max: 0.05,
            per_shot: 0.2,
            full: 1.5,
        };
        assert_eq!(accuracy.cone(0.0), 0.01);
        assert!((accuracy.cone(0.75) - 0.03).abs() < 1e-6);
        assert!((accuracy.cone(1.5) - 0.05).abs() < 1e-6);
        assert!((accuracy.cone(9.0) - 0.05).abs() < 1e-6, "capped");
        assert_eq!(accuracy.cone(-1.0), 0.01, "never negative");
    }

    #[test]
    fn a_consumable_weapon_fires_from_carried_ammo_alone() {
        let mut weapon = pistol();
        weapon.consumable = true;
        assert_eq!(problems(&weapon), ["consumable"], "it has a clip");
        weapon.clip = None;
        if let Some(fire) = &mut weapon.primary {
            fire.ammo = FireAmmo::Reserve {
                ammo: "frag".to_owned(),
                count: 1,
            };
        }
        assert!(problems(&weapon).is_empty());
    }

    #[test]
    fn fuses_tick_slowly_then_quicken_near_the_end() {
        let blip = BlipConfig {
            interval: 1.0,
            fast_interval: 0.3,
            warn_time: 1.5,
        };
        // A 2.5 s fuse: at 0 and 1 s, then every 0.3 s.
        assert_eq!(blip.last(0.0, 2.5), (0, 0.0));
        assert_eq!(blip.last(0.99, 2.5), (0, 0.0));
        assert_eq!(blip.last(1.0, 2.5), (1, 1.0));
        assert_eq!(blip.last(1.29, 2.5), (1, 1.0));
        let (number, at) = blip.last(1.65, 2.5);
        assert_eq!(number, 3);
        assert!((at - 1.6).abs() < 1e-5, "{at}");
        let (number, _) = blip.last(2.5, 2.5);
        assert_eq!(number, 6);
    }

    #[test]
    fn pushes_stop_growing_at_the_cap() {
        let blast = ExplosionConfig {
            radius: 300.0,
            damage: 150.0,
            knockback: 529.0,
            knockback_cap: Some(353.0),
            force: 0.0,
        };
        assert_eq!(blast.push_speed(1.0), 353.0);
        assert_eq!(blast.push_speed(0.7), 353.0, "still at the cap");
        assert!((blast.push_speed(0.5) - 264.5).abs() < 1e-3);
        assert_eq!(blast.push_speed(0.0), 0.0);
        let uncapped = ExplosionConfig {
            knockback_cap: None,
            ..blast
        };
        assert!((uncapped.push_speed(1.0) - 529.0).abs() < 1e-3);
    }

    #[test]
    fn blasts_fade_to_nothing_at_their_radius() {
        let blast = ExplosionConfig {
            radius: 200.0,
            damage: 100.0,
            knockback: 0.0,
            knockback_cap: None,
            force: 0.0,
        };
        assert_eq!(blast.scale(0.0), 1.0);
        assert_eq!(blast.scale(50.0), 0.75);
        assert_eq!(blast.scale(250.0), 0.0);
    }
}
