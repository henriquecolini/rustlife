//! `effects.ron`: what shots and blasts look like.
//!
//! Effects are drawn with the game's own sprite materials (named relative
//! to `materials/`, without extension), on quads facing the camera: muzzle
//! flashes, bullet tracers, the spray and decals where bullets strike, and
//! explosions, whose sprites may be animated, and what a thrown grenade
//! shows: a red dot flashing with its ticking and a red trail behind it.

use std::collections::BTreeMap;

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
    /// What a bullet throws off and leaves where it strikes, by surface
    /// class (`concrete`, `metal`, `wood`, `glass`, `dirt`, `water`);
    /// `default` catches the rest.
    pub bullet_impacts: BTreeMap<String, ImpactEffectConfig>,
    /// A bullet striking a player: blood and the stain it leaves.
    pub blood: ImpactEffectConfig,
    /// A blast.
    pub explosion: ExplosionEffectConfig,
    /// A grenade in flight.
    pub grenade: GrenadeEffectConfig,
}

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

/// What a bullet throws off a surface and the mark it leaves: spray
/// sprites flying away from the surface, and decals drawn flat on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactEffectConfig {
    /// Sprites to fling off, one per particle chosen at random.
    pub sprites: Vec<String>,
    /// A spray sprite's size, units.
    pub size: f32,
    /// How fast spray flies, units/s.
    pub speed: f32,
    /// How many spray sprites.
    pub particles: u32,
    /// How long spray lives, seconds.
    pub duration: f32,
    /// A tint multiplied into the spray sprites (linear RGB), for game
    /// textures that are drawn white and coloured by the engine.
    pub color: [f32; 3],
    /// Whether the spray is lit by the scene rather than drawn fullbright.
    /// The game's impact sprites are fullbright; blood should fall under the
    /// same light as everything else.
    pub lit: bool,
    /// Decals to choose from; empty for a surface that keeps no mark.
    pub decals: Vec<String>,
    /// A decal's size, units.
    pub decal_size: f32,
    /// How long a decal stays, seconds.
    pub decal_life: f32,
    /// The game's particle system (from its `.pcf` files) to play instead of
    /// the spray; the spray is used when the system is not there.
    #[serde(default)]
    pub pcf: Option<String>,
}

impl ImpactEffectConfig {
    fn validate(&self, check: &mut Validator, field: &str) {
        if self.sprites.is_empty() {
            check.fail(&format!("{field}.sprites"), "needs at least one sprite");
        }
        for (index, sprite) in self.sprites.iter().enumerate() {
            if sprite.trim().is_empty() {
                check.fail(&format!("{field}.sprites[{index}]"), "must name a sprite");
            }
        }
        check.positive(&format!("{field}.size"), self.size);
        check.positive(&format!("{field}.speed"), self.speed);
        check.positive(&format!("{field}.duration"), self.duration);
        for (axis, value) in self.color.iter().enumerate() {
            check.non_negative(&format!("{field}.color[{axis}]"), *value);
        }
        if self.particles == 0 {
            check.fail(&format!("{field}.particles"), "needs at least one");
        }
        for (index, decal) in self.decals.iter().enumerate() {
            if decal.trim().is_empty() {
                check.fail(&format!("{field}.decals[{index}]"), "must name a decal");
            }
        }
        if self
            .pcf
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            check.fail(&format!("{field}.pcf"), "must name a particle system");
        }
        if !self.decals.is_empty() {
            check.positive(&format!("{field}.decal_size"), self.decal_size);
            check.non_negative(&format!("{field}.decal_life"), self.decal_life);
        }
    }
}

/// What an explosion does to the view. The blast itself is drawn as the
/// game draws it (see `rl_client::explosion`): its size is the game's, not a
/// setting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplosionEffectConfig {
    /// How hard the view shakes right at the blast, `0..=1`. It fades
    /// with distance, reaching nothing at three times the blast's radius.
    pub shake: f32,
    /// A particle system of the game's own to play instead of the blast
    /// drawn by the client, if it has one. HL2DM draws its explosions
    /// without one, so this is unset.
    #[serde(default)]
    pub pcf: Option<String>,
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
    /// What a bullet throws off the surface class `class`, or the default
    /// surface's effect.
    #[must_use]
    pub fn bullet_impact(&self, class: &str) -> Option<&ImpactEffectConfig> {
        self.bullet_impacts
            .get(class)
            .or_else(|| self.bullet_impacts.get("default"))
    }

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
        if !self.bullet_impacts.contains_key("default") {
            check.fail("bullet_impacts", "needs a \"default\" surface");
        }
        for (surface, impact) in &self.bullet_impacts {
            impact.validate(check, &format!("bullet_impacts.{surface}"));
        }
        self.blood.validate(check, "blood");
        let grenade = &self.grenade;
        check.positive("grenade.dot_size", grenade.dot_size);
        check.range("grenade.dot_dim", grenade.dot_dim, 0.0, 1.0);
        check.positive("grenade.flash_fade", grenade.flash_fade);
        check.positive("grenade.trail_life", grenade.trail_life);
        check.non_negative("grenade.trail_width.0", grenade.trail_width.0);
        check.non_negative("grenade.trail_width.1", grenade.trail_width.1);
        check.range("grenade.trail_alpha", grenade.trail_alpha, 0.0, 1.0);
        let blast = &self.explosion;
        check.range("explosion.shake", blast.shake, 0.0, 1.0);
        if blast
            .pcf
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            check.fail("explosion.pcf", "must name a particle system");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impact() -> ImpactEffectConfig {
        ImpactEffectConfig {
            sprites: vec!["effects/spark".to_owned()],
            size: 3.0,
            speed: 60.0,
            particles: 5,
            duration: 0.4,
            color: [1.0; 3],
            lit: false,
            decals: vec!["decals/concrete/shot1".to_owned()],
            decal_size: 10.0,
            decal_life: 20.0,
            pcf: None,
        }
    }

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
            bullet_impacts: BTreeMap::from([
                ("default".to_owned(), impact()),
                ("metal".to_owned(), impact()),
            ]),
            blood: impact(),
            explosion: ExplosionEffectConfig {
                shake: 0.6,
                pcf: None,
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
        bad.bullet_impacts
            .get_mut("default")
            .expect("default impact")
            .sprites
            .clear();
        bad.explosion.shake = 2.0;
        bad.grenade.flash_fade = 0.0;
        bad.explosion.pcf = Some(" ".to_owned());
        bad.blood.pcf = Some(String::new());
        let mut check = Validator::default();
        bad.validate(&mut check);
        assert_eq!(check.finish().len(), 7);
    }
}
