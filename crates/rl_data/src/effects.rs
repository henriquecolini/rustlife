//! `effects.ron`: what shots and blasts look like.
//!
//! Effects are drawn with the game's own sprite materials (named relative
//! to `materials/`, without extension), on quads facing the camera: muzzle
//! flashes, bullet tracers, the debris and decals where bullets strike, and
//! explosions, whose sprites may be animated, and what a thrown grenade
//! shows: a red dot flashing with its ticking and a red trail behind it.
//!
//! What a bullet throws off a surface is one of the game's code effects
//! (`PerformCustomEffects` in `client.so`), chosen by the surface's
//! material: flecks and dust for concrete, tile and wood, a dust cloud for
//! dirt and sand, sparks for metal. Their numbers are in
//! [`ImpactParticlesConfig`].

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
    pub bullet_impacts: BTreeMap<String, SurfaceImpactConfig>,
    /// The numbers of the code effects `bullet_impacts` choose from.
    pub impact_particles: ImpactParticlesConfig,
    /// A bullet striking a player: blood and the stain it leaves.
    pub blood: ImpactEffectConfig,
    /// The marks blasts leave.
    pub scorch: ScorchConfig,
    /// A blast.
    pub explosion: ExplosionEffectConfig,
    /// A grenade in flight.
    pub grenade: GrenadeEffectConfig,
    /// What a weapon adds where its bullets strike, by weapon id: the game's
    /// own weapon code dispatches an extra effect beside the surface's
    /// (the Pulse Rifle's flash, for one).
    #[serde(default)]
    pub weapon_flashes: BTreeMap<String, ImpactFlashConfig>,
    /// A weapon's own muzzle flash, by weapon id, instead of `muzzle_flash`
    /// (the game gives each gun a flash of its own kind).
    #[serde(default)]
    pub weapon_muzzle_flashes: BTreeMap<String, MuzzleFlashConfig>,
    /// The Pulse Rifle's energy ball bouncing and bursting, if it is drawn
    /// as the game draws it.
    #[serde(default)]
    pub combine_ball: Option<CombineBallEffectConfig>,
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

/// A flash drawn flat on the surface a weapon's bullet strikes: the game's
/// `FX_AddQuad`. Its size and its opacity each run from a start to an end
/// over its life along a bias curve (`t^-log2(bias)`, so below `0.5` it
/// lingers and above it moves early).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactFlashConfig {
    /// The sprite (a material under `materials/`).
    pub sprite: String,
    /// The half-size at birth, picked between these two, units.
    pub size: (f32, f32),
    /// The half-size at death, units.
    pub end_size: f32,
    /// The size's bias curve, `0..1`.
    pub size_bias: f32,
    /// Opacity at birth and at death.
    pub alpha: (f32, f32),
    /// The opacity's bias curve, `0..1`.
    pub alpha_bias: f32,
    /// Seconds it lasts.
    pub life: f32,
}

impl ImpactFlashConfig {
    fn validate(&self, check: &mut Validator, field: &str) {
        if self.sprite.trim().is_empty() {
            check.fail(&format!("{field}.sprite"), "must name a sprite");
        }
        check.positive(&format!("{field}.size.0"), self.size.0);
        if self.size.1 < self.size.0 {
            check.fail(&format!("{field}.size"), "the range runs low to high");
        }
        check.non_negative(&format!("{field}.end_size"), self.end_size);
        check.range(&format!("{field}.alpha.0"), self.alpha.0, 0.0, 1.0);
        check.range(&format!("{field}.alpha.1"), self.alpha.1, 0.0, 1.0);
        for (name, bias) in [
            ("size_bias", self.size_bias),
            ("alpha_bias", self.alpha_bias),
        ] {
            if !(bias > 0.0 && bias < 1.0) {
                check.fail(&format!("{field}.{name}"), "must be between 0 and 1");
            }
        }
        check.positive(&format!("{field}.life"), self.life);
    }
}

