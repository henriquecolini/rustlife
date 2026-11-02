//! `physics.ron`: how props are simulated, pushed and how hard they hit.
//!
//! The physics world itself (rigid bodies, contacts, the solver) is
//! `rl_sim::physics`. This file holds everything about it worth tuning: the
//! world's gravity and limits, the surface materials props are made of, how
//! players push props around and how much a prop hurts when it hits someone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// Every physics parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Physics", deny_unknown_fields)]
pub struct PhysicsConfig {
    /// Downward acceleration of props, units/s².
    pub gravity: f32,
    /// Fastest a prop may move, units/s.
    pub max_speed: f32,
    /// Fastest a prop may spin, degrees/s.
    pub max_angular_speed: f32,
    /// `Box3D` solver sub-steps per tick: more keeps fast hits and stacks
    /// from sinking props into each other, at a higher cost.
    pub ccd_substeps: u32,
    /// A prop moving faster than this, units/s, is swept against the other
    /// props along its path (it is a "bullet"); slower ones are not, which
    /// keeps resting piles cheap.
    pub ccd_min_speed: f32,
    /// Fastest speed at which overlapping props are pushed apart, units/s.
    pub max_corrective_speed: f32,
    /// What props are made of.
    pub materials: MaterialsConfig,
    /// How players push props.
    pub push: PushConfig,
    /// Damage dealt to players by props hitting them.
    pub impact_damage: ImpactDamageConfig,
    /// How props break and blow up.
    pub breakables: BreakablesConfig,
}

/// How props break: from blows, blasts and hard impacts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreakablesConfig {
    /// Speed given to a player at the centre of an exploding prop's blast,
    /// units/s.
    pub explosion_knockback: f32,
    /// Impulse given to props at the centre of such a blast, kg·units/s.
    pub explosion_force: f32,
    /// How many times tougher than its `prop_data` health a prop is to
    /// weapons: bullets, clubs and blasts do this much less to it. Source's
    /// values are low for rustlife's damage scale; this makes bottles, boxes
    /// and sinks take a few solid hits instead of one. Impacts are measured
    /// against the plain health, which is what their tables are made for.
    pub weapon_toughness: f32,
    /// Props striking each other or the map, which make noise and may break
    /// the prop.
    pub impact: PropImpactConfig,
    /// How explosive barrels are set off.
    pub barrels: BarrelConfig,
}

/// How an explosive prop is set off. Bullets and knocks do not break it:
/// the first bullet only primes it, the second sets it on fire, a bullet
/// into a burning one sets it off at once, a nearby blast sets it off or
/// on fire, and one thrown with the gravity gun goes off on whatever it
/// touches first.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarrelConfig {
    /// Seconds a barrel burns, set on fire by a bullet, before it explodes.
    pub burn_seconds: f32,
    /// A blast's damage, at the barrel, that sets it off at once; weaker
    /// ones set it on fire.
    pub instant_damage: f32,
    /// Fuse, seconds, of a barrel set on fire by a blast of next to no
    /// damage...
    pub longest_fuse: f32,
    /// ...and of one set on fire by a blast just short of `instant_damage`.
    pub shortest_fuse: f32,
    /// Seconds a barrel stays dangerous after being thrown or punted by
    /// the gravity gun, if it touches nothing.
    pub launched_seconds: f32,
}

impl BarrelConfig {
    /// The fuse, seconds, of a barrel that a blast of `damage` (less than
    /// `instant_damage`) sets on fire: the harder the blast, the shorter.
    #[must_use]
    pub fn blast_fuse(&self, damage: f32) -> f32 {
        let hardness = (damage / self.instant_damage).clamp(0.0, 1.0);
        self.longest_fuse + (self.shortest_fuse - self.longest_fuse) * hardness
    }
}

/// Props striking props and the map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropImpactConfig {
    /// Slower closing speeds than this, units/s, are not heard, and do not
    /// set off a launched barrel. They may still break what is fragile.
    pub report_speed: f32,
    /// A prop sliding (not rolling) faster than this, units/s, scrapes.
    pub scrape_speed: f32,
    /// A second hit between the same two surfaces this soon after the last,
    /// seconds, is rubbing or a settling bounce: it does no harm.
    pub repeat_seconds: f32,
    /// What impacts do to a prop, by the name its `prop_data` gives as
    /// `damage_table`. A prop naming none, or an unknown one, uses
    /// [`DEFAULT_DAMAGE_TABLE`].
    pub tables: BTreeMap<String, DamageTable>,
}

