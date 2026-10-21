//! `effects.ron`: what shots and blasts look like.
//!
//! Effects are drawn with the game's own sprite materials (named relative
//! to `materials/`, without extension), on quads facing the camera: muzzle
//! flashes, bullet tracers, sparks where bullets strike, and explosions,
//! whose sprites may be animated, and what a thrown grenade shows: a red dot
//! flashing with its ticking and a red trail behind it.

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// How combat effects look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Effects", deny_unknown_fields)]
pub struct EffectsConfig {
    /// A gun going off.
    pub muzzle_flash: MuzzleFlashConfig,
    /// A bullet's streak.
    pub tracer: TracerConfig,
    /// A bullet or swing striking something.
    pub impact: SpriteEffectConfig,
    /// A blast.
    pub explosion: ExplosionEffectConfig,
    /// A grenade in flight.
    pub grenade: GrenadeEffectConfig,
}

/// Most particles of one kind an explosion may ask for.
const MAX_PARTICLES: u32 = 200;

/// A muzzle flash: one of several sprites, briefly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MuzzleFlashConfig {
    /// Sprites to pick from, one per shot.
    pub sprites: Vec<String>,
    /// Size of the flash in the world, units.
    pub size: f32,
    /// Size of the flash at the view model's muzzle, units.
    pub view_size: f32,
    /// How long it shows, seconds.
    pub duration: f32,
}

/// A tracer: a streak flying from the gun to where the bullet stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracerConfig {
    /// Its sprite, stretched along the streak.
    pub sprite: String,
    /// Width, units.
    pub width: f32,
    /// Length, units.
    pub length: f32,
    /// Speed, units/s.
    pub speed: f32,
}

/// A sprite shown at a point for a while.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpriteEffectConfig {
    /// The sprite.
    pub sprite: String,
    /// Size, units.
    pub size: f32,
    /// How long it shows, seconds.
    pub duration: f32,
}

/// An explosion, drawn as particles the client makes itself: a flash, a
/// fireball of overlapping puffs, rising smoke, flying sparks and a
/// shockwave, with a shake of the view for those nearby.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplosionEffectConfig {
    /// The fireball's size as a share of the blast's radius.
    pub size_per_radius: f32,
    /// How far above the blast its centre is drawn, as a share of its size:
    /// fireballs rise, and one centred on a blast on the floor would be
    /// cut in half by it.
    pub rise: f32,
    /// Puffs of fire.
    pub fireballs: u32,
    /// Puffs of smoke.
    pub smoke: u32,
    /// Sparks thrown out.
    pub sparks: u32,
    /// Seconds the fireball burns.
    pub duration: f32,
    /// Seconds the smoke lingers.
    pub smoke_duration: f32,
    /// How hard the view shakes right at the blast, `0..=1`. It fades
    /// with distance, reaching nothing at three times the blast's radius.
    pub shake: f32,
}

/// What a grenade in flight shows: a glowing red dot on its top that
/// flashes with each tick of its fuse and dims between, and a red trail of
/// where it has been.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrenadeEffectConfig {
    /// The dot's size at its brightest, units.
    pub dot_size: f32,
    /// Where the dot sits, in the grenade's own frame relative to its
    /// centre, units.
    pub dot_offset: [f32; 3],
    /// The dot's colour (linear RGB; values above one burn white).
    pub dot_color: [f32; 3],
    /// How bright the dot stays between ticks, `0..=1` of its brightest.
    pub dot_dim: f32,
    /// Seconds a tick's flash takes to fade to the dim.
    pub flash_fade: f32,
    /// How long the trail lasts behind the grenade, seconds.
    pub trail_life: f32,
    /// The trail's width at the grenade and at its far end, units.
    pub trail_width: (f32, f32),
    /// The trail's colour (linear RGB).
    pub trail_color: [f32; 3],
    /// The trail's opacity at the grenade, `0..=1`; it fades to nothing at
    /// its far end.
    pub trail_alpha: f32,
}