/// What a bullet or swing leaves where it strikes a surface: the game's code
/// effect for the surface's material, and the mark.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceImpactConfig {
    /// What is thrown off it.
    pub effect: SurfaceEffect,
    /// The surface's own colour, linear RGB `0..=1`: what the game reads from
    /// the struck texture's reflectivity. The light falling on the spot
    /// darkens it (`GetColorForSurface`); debris and dust are tinted by it.
    pub color: [f32; 3],
    /// Decals to choose from; empty for a surface that keeps no mark. A
    /// decal is as large as its texture times its `$decalscale`.
    pub decals: Vec<String>,
    /// How long a decal stays, seconds.
    pub decal_life: f32,
}

impl SurfaceImpactConfig {
    fn validate(&self, check: &mut Validator, field: &str) {
        if let SurfaceEffect::Debris(flecks) = &self.effect {
            if flecks.is_empty() {
                check.fail(&format!("{field}.effect"), "debris needs fleck sprites");
            }
            for (index, sprite) in flecks.iter().enumerate() {
                if sprite.trim().is_empty() {
                    check.fail(&format!("{field}.effect[{index}]"), "must name a sprite");
                }
            }
        }
        for (axis, value) in self.color.iter().enumerate() {
            check.range(&format!("{field}.color[{axis}]"), *value, 0.0, 1.0);
        }
        for (index, decal) in self.decals.iter().enumerate() {
            if decal.trim().is_empty() {
                check.fail(&format!("{field}.decals[{index}]"), "must name a decal");
            }
        }
        if !self.decals.is_empty() {
            check.non_negative(&format!("{field}.decal_life"), self.decal_life);
        }
    }
}

/// The game's code effect for a struck surface (`PerformCustomEffects`, by
/// the surface's game material).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SurfaceEffect {
    /// Nothing is thrown off (glass, flesh, water).
    None,
    /// `FX_DebrisFlecks` (concrete, tile, wood): flecks of these sprites,
    /// and the `debris_dust` puffs.
    Debris(Vec<String>),
    /// `FX_DustImpact` (dirt, sand): the `dust` puffs.
    Dust,
    /// `FX_MetalSpark` (metal, vents): `sparks` and the `spark_glow`.
    Sparks,
}

/// The numbers of the game's code effects for struck surfaces, read from
/// `client.so` (`c_impact_effects.cpp`, `fx_sparks.cpp`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactParticlesConfig {
    /// `FX_DebrisFlecks`' flecks.
    pub flecks: FleckConfig,
    /// `FX_DebrisFlecks`' puffs of dust.
    pub debris_dust: Vec<PuffConfig>,
    /// `FX_DustImpact`'s puffs.
    pub dust: Vec<PuffConfig>,
    /// `FX_MetalSpark`'s sparks.
    pub sparks: SparkConfig,
    /// `FX_MetalSpark`'s glow lying on the surface.
    pub spark_glow: ImpactFlashConfig,
}

impl ImpactParticlesConfig {
    fn validate(&self, check: &mut Validator) {
        let field = "impact_particles";
        self.flecks.validate(check, &format!("{field}.flecks"));
        for (index, puff) in self.debris_dust.iter().enumerate() {
            puff.validate(check, &format!("{field}.debris_dust[{index}]"));
        }
        for (index, puff) in self.dust.iter().enumerate() {
            puff.validate(check, &format!("{field}.dust[{index}]"));
        }
        self.sparks.validate(check, &format!("{field}.sparks"));
        self.spark_glow
            .validate(check, &format!("{field}.spark_glow"));
    }
}

/// Requires `low <= high`, both at least zero.
fn span(check: &mut Validator, field: &str, (low, high): (f32, f32)) {
    check.non_negative(&format!("{field}.0"), low);
    if high < low || high.is_nan() {
        check.fail(field, "the range runs low to high");
    }
}

/// Flecks: chips that fly off a surface, fall, bounce off the map and fade
/// (`CFleckParticles` with a `CParticleCollision`). Units and seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleckConfig {
    /// How many, picked between these.
    pub count: (u32, u32),
    /// How far each fleck's direction strays from the surface's normal, on
    /// each axis.
    pub spread: f32,
    /// Its speed, picked between these, then times three less its size.
    pub speed: (f32, f32),
    /// Its half-size, a whole number picked between these.
    pub size: (u32, u32),
    /// Seconds it lasts, fading all the while.
    pub life: f32,
    /// How fast it falls, units/s².
    pub gravity: f32,
    /// The share of its speed a bounce keeps, give or take `bounce_jitter`.
    pub bounce: f32,
    /// How much `bounce` varies.
    pub bounce_jitter: f32,
    /// A fleck landing on a floor slower than this (vertically) lies still.
    pub rest_speed: f32,
    /// Its turn, radians/s, picked up to this.
    pub roll_speed: f32,
    /// What the surface's colour is multiplied by, picked between these.
    pub color_ramp: (f32, f32),
}