/// The table of props that name none, as in Source.
pub const DEFAULT_DAMAGE_TABLE: &str = "npc";

impl PropImpactConfig {
    /// The table called `name` (case is ignored), else the default one.
    #[must_use]
    pub fn table(&self, name: Option<&str>) -> Option<&DamageTable> {
        name.and_then(|name| self.tables.get(&name.to_ascii_lowercase()))
            .or_else(|| self.tables.get(DEFAULT_DAMAGE_TABLE))
    }

    /// The slowest contact worth telling apart, units/s: the slowest any
    /// table notices, or that is heard.
    #[must_use]
    pub fn slowest_contact(&self) -> f32 {
        self.tables
            .values()
            .map(|table| table.min_speed_sqr.max(0.0).sqrt())
            .fold(self.report_speed, f32::min)
    }
}

/// What impacts do to a prop: Source's `impactdamagetable_t`, read by its
/// `CalculatePhysicsImpactDamage`.
///
/// An impact's energy is `(Δv² · m + Δv_other² · m_other) / m · energy_scale`,
/// and the table entry it reaches is the damage. Speeds are units/s (so
/// squared speeds are as the game's tables have them) and masses kilograms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DamageTable {
    /// Multiplier on an impact's energy: Source's `physdamagescale`.
    pub energy_scale: f32,
    /// Steps of `(energy, damage)` from the prop's own and the other's
    /// change of speed. See [`read_steps`] for how they are read.
    pub linear: Vec<(f32, f32)>,
    /// Steps of `(angular impulse, damage)` from a blade spinning into it.
    pub angular: Vec<(f32, f32)>,
    /// Slower closing speeds, squared, do no harm.
    pub min_speed_sqr: f32,
    /// A blade spinning slower than this, squared (degrees/s), does none
    /// (see [`BladeSpin`]).
    pub min_rot_speed_sqr: f32,
    /// A held prop is not hurt by anything lighter than this.
    pub min_mass: f32,
    /// What is lighter than this is a small mass...
    pub small_mass_max: f32,
    /// ...which does no more than this (`0` for no limit)...
    pub small_mass_cap: f32,
    /// ...and does nothing to a held prop if slower than this, squared.
    pub small_mass_min_speed_sqr: f32,
    /// What is at least this heavy is a large mass...
    pub large_mass_min: f32,
    /// ...whose energy counts this many times...
    pub large_mass_scale: f32,
    /// ...and this many times over again as it falls straight down.
    pub large_mass_falling_scale: f32,
    /// The prop's own loss of speed counts only from this, units/s.
    pub my_min_speed: f32,
}

/// What a prop hits, as a [`DamageTable`] sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactOther {
    /// Its mass; `0` for the map.
    pub mass: f32,
    /// It cannot move: the map, or a prop fixed in place.
    pub fixed: bool,
    /// Someone holds it with the Manipulator.
    pub held: bool,
    /// Its speed before the impact, squared.
    pub speed_sqr: f32,
    /// How much of its speed was straight down before the impact, `0..=1`.
    pub falling: f32,
    /// How it spins, if it is a blade. HL2DM reads this only from objects
    /// flagged as slicing (`FVPHYSICS_DMG_SLICE`).
    pub blade: Option<BladeSpin>,
}

impl ImpactOther {
    /// The map, or anything else as immovable that does not come at a prop.
    pub const MAP: Self = Self {
        mass: 0.0,
        fixed: true,
        held: false,
        speed_sqr: 0.0,
        falling: 0.0,
        blade: None,
    };
}

/// A blade spinning into a prop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BladeSpin {
    /// Its spin, squared, in degrees/s.
    pub speed_sqr: f32,
    /// Its angular impulse: the sum over its axes of inertia times spin.
    pub impulse: f32,
}

