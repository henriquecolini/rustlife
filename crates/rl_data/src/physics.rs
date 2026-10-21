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
    /// Constraint solver iterations per tick: more is stiffer stacking at
    /// a higher cost.
    pub solver_iterations: u32,
    /// Gauss-Seidel passes within each solver iteration: more keeps hard
    /// hits from sinking props into each other, at a higher cost.
    pub pgs_iterations: u32,
    /// Sub-steps continuous collision detection may take within a tick, so
    /// fast props are stopped by other props and not only by the map.
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreakablesConfig {
    /// Speed given to a player at the centre of an exploding prop's blast,
    /// units/s.
    pub explosion_knockback: f32,
    /// Impulse given to props at the centre of such a blast, kg·units/s.
    pub explosion_force: f32,
    /// Props striking each other or the map, which make noise and may break
    /// the prop.
    pub impact: PropImpactConfig,
}

/// Props striking props and the map.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropImpactConfig {
    /// Slower closing speeds than this, units/s, are not reported at all
    /// (no sound, no damage).
    pub report_speed: f32,
    /// Impact energy, joules, that does the struck prop no harm...
    pub min_energy: f32,
    /// ...and the damage per joule beyond it.
    pub damage_per_joule: f32,
    /// A prop sliding (not rolling) faster than this, units/s, scrapes.
    pub scrape_speed: f32,
}

impl PropImpactConfig {
    /// The damage to a prop when `mass` kg of other prop (or of itself, for
    /// the map) strikes it at `speed` units/s.
    #[must_use]
    pub fn damage(&self, mass: f32, speed: f32) -> f32 {
        if speed < self.report_speed {
            return 0.0;
        }
        let metres_per_second = speed * ImpactDamageConfig::METRES_PER_UNIT;
        let energy = 0.5 * mass * metres_per_second * metres_per_second;
        ((energy - self.min_energy) * self.damage_per_joule).max(0.0)
    }
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
        if !(1..=64).contains(&self.solver_iterations) {
            check.fail(
                "solver_iterations",
                &format!("must be within 1..=64, got {}", self.solver_iterations),
            );
        }

        if !(1..=16).contains(&self.pgs_iterations) {
            check.fail(
                "pgs_iterations",
                &format!("must be within 1..=16, got {}", self.pgs_iterations),
            );
        }
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
        check.positive(
            "breakables.impact.report_speed",
            breakables.impact.report_speed,
        );
        check.non_negative("breakables.impact.min_energy", breakables.impact.min_energy);
        check.positive(
            "breakables.impact.scrape_speed",
            breakables.impact.scrape_speed,
        );
        check.non_negative(
            "breakables.impact.damage_per_joule",
            breakables.impact.damage_per_joule,
        );
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
