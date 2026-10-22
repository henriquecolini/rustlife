//! `sounds.ron`: what the game sounds like, beyond weapons and items.
//!
//! Each sound is named as the game's own content names it: a soundscript
//! entry (`BaseExplosionEffect.Sound`) or a file under `sound/`
//! (`player/pl_step1.wav`). Weapons name their own sounds
//! ([`crate::WeaponConfig::sounds`]), and so do items
//! ([`crate::ItemConfig::sounds`]); this file names the rest: players'
//! footsteps and falls, deaths, explosions, bullets striking, things
//! breaking and the map's buttons.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::validate::Validator;

/// The game's sounds that belong to no weapon or item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Sounds", deny_unknown_fields)]
pub struct SoundsConfig {
    /// Players' footsteps.
    pub footsteps: FootstepsConfig,
    /// A landing that hurt.
    pub fall_damage: String,
    /// A weapon picked up.
    pub weapon_pickup: String,
    /// A player dying.
    pub death: String,
    /// A blast.
    pub explosion: String,
    /// A bullet or swing striking the map or a prop.
    pub impact: String,
    /// A bullet or swing striking a player.
    pub flesh_impact: String,
    /// Breakables breaking, by their `material` (`glass`, `wood`, `metal`,
    /// `flesh`, `cinder`, `tile`, `computer`, `rock`); `default` for the
    /// rest.
    pub breaking: BTreeMap<String, String>,
    /// A button's press: its `sounds` number replaces `{}`
    /// (`Buttons.snd{}`).
    pub button: String,
    /// Props knocking about: impacts and scraping.
    pub props: PropSoundsConfig,
    /// Choosing a weapon from the weapon list.
    pub weapon_select: WeaponSelectSounds,
}

/// The sounds of the weapon list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponSelectSounds {
    /// The cursor moving to another weapon.
    pub step: String,
    /// A weapon picked.
    pub pick: String,
    /// The list closing with no pick.
    pub close: String,
}

/// How props sound as they strike things and slide about. The sounds
/// themselves are the surfaces' own (`surfaceproperties.txt`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropSoundsConfig {
    /// Impacts closing faster than this, units/s, are the surface's hard
    /// sound; slower ones its soft sound.
    pub hard_speed: f32,
    /// Closing speed, units/s, at which an impact reaches full volume.
    pub loud_speed: f32,
    /// Mass, kg, at which an impact reaches full volume.
    pub loud_mass: f32,
    /// Quietest an impact is, `0..=1`.
    pub min_volume: f32,
    /// Seconds before the same prop makes another impact sound.
    pub cooldown: f32,
    /// Most impact sounds started in one frame.
    pub max_per_frame: u32,
    /// Seconds without news of a sliding prop before its scraping stops.
    pub scrape_timeout: f32,
    /// Sliding speed, units/s, at which scraping reaches full volume.
    pub scrape_loud_speed: f32,
}

/// Footsteps: one every so often while moving on the ground, left and
/// right in turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FootstepsConfig {
    /// The left foot's sound.
    pub left: String,
    /// The right foot's sound.
    pub right: String,
    /// Seconds between steps when running.
    pub run_interval: f32,
    /// Seconds between steps when walking or crouched.
    pub walk_interval: f32,
    /// Below this speed (units/s) a player walks; below a quarter of it
    /// they make no sound at all.
    pub run_speed: f32,
    /// Volume of a running step, `0..=1`.
    pub run_volume: f32,
    /// Volume of a walking step, `0..=1`.
    pub walk_volume: f32,
}