/// An impact on a prop, as a [`DamageTable`] sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PropImpact {
    /// The prop's mass.
    pub mass: f32,
    /// It cannot move.
    pub fixed: bool,
    /// Someone holds it with the Manipulator.
    pub held: bool,
    /// How fast they were closing along the contact.
    pub closing: f32,
    /// What it hit.
    pub other: ImpactOther,
}

impl PropImpact {
    /// How much speed each of the two loses, `(the prop, the other)`, were
    /// they to stop against each other: all of it for what meets something
    /// immovable, else in inverse proportion to their masses.
    ///
    /// This stands in for Source's change in speed across the collision,
    /// which `Box3D` does not give per contact: it gives the approach speed.
    /// Bounces take no speed off the blow, so a prop that bounces well is
    /// hurt a little more than in Source.
    #[must_use]
    pub fn speed_changes(&self) -> (f32, f32) {
        match (self.fixed, self.other.fixed) {
            (true, true) => (0.0, 0.0),
            (true, false) => (0.0, self.closing),
            (false, true) => (self.closing, 0.0),
            (false, false) => {
                let total = (self.mass + self.other.mass).max(f32::EPSILON);
                (
                    self.closing * self.other.mass / total,
                    self.closing * self.mass / total,
                )
            }
        }
    }
}

impl DamageTable {
    /// The damage `impact` does to a prop: `CalculatePhysicsImpactDamage`,
    /// with props' `bDamageFromHeldObjects` off and `allowStaticDamage` on
    /// unless held. Its early-outs for ragdolls, dissolving, flagged and
    /// floating objects have no counterpart here.
    #[must_use]
    pub fn damage(&self, impact: &PropImpact) -> f32 {
        let other = &impact.other;
        if other.held || impact.closing * impact.closing < self.min_speed_sqr {
            return 0.0;
        }
        let spin = other.blade.map_or(0.0, |blade| blade.speed_sqr);
        if impact.held
            && (other.mass < self.min_mass
                || (other.mass < self.small_mass_max
                    && other.speed_sqr < self.small_mass_min_speed_sqr)
                || (other.speed_sqr < self.min_speed_sqr && spin < self.min_rot_speed_sqr))
        {
            return 0.0;
        }
        let mut damage = 0.0;
        if let Some(blade) = other.blade.filter(|_| spin > self.min_rot_speed_sqr) {
            damage = read_steps(&self.angular, blade.impulse * self.energy_scale);
        }
        let (own_change, other_change) = impact.speed_changes();
        let own = if own_change > 0.0 && own_change < self.my_min_speed {
            0.0
        } else {
            own_change * own_change * impact.mass
        };
        let mut theirs = other_change * other_change * other.mass;
        if other.mass >= self.large_mass_min {
            theirs *= self.large_mass_scale;
            if other_change > 0.0 {
                theirs *= 1.0 + other.falling * (self.large_mass_falling_scale - 1.0);
            }
        }
        let energy = (own + theirs) / impact.mass.max(f32::EPSILON) * self.energy_scale;
        damage += read_steps(&self.linear, energy);
        if !other.fixed && other.mass < self.small_mass_max && self.small_mass_cap > 0.0 {
            damage = damage.clamp(0.0, self.small_mass_cap);
        }
        damage
    }
}

/// Reads a table of `(threshold, damage)` steps as Source's `ReadDamageTable`
/// does, without interpolating: the damage of the entry before the first
/// whose threshold `value` is under; nothing below the first entry, the last
/// entry's damage beyond them all.
#[must_use]
pub fn read_steps(steps: &[(f32, f32)], value: f32) -> f32 {
    let mut damage = 0.0;
    for &(threshold, step) in steps {
        if value < threshold {
            return damage;
        }
        damage = step;
    }
    damage
}

/// Surface materials, looked up by a model's `$surfaceprop` (see
/// [`MaterialsConfig::get`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialsConfig {
    /// Used for any surface property not matched below.
    pub default: MaterialConfig,
    /// Materials by lower-case surface property name.
    pub surfaces: BTreeMap<String, MaterialConfig>,
}

