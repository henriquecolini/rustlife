//! Combat: health and armor, damage, and finding what a shot hits.
//!
//! - [`Vitals`] is a player's health and armor, and how damage and pickups
//!   change them (`player.ron`).
//! - [`Damage`] is one instance of harm, with who and what caused it, for
//!   the kill feed and scoring.
//! - [`Target`] is a player's box as weapons see it; [`hitscan`] traces
//!   bullets and swings through the map, its props and those boxes, and
//!   [`blast`] finds who and what an explosion reaches.
//!
//! Players are hit as their movement box: rustlife has no per-limb hitboxes
//! (they need animated skeletons, M7).

use glam::Vec3;
use rl_data::{ArmorConfig, PlayerConfig};

use crate::simulation::PlayerId;
use crate::weapons::WeaponId;

pub mod blast;
pub mod hitscan;

/// A player's health and armor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vitals {
    /// Health; the player is dead at zero or below.
    pub health: f32,
    /// Armor points.
    pub armor: f32,
}

impl Vitals {
    /// A freshly spawned player's vitals.
    #[must_use]
    pub fn spawn(config: &PlayerConfig) -> Self {
        Self {
            health: config.health.spawn,
            armor: config.armor.spawn,
        }
    }

    /// Whether the player is alive.
    #[must_use]
    pub fn alive(&self) -> bool {
        self.health > 0.0
    }

    /// Takes `damage`, armor soaking its share; returns the health lost.
    pub fn take(&mut self, damage: f32, armor: &ArmorConfig) -> f32 {
        let (health_damage, armor_used) = armor.split(damage, self.armor);
        self.armor = (self.armor - armor_used).max(0.0);
        self.health -= health_damage;
        health_damage
    }

    /// Adds up to `amount` health without exceeding `max`; returns whether
    /// any was added (a full player leaves a medkit where it is).
    pub fn heal(&mut self, amount: f32, max: f32) -> bool {
        raise(&mut self.health, amount, max)
    }

    /// Adds up to `amount` armor without exceeding `max`; returns whether
    /// any was added.
    pub fn add_armor(&mut self, amount: f32, max: f32) -> bool {
        raise(&mut self.armor, amount, max)
    }
}

/// Raises `value` by `amount` up to `max`; whether it rose.
fn raise(value: &mut f32, amount: f32, max: f32) -> bool {
    if *value >= max || amount <= 0.0 {
        return false;
    }
    *value = (*value + amount).min(max);
    true
}

/// What dealt some damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cause {
    /// A weapon's shot, swing, projectile or blast.
    Weapon(WeaponId),
    /// A prop hitting the player.
    Prop,
    /// Landing too hard.
    Fall,
    /// The map: a `trigger_hurt` volume.
    Hazard,
}

/// One instance of harm to a player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Damage {
    /// How much, before armor.
    pub amount: f32,
    /// The player responsible, if any (a prop someone threw counts as
    /// theirs).
    pub attacker: Option<PlayerId>,
    /// What did it.
    pub cause: Cause,
}

/// A player's box in the world, as shots see it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    /// The player.
    pub player: PlayerId,
    /// Minimum corner, Source space.
    pub mins: Vec3,
    /// Maximum corner, Source space.
    pub maxs: Vec3,
}

impl Target {
    /// The box's centre.
    #[must_use]
    pub fn center(&self) -> Vec3 {
        (self.mins + self.maxs) * 0.5
    }

    /// The point of the box nearest `point` (`point` itself if inside).
    #[must_use]
    pub fn nearest_point(&self, point: Vec3) -> Vec3 {
        point.clamp(self.mins, self.maxs)
    }

    /// Where a box of half-size `extents` moving from `start` to `end`
    /// first touches this box: the fraction of the move, and the normal of
    /// the face touched. `None` if it misses, or starts inside.
    #[must_use]
    pub fn sweep(&self, start: Vec3, end: Vec3, extents: Vec3) -> Option<(f32, Vec3)> {
        segment_box(start, end, self.mins - extents, self.maxs + extents)
    }
}

