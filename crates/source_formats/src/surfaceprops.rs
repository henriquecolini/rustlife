//! `scripts/surfaceproperties.txt`: what the game says things sound like.
//!
//! Each surface (`wood_crate`, `metal_barrel`, `concrete`) names the sounds
//! it makes when something hits it hard or soft, scrapes along it rough or
//! smooth, rolls on it, breaks, or is shot. A surface may be `"base"`d on
//! another and inherits everything it does not set; every surface inherits
//! from `default`.

use std::collections::HashMap;

use source_fs::keyvalues;

/// How deep `"base"` chains are followed.
const MAX_BASE_DEPTH: usize = 8;

/// What a surface sounds like. Names are soundscript entries; empty when the
/// surface (and `default`) do not say.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurfaceSounds {
    /// The left foot stepping on it.
    pub step_left: String,
    /// The right foot stepping on it.
    pub step_right: String,
    /// A hard impact.
    pub impact_hard: String,
    /// A soft impact.
    pub impact_soft: String,
    /// Scraping across a rough surface (a loop).
    pub scrape_rough: String,
    /// Scraping across a smooth surface (a loop).
    pub scrape_smooth: String,
    /// Rolling (a loop), for surfaces that have it.
    pub roll: String,
    /// Breaking.
    pub break_sound: String,
    /// Being shot.
    pub bullet_impact: String,
}

/// The surfaces of `surfaceproperties.txt`.
#[derive(Debug, Clone, Default)]
pub struct SurfaceTable {
    /// Each surface's own keys (lower-cased), by lower-cased name.
    surfaces: HashMap<String, Vec<(String, String)>>,
}

impl SurfaceTable {
    /// Parses the file; malformed text gives an empty table (everything is
    /// silent).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let Ok(document) = keyvalues::parse(text) else {
            return Self::default();
        };
        let surfaces = document
            .root
            .iter()
            .filter_map(|pair| {
                let block = pair.value.as_block()?;
                let entries = block
                    .iter()
                    .filter_map(|entry| {
                        Some((
                            entry.key.to_ascii_lowercase(),
                            entry.value.as_str()?.to_owned(),
                        ))
                    })
                    .collect();
                Some((pair.key.to_ascii_lowercase(), entries))
            })
            .collect();
        Self { surfaces }
    }

    /// What `surface` sounds like: its own settings, over its bases', over
    /// `default`'s. A name the table lacks falls back to its prefix before the
    /// last underscore, then to `default`.
    #[must_use]
    pub fn sounds(&self, surface: &str) -> SurfaceSounds {
        let mut sounds = SurfaceSounds::default();
        self.apply("default", &mut sounds, 0);
        let mut name = surface.trim().to_ascii_lowercase();
        loop {
            if self.surfaces.contains_key(&name) {
                self.apply(&name, &mut sounds, 0);
                return sounds;
            }
            match name.rfind('_') {
                Some(cut) => name.truncate(cut),
                None => return sounds,
            }
        }
    }

    /// Every sound the table names for footsteps or bullet impacts, for
    /// loading them ahead of the first step or shot.
    pub fn sounds_used(&self) -> impl Iterator<Item = &str> {
        self.surfaces
            .values()
            .flat_map(|entries| entries.iter())
            .filter(|(key, _)| matches!(key.as_str(), "stepleft" | "stepright" | "bulletimpact"))
            .map(|(_, value)| value.as_str())
    }

    fn apply(&self, name: &str, sounds: &mut SurfaceSounds, depth: usize) {
        if depth >= MAX_BASE_DEPTH {
            return;
        }
        let Some(own) = self.surfaces.get(&name.to_ascii_lowercase()) else {
            return;
        };
        if let Some((_, base)) = own.iter().find(|(key, _)| key == "base") {
            self.apply(base, sounds, depth + 1);
        }
        for (key, value) in own {
            let slot = match key.as_str() {
                "stepleft" => &mut sounds.step_left,
                "stepright" => &mut sounds.step_right,
                "impacthard" => &mut sounds.impact_hard,
                "impactsoft" => &mut sounds.impact_soft,
                "scraperough" => &mut sounds.scrape_rough,
                "scrapesmooth" => &mut sounds.scrape_smooth,
                "roll" => &mut sounds.roll,
                "break" => &mut sounds.break_sound,
                "bulletimpact" => &mut sounds.bullet_impact,
                _ => continue,
            };
            value.clone_into(slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
"default" { "stepleft" "Default.StepLeft" "stepright" "Default.StepRight" "bulletimpact" "Default.BulletImpact" "impacthard" "Default.ImpactHard" "impactsoft" "Default.ImpactSoft" "scraperough" "Default.ScrapeRough" "scrapesmooth" "Default.ScrapeSmooth" "density" "2000" }
"metal_box" { "stepleft" "Metal_Box.StepLeft" "stepright" "Metal_Box.StepRight" "impacthard" "Metal_Box.ImpactHard" "impactsoft" "Metal_Box.ImpactSoft" "scraperough" "MetalBox.Scrape" }
"Metal_Barrel" { "base" "metal_box" "impacthard" "Metal_Barrel.ImpactHard" "roll" "Metal_Barrel.Roll" }
"loop_a" { "base" "loop_b" "break" "A" }
"loop_b" { "base" "loop_a" "roll" "B" }
"#;

    #[test]
    fn surfaces_inherit_from_their_bases_and_default() {
        let table = SurfaceTable::parse(TABLE);
        let barrel = table.sounds("metal_barrel");
        assert_eq!(barrel.step_left, "Metal_Box.StepLeft", "from its base");
        assert_eq!(barrel.step_right, "Metal_Box.StepRight");
        assert_eq!(barrel.impact_hard, "Metal_Barrel.ImpactHard");
        assert_eq!(barrel.impact_soft, "Metal_Box.ImpactSoft", "from its base");
        assert_eq!(barrel.scrape_rough, "MetalBox.Scrape");
        assert_eq!(barrel.scrape_smooth, "Default.ScrapeSmooth", "from default");
        assert_eq!(barrel.roll, "Metal_Barrel.Roll");
        assert_eq!(barrel.break_sound, "");
    }

    #[test]
    fn unknown_surfaces_fall_back_by_prefix_then_to_default() {
        let table = SurfaceTable::parse(TABLE);
        assert_eq!(
            table.sounds("metal_barrel_buoyant").impact_hard,
            "Metal_Barrel.ImpactHard"
        );
        assert_eq!(table.sounds("nope").impact_hard, "Default.ImpactHard");
        assert_eq!(
            table.sounds("  METAL_BOX ").impact_soft,
            "Metal_Box.ImpactSoft"
        );
    }

    #[test]
    fn every_step_and_bullet_sound_is_listed_for_preloading() {
        let table = SurfaceTable::parse(TABLE);
        let names: Vec<&str> = table.sounds_used().collect();
        assert!(names.contains(&"Metal_Box.StepLeft"));
        assert!(names.contains(&"Default.BulletImpact"));
        assert!(
            !names.contains(&"MetalBox.Scrape"),
            "not a step or bullet sound"
        );
    }

    #[test]
    fn base_loops_and_garbage_are_survived() {
        let table = SurfaceTable::parse(TABLE);
        let looped = table.sounds("loop_a");
        assert_eq!(
            (looped.break_sound.as_str(), looped.roll.as_str()),
            ("A", "B")
        );
        let silent = SurfaceTable::parse("}{ nonsense").sounds("wood");
        assert_eq!(silent, SurfaceSounds::default());
    }
}