impl MaterialsConfig {
    /// The material for `surface_prop`: the entry of that exact name, else
    /// of each shorter prefix ending before an underscore (`metal_barrel`
    /// falls back to `metal`), else the default. Case is ignored.
    #[must_use]
    pub fn get(&self, surface_prop: &str) -> &MaterialConfig {
        let name = surface_prop.trim().to_ascii_lowercase();
        let mut candidate = name.as_str();
        loop {
            if let Some(material) = self.surfaces.get(candidate) {
                return material;
            }
            match candidate.rfind('_') {
                Some(cut) => candidate = &candidate[..cut],
                None => return &self.default,
            }
        }
    }
}

/// One surface material.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialConfig {
    /// Coulomb friction coefficient.
    pub friction: f32,
    /// Bounciness: 0 absorbs impacts, 1 bounces back at full speed.
    pub restitution: f32,
    /// Density, kg/m³; gives the mass of props that do not declare one.
    pub density: f32,
}

/// How players push props they walk into.
///
/// The push treats the player as a body of [`player_mass`](Self::player_mass)
/// meeting the prop in a perfectly inelastic collision along the contact
/// normal, so a light prop is shoved along at the player's speed and a heavy
/// one barely moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushConfig {
    /// The player's mass, kg.
    pub player_mass: f32,
    /// Largest speed change one push may give a prop, units/s.
    pub max_speed_change: f32,
    /// The mass props meet a player with when they shove them, kg. Lighter
    /// than [`player_mass`](Self::player_mass), so even a small prop moves a
    /// player.
    pub shove_mass: f32,
    /// Largest speed change one tick of shoving gives a player, units/s.
    pub max_shove_change: f32,
}

/// Damage props deal to players they hit.
///
/// The damage comes from the prop's kinetic energy along the contact normal,
/// relative to the player, in joules: `½·m·v²` with `v` converted to m/s.
/// Energy below [`min_energy`](Self::min_energy) does nothing; above it,
/// every joule deals [`damage_per_joule`](Self::damage_per_joule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactDamageConfig {
    /// Slowest impact that can hurt, units/s, whatever the mass.
    pub min_speed: f32,
    /// Impact energy absorbed without harm, joules.
    pub min_energy: f32,
    /// Damage per joule above the threshold.
    pub damage_per_joule: f32,
    /// Most damage one impact may deal.
    pub max_damage: f32,
}

impl ImpactDamageConfig {
    /// Metres per Source unit (one inch).
    pub(crate) const METRES_PER_UNIT: f32 = 0.0254;

    /// The damage of a prop of `mass` kg meeting a player at `speed` units/s
    /// along the contact normal.
    #[must_use]
    pub fn damage(&self, mass: f32, speed: f32) -> f32 {
        if speed < self.min_speed {
            return 0.0;
        }
        let metres_per_second = speed * Self::METRES_PER_UNIT;
        let energy = 0.5 * mass * metres_per_second * metres_per_second;
        ((energy - self.min_energy) * self.damage_per_joule).clamp(0.0, self.max_damage)
    }
}