impl EffectsConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        let named = |check: &mut Validator, field: &str, sprite: &str| {
            if sprite.trim().is_empty() {
                check.fail(field, "must name a sprite");
            }
        };
        let flash = &self.muzzle_flash;
        if flash.sprites.is_empty() {
            check.fail("muzzle_flash.sprites", "needs at least one sprite");
        }
        for (index, sprite) in flash.sprites.iter().enumerate() {
            named(check, &format!("muzzle_flash.sprites[{index}]"), sprite);
        }
        check.positive("muzzle_flash.size", flash.size);
        check.positive("muzzle_flash.view_size", flash.view_size);
        check.positive("muzzle_flash.duration", flash.duration);
        named(check, "tracer.sprite", &self.tracer.sprite);
        check.positive("tracer.width", self.tracer.width);
        check.positive("tracer.length", self.tracer.length);
        check.positive("tracer.speed", self.tracer.speed);
        named(check, "impact.sprite", &self.impact.sprite);
        check.positive("impact.size", self.impact.size);
        check.positive("impact.duration", self.impact.duration);
        let grenade = &self.grenade;
        check.positive("grenade.dot_size", grenade.dot_size);
        check.range("grenade.dot_dim", grenade.dot_dim, 0.0, 1.0);
        check.positive("grenade.flash_fade", grenade.flash_fade);
        check.positive("grenade.trail_life", grenade.trail_life);
        check.non_negative("grenade.trail_width.0", grenade.trail_width.0);
        check.non_negative("grenade.trail_width.1", grenade.trail_width.1);
        check.range("grenade.trail_alpha", grenade.trail_alpha, 0.0, 1.0);
        let blast = &self.explosion;
        check.positive("explosion.size_per_radius", blast.size_per_radius);
        check.range("explosion.rise", blast.rise, 0.0, 1.0);
        check.positive("explosion.duration", blast.duration);
        check.positive("explosion.smoke_duration", blast.smoke_duration);
        check.range("explosion.shake", blast.shake, 0.0, 1.0);
        for (field, count) in [
            ("explosion.fireballs", blast.fireballs),
            ("explosion.smoke", blast.smoke),
            ("explosion.sparks", blast.sparks),
        ] {
            if count > MAX_PARTICLES {
                check.fail(field, &format!("at most {MAX_PARTICLES}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EffectsConfig {
        EffectsConfig {
            muzzle_flash: MuzzleFlashConfig {
                sprites: vec!["effects/muzzleflash1".to_owned()],
                size: 14.0,
                view_size: 8.0,
                duration: 0.05,
            },
            tracer: TracerConfig {
                sprite: "effects/spark".to_owned(),
                width: 1.5,
                length: 120.0,
                speed: 6000.0,
            },
            impact: SpriteEffectConfig {
                sprite: "effects/spark".to_owned(),
                size: 8.0,
                duration: 0.1,
            },
            explosion: ExplosionEffectConfig {
                size_per_radius: 1.2,
                rise: 0.3,
                fireballs: 8,
                smoke: 10,
                sparks: 24,
                duration: 0.9,
                smoke_duration: 3.0,
                shake: 0.6,
            },
            grenade: GrenadeEffectConfig {
                dot_size: 6.0,
                dot_offset: [0.0, 0.0, 5.0],
                dot_color: [1.0, 0.1, 0.05],
                dot_dim: 0.2,
                flash_fade: 0.2,
                trail_life: 0.5,
                trail_width: (3.0, 0.5),
                trail_color: [1.0, 0.05, 0.02],
                trail_alpha: 0.8,
            },
        }
    }

    #[test]
    fn sensible_values_pass() {
        let mut check = Validator::default();
        config().validate(&mut check);
        assert!(check.finish().is_empty());
    }

    #[test]
    fn every_bad_value_is_reported() {
        let mut bad = config();
        bad.muzzle_flash.sprites.clear();
        bad.tracer.speed = 0.0;
        bad.impact.sprite.clear();
        bad.explosion.duration = -1.0;
        bad.explosion.sparks = 5000;
        bad.explosion.rise = 2.0;
        bad.grenade.flash_fade = 0.0;
        let mut check = Validator::default();
        bad.validate(&mut check);
        assert_eq!(check.finish().len(), 7);
    }
}
