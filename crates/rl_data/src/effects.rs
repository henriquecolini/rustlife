//! `effects.ron`: what shots and blasts look like.
//!
//! Effects are drawn with the game's own sprite materials (named relative
//! to `materials/`, without extension), on quads facing the camera: muzzle
//! flashes, bullet tracers, sparks where bullets strike, and explosions,
//! whose sprites may be animated.

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
        let mut check = Validator::default();
        bad.validate(&mut check);
        assert_eq!(check.finish().len(), 6);
    }
}