impl PhysicsConfig {
    /// Checks every value for sense, reporting each problem with its field
    /// path.
    pub(crate) fn validate(&self, check: &mut Validator) {
        check.positive("gravity", self.gravity);
        check.positive("max_speed", self.max_speed);
        check.positive("max_angular_speed", self.max_angular_speed);
        if self.ccd_substeps > 16 {
            check.fail(
                "ccd_substeps",
                &format!("must be at most 16, got {}", self.ccd_substeps),
            );
        }
        check.positive("max_corrective_speed", self.max_corrective_speed);
        check.positive("ccd_min_speed", self.ccd_min_speed);

        material(check, "materials.default", &self.materials.default);
        for (name, config) in &self.materials.surfaces {
            if name.is_empty() || name.trim() != name || name.to_ascii_lowercase() != *name {
                check.fail(
                    &format!("materials.surfaces.{name}"),
                    "names must be lower case, without surrounding spaces",
                );
            }
            material(check, &format!("materials.surfaces.{name}"), config);
        }

        check.positive("push.player_mass", self.push.player_mass);
        check.non_negative("push.max_speed_change", self.push.max_speed_change);
        check.positive("push.shove_mass", self.push.shove_mass);
        check.non_negative("push.max_shove_change", self.push.max_shove_change);

        let damage = &self.impact_damage;
        check.non_negative("impact_damage.min_speed", damage.min_speed);
        check.non_negative("impact_damage.min_energy", damage.min_energy);
        check.non_negative("impact_damage.damage_per_joule", damage.damage_per_joule);
        check.non_negative("impact_damage.max_damage", damage.max_damage);

        let breakables = &self.breakables;
        check.non_negative(
            "breakables.explosion_knockback",
            breakables.explosion_knockback,
        );
        check.non_negative("breakables.explosion_force", breakables.explosion_force);
        check.positive("breakables.weapon_toughness", breakables.weapon_toughness);
        let impact = &breakables.impact;
        check.positive("breakables.impact.report_speed", impact.report_speed);
        check.positive("breakables.impact.scrape_speed", impact.scrape_speed);
        check.non_negative("breakables.impact.repeat_seconds", impact.repeat_seconds);
        if !impact.tables.contains_key(DEFAULT_DAMAGE_TABLE) {
            check.fail(
                "breakables.impact.tables",
                &format!("must have a table called {DEFAULT_DAMAGE_TABLE}"),
            );
        }
        for (name, table) in &impact.tables {
            damage_table(
                check,
                &format!("breakables.impact.tables.{name}"),
                name,
                table,
            );
        }
        let barrels = &breakables.barrels;
        check.positive("breakables.barrels.burn_seconds", barrels.burn_seconds);
        check.positive("breakables.barrels.instant_damage", barrels.instant_damage);
        check.positive("breakables.barrels.shortest_fuse", barrels.shortest_fuse);
        check.positive("breakables.barrels.longest_fuse", barrels.longest_fuse);
        if barrels.shortest_fuse > barrels.longest_fuse {
            check.fail(
                "breakables.barrels.shortest_fuse",
                "must not exceed longest_fuse",
            );
        }
        check.positive(
            "breakables.barrels.launched_seconds",
            barrels.launched_seconds,
        );
    }
}

fn damage_table(check: &mut Validator, field: &str, name: &str, table: &DamageTable) {
    if name.to_ascii_lowercase() != name {
        check.fail(field, "names must be lower case");
    }
    check.positive(&format!("{field}.energy_scale"), table.energy_scale);
    if table.linear.is_empty() {
        check.fail(&format!("{field}.linear"), "must have a step");
    }
    for (axis, value) in [
        ("min_speed_sqr", table.min_speed_sqr),
        ("min_rot_speed_sqr", table.min_rot_speed_sqr),
        ("min_mass", table.min_mass),
        ("small_mass_max", table.small_mass_max),
        ("small_mass_cap", table.small_mass_cap),
        ("small_mass_min_speed_sqr", table.small_mass_min_speed_sqr),
        ("large_mass_min", table.large_mass_min),
        ("large_mass_scale", table.large_mass_scale),
        ("large_mass_falling_scale", table.large_mass_falling_scale),
        ("my_min_speed", table.my_min_speed),
    ] {
        check.non_negative(&format!("{field}.{axis}"), value);
    }
}