impl FleckConfig {
    fn validate(&self, check: &mut Validator, field: &str) {
        if self.count.1 < self.count.0 || self.size.1 < self.size.0 {
            check.fail(field, "count and size run low to high");
        }
        check.non_negative(&format!("{field}.spread"), self.spread);
        span(check, &format!("{field}.speed"), self.speed);
        check.positive(&format!("{field}.life"), self.life);
        check.non_negative(&format!("{field}.gravity"), self.gravity);
        check.range(&format!("{field}.bounce"), self.bounce, 0.0, 1.0);
        check.non_negative(&format!("{field}.bounce_jitter"), self.bounce_jitter);
        check.non_negative(&format!("{field}.rest_speed"), self.rest_speed);
        span(check, &format!("{field}.color_ramp"), self.color_ramp);
    }
}

/// A group of puffs: sprites that drift off a struck surface, growing and
/// fading (the game's `SimpleParticle`s). Units and seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PuffConfig {
    /// The sprite (a material under `materials/`).
    pub sprite: String,
    /// How many.
    pub count: u32,
    /// Seconds each lasts, picked between these.
    pub life: (f32, f32),
    /// Which way each goes.
    pub aim: PuffAim,
    /// Its speed, picked between these.
    pub speed: (f32, f32),
    /// What the speed of the first, second, ... puff is multiplied by, over
    /// and over; none for one.
    #[serde(default)]
    pub steps: Vec<f32>,
    /// What is done to its vertical speed.
    #[serde(default)]
    pub vertical: Vertical,
    /// Its half-size at birth, a whole number picked between these.
    pub size: (u32, u32),
    /// Whether the size is also multiplied by one more than its step.
    #[serde(default)]
    pub size_by_step: bool,
    /// Its half-size at death, as a multiple of that at birth.
    pub end_size: f32,
    /// Its opacity at birth, out of 255, picked between these; it fades to
    /// nothing. A `dust` puff ignores it.
    pub alpha: (u32, u32),
    /// Its turn, radians/s, picked between minus and plus this.
    pub roll_speed: f32,
    /// What the surface's colour is multiplied by, picked between these.
    pub color_ramp: (f32, f32),
    /// Whether it moves as the game's `CDustParticle`: braking to a ten
    /// thousandth of its speed each half second (but never below 32 units/s),
    /// its turn slowing too, and fading from full as `1 - t`, squared once
    /// a quarter of its life is gone.
    #[serde(default)]
    pub dust: bool,
}

impl PuffConfig {
    fn validate(&self, check: &mut Validator, field: &str) {
        if self.sprite.trim().is_empty() {
            check.fail(&format!("{field}.sprite"), "must name a sprite");
        }
        span(check, &format!("{field}.life"), self.life);
        check.positive(&format!("{field}.life.1"), self.life.1);
        span(check, &format!("{field}.speed"), self.speed);
        if self.size.1 < self.size.0 || self.alpha.1 < self.alpha.0 || self.alpha.1 > 255 {
            check.fail(field, "size and alpha run low to high, alpha up to 255");
        }
        check.non_negative(&format!("{field}.end_size"), self.end_size);
        span(check, &format!("{field}.color_ramp"), self.color_ramp);
    }
}

/// Which way a puff is thrown.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PuffAim {
    /// The surface's normal, each axis strayed by up to this.
    Scatter(f32),
    /// The surface's normal times a push picked between `push`, plus a stray
    /// of up to `scatter` on each axis, made one long.
    Pushed {
        /// The stray on each axis.
        scatter: f32,
        /// How far the normal is pushed out, picked between these.
        push: (f32, f32),
    },
}

/// What is done to a puff's vertical speed once it is aimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub enum Vertical {
    /// Nothing.
    #[default]
    Keep,
    /// Lowered by a speed picked between these, times its step.
    Sink(f32, f32),
    /// Set to a speed picked between these.
    Set(f32, f32),
}