impl SoundsConfig {
    pub(crate) fn validate(&self, check: &mut Validator) {
        let named = |check: &mut Validator, field: &str, name: &str| {
            if name.trim().is_empty() {
                check.fail(field, "must name a sound");
            }
        };
        let steps = &self.footsteps;
        named(check, "footsteps.left", &steps.left);
        named(check, "footsteps.right", &steps.right);
        check.positive("footsteps.run_interval", steps.run_interval);
        check.positive("footsteps.walk_interval", steps.walk_interval);
        check.positive("footsteps.run_speed", steps.run_speed);
        check.range("footsteps.run_volume", steps.run_volume, 0.0, 1.0);
        check.range("footsteps.walk_volume", steps.walk_volume, 0.0, 1.0);
        named(check, "fall_damage", &self.fall_damage);
        named(check, "weapon_pickup", &self.weapon_pickup);
        named(check, "death", &self.death);
        named(check, "explosion", &self.explosion);
        named(check, "impact", &self.impact);
        named(check, "flesh_impact", &self.flesh_impact);
        if !self.breaking.contains_key("default") {
            check.fail("breaking", "needs a \"default\" sound");
        }
        for (material, name) in &self.breaking {
            named(check, &format!("breaking.{material}"), name);
        }
        let props = &self.props;
        check.positive("props.hard_speed", props.hard_speed);
        check.positive("props.loud_speed", props.loud_speed);
        check.positive("props.loud_mass", props.loud_mass);
        check.range("props.min_volume", props.min_volume, 0.0, 1.0);
        check.non_negative("props.cooldown", props.cooldown);
        check.positive("props.scrape_timeout", props.scrape_timeout);
        check.positive("props.scrape_loud_speed", props.scrape_loud_speed);
        let select = &self.weapon_select;
        named(check, "weapon_select.step", &select.step);
        named(check, "weapon_select.pick", &select.pick);
        named(check, "weapon_select.close", &select.close);
        if !self.button.contains("{}") {
            check.fail("button", "must contain {} for the button's sound number");
        }
    }

    /// The sound of a breakable of `material` breaking.
    #[must_use]
    pub fn breaking(&self, material: &str) -> Option<&str> {
        self.breaking
            .get(material)
            .or_else(|| self.breaking.get("default"))
            .map(String::as_str)
    }

    /// The sound of a button whose `sounds` key is `number`.
    #[must_use]
    pub fn button(&self, number: u32) -> String {
        self.button.replace("{}", &number.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SoundsConfig {
        SoundsConfig {
            footsteps: FootstepsConfig {
                left: "Default.StepLeft".to_owned(),
                right: "Default.StepRight".to_owned(),
                run_interval: 0.3,
                walk_interval: 0.4,
                run_speed: 150.0,
                run_volume: 0.5,
                walk_volume: 0.2,
            },
            fall_damage: "Player.FallDamage".to_owned(),
            weapon_pickup: "Player.PickupWeapon".to_owned(),
            death: "Player.Death".to_owned(),
            explosion: "BaseExplosionEffect.Sound".to_owned(),
            impact: "Concrete.BulletImpact".to_owned(),
            flesh_impact: "Flesh.BulletImpact".to_owned(),
            breaking: BTreeMap::from([
                ("default".to_owned(), "Breakable.Crate".to_owned()),
                ("glass".to_owned(), "Breakable.MatGlass".to_owned()),
            ]),
            button: "Buttons.snd{}".to_owned(),
            props: PropSoundsConfig {
                hard_speed: 320.0,
                loud_speed: 700.0,
                loud_mass: 60.0,
                min_volume: 0.15,
                cooldown: 0.1,
                max_per_frame: 4,
                scrape_timeout: 0.3,
                scrape_loud_speed: 400.0,
            },
            weapon_select: WeaponSelectSounds {
                step: "Player.WeaponSelectionMoveSlot".to_owned(),
                pick: "Player.WeaponSelected".to_owned(),
                close: "Player.WeaponSelectionClose".to_owned(),
            },
        }
    }

    #[test]
    fn sensible_values_pass_and_look_up() {
        let mut check = Validator::default();
        config().validate(&mut check);
        assert!(check.finish().is_empty());
        let config = config();
        assert_eq!(config.breaking("glass"), Some("Breakable.MatGlass"));
        assert_eq!(config.breaking("rock"), Some("Breakable.Crate"));
        assert_eq!(config.button(14), "Buttons.snd14");
    }

    #[test]
    fn every_bad_value_is_reported() {
        let mut bad = config();
        bad.footsteps.left.clear();
        bad.footsteps.run_interval = 0.0;
        bad.footsteps.walk_volume = 2.0;
        bad.explosion = " ".to_owned();
        bad.breaking.remove("default");
        bad.button = "Buttons.snd".to_owned();
        bad.props.min_volume = 3.0;
        bad.props.scrape_timeout = 0.0;
        let mut check = Validator::default();
        bad.validate(&mut check);
        assert_eq!(check.finish().len(), 8);
    }
}