fn material(check: &mut Validator, field: &str, material: &MaterialConfig) {
    check.non_negative(&format!("{field}.friction"), material.friction);
    check.range(
        &format!("{field}.restitution"),
        material.restitution,
        0.0,
        1.0,
    );
    check.positive(&format!("{field}.density"), material.density);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(friction: f32) -> MaterialConfig {
        MaterialConfig {
            friction,
            restitution: 0.1,
            density: 1000.0,
        }
    }

    #[test]
    fn materials_fall_back_by_prefix_then_default() {
        let materials = MaterialsConfig {
            default: material(0.0),
            surfaces: BTreeMap::from([
                ("metal".to_owned(), material(1.0)),
                ("metal_box".to_owned(), material(2.0)),
            ]),
        };
        let friction = |name| materials.get(name).friction;
        assert_eq!(friction("metal_box"), 2.0);
        assert_eq!(friction("Metal_Barrel"), 1.0);
        assert_eq!(friction("metal_box_big"), 2.0);
        assert_eq!(friction("metalpanel"), 0.0);
        assert_eq!(friction(""), 0.0);
        assert_eq!(friction("_"), 0.0);
    }

    /// The tables as `data/physics.ron` has them.
    fn tables() -> PropImpactConfig {
        crate::GameData::builtin()
            .expect("builtin")
            .physics
            .breakables
            .impact
    }

    /// A prop of `mass` hitting the map at `closing`.
    fn on_the_map(mass: f32, closing: f32) -> PropImpact {
        PropImpact {
            mass,
            fixed: false,
            held: false,
            closing,
            other: ImpactOther::MAP,
        }
    }

    /// Something of `mass` moving at `speed`.
    fn moving(mass: f32, speed: f32) -> ImpactOther {
        ImpactOther {
            mass,
            fixed: false,
            speed_sqr: speed * speed,
            ..ImpactOther::MAP
        }
    }

    #[test]
    fn steps_are_read_without_interpolating() {
        let steps = [(10.0, 1.0), (20.0, 2.0), (30.0, 3.0)];
        assert_eq!(read_steps(&steps, 9.9), 0.0, "below the first");
        assert_eq!(read_steps(&steps, 10.0), 1.0);
        assert_eq!(read_steps(&steps, 19.9), 1.0);
        assert_eq!(read_steps(&steps, 30.0), 3.0, "the last");
        assert_eq!(read_steps(&steps, 1e9), 3.0, "beyond them all");
        assert_eq!(read_steps(&[], 5.0), 0.0);
    }

    #[test]
    fn the_glass_tables_out_of_order_last_step_reads_as_in_the_game() {
        let tables = tables();
        let glass = &tables.table(Some("glass")).expect("glass").linear;
        assert_eq!(read_steps(glass, 62_500.0), 75.0);
        assert_eq!(read_steps(glass, 249_999.0), 75.0);
        assert_eq!(read_steps(glass, 250_000.0), 500.0);
    }

    #[test]
    fn a_missing_or_unknown_table_is_the_default_one() {
        let tables = tables();
        let npc = tables.tables.get("npc");
        assert_eq!(tables.table(None), npc);
        assert_eq!(tables.table(Some("nonsense")), npc);
        assert_eq!(tables.table(Some("GLASS")), tables.tables.get("glass"));
        assert_ne!(tables.table(Some("glass")), npc);
    }

    #[test]
    fn glass_is_noticed_far_below_what_is_heard() {
        let tables = tables();
        assert_eq!(tables.slowest_contact(), 8.0);
        assert!(tables.slowest_contact() < tables.report_speed);
    }

    #[test]
    fn a_crate_takes_more_the_harder_it_lands() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        let at = |speed| npc.damage(&on_the_map(30.0, speed));
        // Sprinting a crate into a wall, or less: nothing.
        assert_eq!(at(200.0), 0.0);
        // A drop from 40 units: a scratch of a crate's 30 health.
        let drop = (2.0_f32 * 600.0 * 40.0).sqrt();
        assert!((1.0..30.0).contains(&at(drop)), "{}", at(drop));
        // A fall from 210 units, or a launch at 1500 units/s: broken.
        let fall = (2.0_f32 * 600.0 * 210.0).sqrt();
        assert!(at(fall) >= 30.0, "{}", at(fall));
        assert!(at(1500.0) >= 30.0, "{}", at(1500.0));
    }

    #[test]
    fn the_same_blow_means_the_same_whatever_the_props_mass() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        assert_eq!(
            npc.damage(&on_the_map(2.0, 600.0)),
            npc.damage(&on_the_map(200.0, 600.0))
        );
    }

    #[test]
    fn glass_breaks_on_a_short_drop_and_a_slow_touch_does_nothing() {
        let tables = tables();
        let glass = tables.table(Some("glass")).expect("glass");
        let drop = (2.0_f32 * 600.0 * 8.0).sqrt();
        assert!(glass.damage(&on_the_map(1.0, drop)) >= 5.0);
        assert_eq!(glass.damage(&on_the_map(1.0, 7.9)), 0.0);
        assert_eq!(glass.damage(&on_the_map(1.0, 40.0)), 0.0);
    }

    #[test]
    fn speed_changes_share_the_closing_speed_by_mass() {
        let mut impact = on_the_map(30.0, 100.0);
        assert_eq!(impact.speed_changes(), (100.0, 0.0), "the map stays put");
        impact.other = moving(10.0, 100.0);
        assert_eq!(impact.speed_changes(), (25.0, 75.0), "the light one gives");
        impact.fixed = true;
        assert_eq!(impact.speed_changes(), (0.0, 100.0));
        impact.other.fixed = true;
        assert_eq!(impact.speed_changes(), (0.0, 0.0));
    }

    #[test]
    fn a_large_mass_hits_harder_and_harder_still_falling() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        let struck_by = |mass: f32, falling: f32, closing: f32| {
            npc.damage(&PropImpact {
                closing,
                other: ImpactOther {
                    falling,
                    ..moving(mass, closing)
                },
                ..on_the_map(30.0, 0.0)
            })
        };
        // At this speed a 499 kg prop does less than one over the line.
        let closing = 500.0;
        assert!(struck_by(500.0, 0.0, closing) > struck_by(499.0, 0.0, closing));
        // Falling multiplies the large mass's energy as well.
        let closing = 300.0;
        assert!(struck_by(500.0, 1.0, closing) > struck_by(500.0, 0.0, closing));
    }

    #[test]
    fn a_small_mass_does_no_more_than_its_cap() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        let damage = npc.damage(&PropImpact {
            closing: 3000.0,
            other: moving(1.0, 3000.0),
            ..on_the_map(1.0, 0.0)
        });
        assert_eq!(damage, npc.small_mass_cap);
    }

    #[test]
    fn a_held_prop_is_not_hurt_by_the_map_and_nothing_is_hurt_by_a_held_prop() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        let mut held = on_the_map(30.0, 3000.0);
        assert!(npc.damage(&held) > 0.0, "if it were not held");
        held.held = true;
        assert_eq!(npc.damage(&held), 0.0);
        // A prop coming at it fast still counts.
        held.other = moving(30.0, 3000.0);
        assert!(npc.damage(&held) > 0.0);
        // What a player holds hurts nothing.
        let mut by_held = on_the_map(30.0, 3000.0);
        by_held.other = ImpactOther {
            held: true,
            ..moving(30.0, 3000.0)
        };
        assert_eq!(npc.damage(&by_held), 0.0);
    }

    #[test]
    fn a_fixed_prop_takes_what_hits_it() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        let impact = PropImpact {
            fixed: true,
            closing: 1500.0,
            other: moving(30.0, 1500.0),
            ..on_the_map(30.0, 0.0)
        };
        assert!(npc.damage(&impact) >= 30.0);
    }

    #[test]
    fn a_blade_does_angular_damage_only_when_spinning_fast() {
        let tables = tables();
        let npc = tables.table(None).expect("npc");
        let by_blade = |speed_sqr: f32| {
            npc.damage(&PropImpact {
                closing: 30.0,
                other: ImpactOther {
                    blade: Some(BladeSpin {
                        speed_sqr,
                        impulse: 1e6,
                    }),
                    ..moving(30.0, 30.0)
                },
                ..on_the_map(30.0, 0.0)
            })
        };
        assert_eq!(by_blade(100.0), 0.0);
        assert_eq!(by_blade(npc.min_rot_speed_sqr + 1.0), 500.0);
    }

    #[test]
    fn impact_damage_grows_with_energy_above_the_threshold() {
        let config = ImpactDamageConfig {
            min_speed: 100.0,
            min_energy: 100.0,
            damage_per_joule: 0.1,
            max_damage: 50.0,
        };
        assert_eq!(config.damage(1000.0, 99.0), 0.0, "too slow");
        // 10 kg at 1 m/s: 5 J, under the threshold.
        assert_eq!(config.damage(10.0, 1.0 / 0.0254), 0.0);
        // 10 kg at 10 m/s: 500 J, 400 over.
        let damage = config.damage(10.0, 10.0 / 0.0254);
        assert!((damage - 40.0).abs() < 1e-3, "{damage}");
        assert_eq!(config.damage(100.0, 2000.0), 50.0, "capped");
    }
}