/// The first point where the segment `start → end` enters the box
/// `mins..maxs` (slab method): the fraction along the segment and the
/// normal of the face entered. `None` if it misses or starts inside.
#[must_use]
pub fn segment_box(start: Vec3, end: Vec3, mins: Vec3, maxs: Vec3) -> Option<(f32, Vec3)> {
    let delta = end - start;
    let mut enter = 0.0_f32;
    let mut exit = 1.0_f32;
    let mut normal = Vec3::ZERO;
    for axis in 0..3 {
        let (origin, step) = (start[axis], delta[axis]);
        if step.abs() < f32::EPSILON {
            if origin < mins[axis] || origin > maxs[axis] {
                return None;
            }
            continue;
        }
        let (mut near, mut far) = ((mins[axis] - origin) / step, (maxs[axis] - origin) / step);
        let mut face = Vec3::ZERO;
        face[axis] = -step.signum();
        if near > far {
            std::mem::swap(&mut near, &mut far);
        }
        if near > enter {
            enter = near;
            normal = face;
        }
        exit = exit.min(far);
        if enter > exit {
            return None;
        }
    }
    // Starting inside (no face entered) is not a hit: the shooter's own
    // box, or a projectile spawned overlapping someone.
    (normal != Vec3::ZERO).then_some((enter, normal))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARMOR: ArmorConfig = ArmorConfig {
        spawn: 0.0,
        max: 100.0,
        absorb: 0.5,
        cost: 1.0,
    };

    #[test]
    fn armor_soaks_damage_until_it_runs_out() {
        let mut vitals = Vitals {
            health: 100.0,
            armor: 10.0,
        };
        assert_eq!(vitals.take(40.0, &ARMOR), 30.0);
        assert_eq!(vitals.armor, 0.0);
        assert_eq!(vitals.take(40.0, &ARMOR), 40.0);
        assert!(vitals.alive());
        vitals.take(30.0, &ARMOR);
        assert!(!vitals.alive());
    }

    #[test]
    fn pickups_top_up_without_overfilling() {
        let mut vitals = Vitals {
            health: 90.0,
            armor: 0.0,
        };
        assert!(vitals.heal(25.0, 100.0));
        assert_eq!(vitals.health, 100.0);
        assert!(!vitals.heal(25.0, 100.0), "a full player takes nothing");
        assert!(vitals.add_armor(15.0, 100.0));
        assert_eq!(vitals.armor, 15.0);
    }

    #[test]
    fn segments_enter_boxes_through_the_nearest_face() {
        let (mins, maxs) = (Vec3::splat(-1.0), Vec3::splat(1.0));
        let hit = segment_box(
            Vec3::new(-3.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
            mins,
            maxs,
        );
        assert_eq!(hit, Some((1.0 / 3.0, Vec3::NEG_X)));
        let above = segment_box(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -5.0),
            mins,
            maxs,
        );
        assert_eq!(above, Some((0.4, Vec3::Z)));
        assert_eq!(
            segment_box(
                Vec3::new(-3.0, 2.0, 0.0),
                Vec3::new(3.0, 2.0, 0.0),
                mins,
                maxs
            ),
            None,
            "passes beside"
        );
        assert_eq!(
            segment_box(
                Vec3::new(-3.0, 0.0, 0.0),
                Vec3::new(-2.0, 0.0, 0.0),
                mins,
                maxs
            ),
            None,
            "stops short"
        );
        assert_eq!(
            segment_box(Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), mins, maxs),
            None,
            "starts inside"
        );
    }

    #[test]
    fn swept_boxes_touch_targets_sooner() {
        let target = Target {
            player: PlayerId::from_index(0),
            mins: Vec3::new(10.0, -16.0, 0.0),
            maxs: Vec3::new(42.0, 16.0, 72.0),
        };
        let start = Vec3::new(0.0, 0.0, 36.0);
        let end = Vec3::new(20.0, 0.0, 36.0);
        let (ray, _) = target.sweep(start, end, Vec3::ZERO).expect("ray hits");
        let (fat, _) = target
            .sweep(start, end, Vec3::splat(4.0))
            .expect("box hits");
        assert!((ray - 0.5).abs() < 1e-6 && (fat - 0.3).abs() < 1e-6);
        assert_eq!(
            target.nearest_point(Vec3::new(0.0, 0.0, 100.0)),
            Vec3::new(10.0, 0.0, 72.0)
        );
    }
}