/// Sparks: streaks thrown off metal (`CTrailParticles`). Units and seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SparkConfig {
    /// How many, picked between these, then doubled.
    pub count: (u32, u32),
    /// How far the bullet's reflection off the surface is strayed on each
    /// axis before the sparks are thrown along it.
    pub jitter: f32,
    /// Each spark's spread, picked between these: it strays its direction
    /// by up to half of it on each axis, and slows it.
    pub spread: (f32, f32),
    /// Its speed, picked between these, then times two less its spread.
    pub speed: (f32, f32),
    /// Seconds it lasts, picked between these.
    pub life: (f32, f32),
    /// Its width, picked between these.
    pub width: (f32, f32),
    /// Seconds of its flight its streak spans, picked between these.
    pub length: (f32, f32),
    /// How fast it falls, units/s².
    pub gravity: f32,
    /// The share of its speed it loses each second.
    pub drag: f32,
}

impl SparkConfig {
    fn validate(&self, check: &mut Validator, field: &str) {
        if self.count.1 < self.count.0 {
            check.fail(&format!("{field}.count"), "the range runs low to high");
        }
        check.non_negative(&format!("{field}.jitter"), self.jitter);
        for (name, range) in [
            ("spread", self.spread),
            ("speed", self.speed),
            ("life", self.life),
            ("width", self.width),
            ("length", self.length),
        ] {
            span(check, &format!("{field}.{name}"), range);
        }
        check.non_negative(&format!("{field}.gravity"), self.gravity);
        check.non_negative(&format!("{field}.drag"), self.drag);
    }
}

/// What a bullet throws out of a wound and the stain it leaves: spray
/// sprites flying out of it (when the game's particle system is missing),
/// and decals drawn flat on what lies behind.
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
    /// Decals to choose from; empty for none. A decal is as large as its
    /// texture times its `$decalscale`.
    pub decals: Vec<String>,
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
            check.non_negative(&format!("{field}.decal_life"), self.decal_life);
        }
    }
}

/// The marks blasts leave on what they burst against: the game's `Scorch`
/// and `SmallScorch` decals (`scripts/decals_subrect.txt`). Each is as large
/// as its texture times its `$decalscale`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScorchConfig {
    /// `Scorch`: the mark of most blasts.
    pub large: Vec<String>,
    /// `SmallScorch`: an SMG grenade's on anything but the map.
    pub small: Vec<String>,
    /// How long a scorch stays, seconds.
    pub life: f32,
}

impl ScorchConfig {
    fn validate(&self, check: &mut Validator) {
        for (field, decals) in [("scorch.large", &self.large), ("scorch.small", &self.small)] {
            if decals.is_empty() {
                check.fail(field, "needs at least one decal");
            }
            for (index, decal) in decals.iter().enumerate() {
                if decal.trim().is_empty() {
                    check.fail(&format!("{field}[{index}]"), "must name a decal");
                }
            }
        }
        check.positive("scorch.life", self.life);
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

/// What the Pulse Rifle's energy ball shows (HL2DM's `prop_combine_ball`).
///
/// Where it bounces the server dispatches `cball_bounce`, whose client code
/// lays two flashes flat on the surface and throws electric sparks off it;
/// where it bursts, `cball_explode` (sparks thrown up), two rings spreading
/// out level (`te->BeamRingPoint`) and a shake (`UTIL_ScreenShake`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombineBallEffectConfig {
    /// The flashes lying on the surface it bounces off (`FX_AddQuad`s).
    pub bounce_flashes: Vec<ImpactFlashConfig>,
    /// The electric sparks thrown off that surface (`FX_ElectricSpark`):
    /// their magnitude and trail length.
    pub bounce_sparks: (u32, u32),
    /// The electric sparks thrown up where it bursts, the same way.
    pub burst_sparks: (u32, u32),
    /// The rings that spread out where it bursts.
    pub rings: Vec<BeamRingConfig>,
    /// How hard the view shakes right at the burst, `0..=1`, on the scale
    /// of `explosion.shake` (a grenade's, HL2DM's amplitude 25).
    pub shake: f32,
    /// How far the shake reaches, fading to nothing, units.
    pub shake_radius: f32,
}

/// A ring of beam that spreads out level from a point and fades as it goes:
/// the game's `TE_BEAMRINGPOINT` with `FBEAM_FADEOUT`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeamRingConfig {
    /// The beam's sprite (a material under `materials/`).
    pub sprite: String,
    /// Its radius at birth and at death, units.
    pub radius: (f32, f32),
    /// The beam's width, units: the ribbon reaches this far either side of
    /// the circle, as the engine's `DrawRing` draws it.
    pub width: f32,
    /// Its colour (linear RGB, `0..=1`).
    pub color: [f32; 3],
    /// Its opacity at birth, `0..=1`; it fades to nothing over its life.
    pub alpha: f32,
    /// Seconds it lasts.
    pub life: f32,
    /// How many of the sprite's frames it steps through a second.
    pub framerate: f32,
}

impl CombineBallEffectConfig {
    fn validate(&self, check: &mut Validator) {
        for (index, flash) in self.bounce_flashes.iter().enumerate() {
            flash.validate(check, &format!("combine_ball.bounce_flashes[{index}]"));
        }
        for (index, ring) in self.rings.iter().enumerate() {
            let field = format!("combine_ball.rings[{index}]");
            if ring.sprite.trim().is_empty() {
                check.fail(&format!("{field}.sprite"), "must name a sprite");
            }
            check.non_negative(&format!("{field}.radius.0"), ring.radius.0);
            check.positive(&format!("{field}.radius.1"), ring.radius.1);
            check.positive(&format!("{field}.width"), ring.width);
            check.range(&format!("{field}.alpha"), ring.alpha, 0.0, 1.0);
            check.positive(&format!("{field}.life"), ring.life);
            check.non_negative(&format!("{field}.framerate"), ring.framerate);
        }
        check.range("combine_ball.shake", self.shake, 0.0, 1.0);
        check.positive("combine_ball.shake_radius", self.shake_radius);
    }
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
    pub fn bullet_impact(&self, class: &str) -> Option<&SurfaceImpactConfig> {
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
        self.impact_particles.validate(check);
        self.blood.validate(check, "blood");
        self.scorch.validate(check);
        for (weapon, flash) in &self.weapon_flashes {
            flash.validate(check, &format!("weapon_flashes.{weapon}"));
        }
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
        if let Some(ball) = &self.combine_ball {
            ball.validate(check);
        }
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
            decal_life: 20.0,
            pcf: None,
        }
    }

    fn surface() -> SurfaceImpactConfig {
        SurfaceImpactConfig {
            effect: SurfaceEffect::Debris(vec!["effects/fleck_cement1".to_owned()]),
            color: [0.4, 0.4, 0.38],
            decals: vec!["decals/concrete/shot1".to_owned()],
            decal_life: 30.0,
        }
    }

    fn puff() -> PuffConfig {
        PuffConfig {
            sprite: "particle/particle_smokegrenade".to_owned(),
            count: 2,
            life: (1.0, 1.0),
            aim: PuffAim::Scatter(0.8),
            speed: (2.0, 24.0),
            steps: vec![1.0, 2.0],
            vertical: Vertical::Sink(8.0, 32.0),
            size: (2, 4),
            size_by_step: false,
            end_size: 8.0,
            alpha: (100, 200),
            roll_speed: 1.0,
            color_ramp: (0.5, 1.25),
            dust: false,
        }
    }

    fn particles() -> ImpactParticlesConfig {
        ImpactParticlesConfig {
            flecks: FleckConfig {
                count: (4, 16),
                spread: 0.4,
                speed: (64.0, 128.0),
                size: (1, 2),
                life: 3.0,
                gravity: 800.0,
                bounce: 0.3,
                bounce_jitter: 0.1,
                rest_speed: 48.0,
                roll_speed: 360.0,
                color_ramp: (0.75, 1.25),
            },
            debris_dust: vec![puff()],
            dust: vec![puff()],
            sparks: SparkConfig {
                count: (4, 8),
                jitter: 0.2,
                spread: (0.0, 2.0),
                speed: (128.0, 512.0),
                life: (0.05, 0.1),
                width: (1.0, 4.0),
                length: (0.025, 0.1),
                gravity: 400.0,
                drag: 8.0,
            },
            spark_glow: flash(),
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
                ("default".to_owned(), surface()),
                ("metal".to_owned(), surface()),
            ]),
            impact_particles: particles(),
            blood: impact(),
            scorch: ScorchConfig {
                large: vec!["decals/scorch1".to_owned()],
                small: vec!["decals/smscorch1".to_owned()],
                life: 60.0,
            },
            explosion: ExplosionEffectConfig {
                shake: 0.6,
                pcf: None,
            },
            weapon_flashes: BTreeMap::new(),
            weapon_muzzle_flashes: BTreeMap::new(),
            combine_ball: None,
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

    fn flash() -> ImpactFlashConfig {
        ImpactFlashConfig {
            sprite: "effects/flash".to_owned(),
            size: (24.0, 32.0),
            end_size: 0.0,
            size_bias: 0.75,
            alpha: (1.0, 0.0),
            alpha_bias: 0.4,
            life: 0.25,
        }
    }

    #[test]
    fn a_weapon_flash_is_checked() {
        let mut config = config();
        config.weapon_flashes.insert("rifle".to_owned(), flash());
        let mut check = Validator::default();
        config.validate(&mut check);
        assert!(check.finish().is_empty());
        let mut bad = flash();
        bad.sprite.clear();
        bad.size = (30.0, 20.0);
        bad.size_bias = 1.0;
        bad.life = 0.0;
        config.weapon_flashes.insert("rifle".to_owned(), bad);
        let mut check = Validator::default();
        config.validate(&mut check);
        assert_eq!(check.finish().len(), 4);
    }

    #[test]
    fn the_energy_balls_flashes_and_rings_are_checked() {
        let ring = BeamRingConfig {
            sprite: "sprites/lgtning".to_owned(),
            radius: (10.0, 1024.0),
            width: 64.0,
            color: [1.0, 1.0, 0.88],
            alpha: 0.25,
            life: 0.5,
            framerate: 2.0,
        };
        let mut config = config();
        config.combine_ball = Some(CombineBallEffectConfig {
            bounce_flashes: vec![flash()],
            bounce_sparks: (2, 1),
            burst_sparks: (4, 1),
            rings: vec![ring.clone()],
            shake: 0.68,
            shake_radius: 1250.0,
        });
        let mut check = Validator::default();
        config.validate(&mut check);
        assert!(check.finish().is_empty());
        let mut bad_flash = flash();
        bad_flash.life = 0.0;
        config.combine_ball = Some(CombineBallEffectConfig {
            bounce_flashes: vec![bad_flash],
            bounce_sparks: (2, 1),
            burst_sparks: (4, 1),
            rings: vec![BeamRingConfig {
                sprite: String::new(),
                alpha: 2.0,
                life: 0.0,
                ..ring
            }],
            shake: 1.5,
            shake_radius: 0.0,
        });
        let mut check = Validator::default();
        config.validate(&mut check);
        assert_eq!(check.finish().len(), 6);
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
            .effect = SurfaceEffect::Debris(Vec::new());
        bad.explosion.shake = 2.0;
        bad.grenade.flash_fade = 0.0;
        bad.explosion.pcf = Some(" ".to_owned());
        bad.blood.pcf = Some(String::new());
        let mut check = Validator::default();
        bad.validate(&mut check);
        assert_eq!(check.finish().len(), 7);
    }

    #[test]
    fn the_code_effects_numbers_are_checked() {
        let mut bad = config();
        bad.impact_particles.flecks.count = (16, 4);
        bad.impact_particles.flecks.bounce = 2.0;
        bad.impact_particles.debris_dust[0].sprite.clear();
        bad.impact_particles.dust[0].alpha = (100, 300);
        bad.impact_particles.sparks.speed = (512.0, 128.0);
        bad.scorch.small.clear();
        bad.bullet_impacts.get_mut("metal").expect("metal").color = [1.5, 0.0, 0.0];
        let mut check = Validator::default();
        bad.validate(&mut check);
        let problems = check.finish();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields.len(), 7, "{fields:?}");
        assert!(fields.contains(&"scorch.small"));
        assert!(fields.contains(&"bullet_impacts.metal.color[0]"));
        assert!(fields.contains(&"impact_particles.sparks.speed"));
    }
}
