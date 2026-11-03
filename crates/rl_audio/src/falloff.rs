//! How loud a sound is at a distance: Source's `SND_GetGainFromMult`.
//!
//! A sound's level (decibels at the reference distance) gives a distance
//! multiplier; the gain is the inverse of the distance times that
//! multiplier, so it halves with every doubling of distance, with a little
//! extra loss through the air (`snd_foliage_db_loss`, 4 dB per 1200
//! units). Near the source, where that gain would pass 0.5, a compressor
//! bends it smoothly up towards 1 instead (more gently for very loud
//! sounds), and far off, below 0.01, it falls the rest of the way to
//! silence linearly. Values are HL2DM's console defaults, read from its
//! `engine.so`.

/// `snd_refdist`: the distance (units) a sound level is measured at.
pub const REFERENCE_DISTANCE: f32 = 36.0;
/// `snd_refdb`: the sound level (decibels) heard at full volume at the
/// reference distance.
pub const REFERENCE_DB: f32 = 60.0;
/// `snd_foliage_db_loss`: decibels lost through the air per 1200 units.
pub const FOLIAGE_DB_LOSS: f32 = 4.0;
/// `snd_gain_max`: the loudest a compressed near sound gets.
pub const GAIN_MAX: f32 = 1.0;
/// `snd_gain_min`: below this gain a sound fades linearly to silence.
pub const GAIN_MIN: f32 = 0.01;
/// `snd_obscured_gain_dB`: the change in level of a sound behind a wall.
pub const OBSCURED_DB: f32 = -2.7;

/// Gain above which the compressor takes over.
const COMPRESSION_THRESHOLD: f32 = 0.5;
/// The compressor's exponent for sounds up to 90 dB...
const COMPRESSION_EXPONENT: f32 = 2.5;
/// ...falling this much per decibel above 90.
const COMPRESSION_EXPONENT_SLOPE: f32 = -0.034;
/// Level above which the compressor softens.
const LOUD_DB: f32 = 90.0;

/// The distance multiplier of a sound of `soundlevel` decibels
/// (`SNDLVL_TO_DIST_MULT`); zero for a sound heard at full volume
/// everywhere (`SNDLVL_NONE`).
#[must_use]
pub fn distance_multiplier(soundlevel: f32) -> f32 {
    if soundlevel <= 0.0 {
        return 0.0;
    }
    db_to_gain(REFERENCE_DB - soundlevel) / REFERENCE_DISTANCE
}

/// The sound level (whole decibels) of a distance multiplier
/// (`DIST_MULT_TO_SNDLVL`).
#[must_use]
pub fn soundlevel(distance_multiplier: f32) -> f32 {
    if distance_multiplier <= 0.0 {
        return 0.0;
    }
    (20.0 * (db_to_gain(REFERENCE_DB) / (distance_multiplier * REFERENCE_DISTANCE)).log10()).trunc()
}

/// The gain of a sound `distance` units away whose distance multiplier is
/// `multiplier`: 1 for a multiplier of zero.
#[must_use]
pub fn gain(distance: f32, multiplier: f32) -> f32 {
    if multiplier <= 0.0 {
        return 1.0;
    }
    let distance = distance.max(0.0);
    let air = db_to_gain(FOLIAGE_DB_LOSS * distance / 1200.0);
    let relative = distance * multiplier * air;
    let mut gain = if relative > 0.1 { 1.0 / relative } else { 10.0 };
    if gain > COMPRESSION_THRESHOLD {
        let level = soundlevel(multiplier);
        let exponent = if level > LOUD_DB {
            (level - LOUD_DB) * COMPRESSION_EXPONENT_SLOPE + COMPRESSION_EXPONENT
        } else {
            COMPRESSION_EXPONENT
        };
        // Meets the plain curve at the threshold with the same slope and
        // levels off at 1.
        let crossover = exponent.exp2();
        gain = (1.0 - 1.0 / (2.0 * crossover * gain.powf(exponent))) * GAIN_MAX;
    }
    if gain < GAIN_MIN {
        gain = GAIN_MIN * (2.0 - relative * GAIN_MIN);
        if gain <= 0.0 {
            gain = 0.001;
        }
    }
    gain
}

/// Amplitude of a level change of `db` decibels.
#[must_use]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const NORM: f32 = 75.0;

    #[test]
    fn a_normal_sound_halves_with_distance_and_is_full_close_by() {
        let multiplier = distance_multiplier(NORM);
        assert!((multiplier - 0.177_827_94 / 36.0).abs() < 1e-7);
        // Up close the compressor holds it just under full volume.
        assert!(gain(12.0, multiplier) > 0.99);
        assert!(gain(36.0, multiplier) > 0.98);
        // Then it falls off: louder near, quieter far.
        let near = gain(250.0, multiplier);
        let mid = gain(500.0, multiplier);
        let far = gain(1000.0, multiplier);
        assert!(near > mid && mid > far);
        assert!((0.3..0.4).contains(&mid), "{mid}");
        assert!((0.12..0.16).contains(&far), "{far}");
    }

    #[test]
    fn gunfire_carries_much_further_than_talk() {
        let gunfire = distance_multiplier(140.0);
        let talk = distance_multiplier(60.0);
        assert!(gain(3000.0, gunfire) > 0.8);
        assert!(gain(3000.0, talk) < 0.05);
        assert_eq!(soundlevel(gunfire), 140.0);
        assert_eq!(soundlevel(distance_multiplier(NORM)), 75.0);
    }

    #[test]
    fn a_sound_without_a_level_is_heard_everywhere() {
        assert_eq!(distance_multiplier(0.0), 0.0);
        assert_eq!(gain(1e6, 0.0), 1.0);
    }

    #[test]
    fn far_sounds_fade_to_silence_but_never_zero() {
        let multiplier = distance_multiplier(60.0);
        assert!(gain(30_000.0, multiplier) < GAIN_MIN);
        assert_eq!(gain(1e9, multiplier), 0.001);
    }

    proptest! {
        #[test]
        fn gain_never_rises_with_distance(
            level in 40.0f32..160.0,
            near in 0.0f32..20_000.0,
            step in 0.0f32..5_000.0,
        ) {
            let multiplier = distance_multiplier(level);
            let (a, b) = (gain(near, multiplier), gain(near + step, multiplier));
            // Source floors a vanishing gain at 0.001 rather than 0.
            prop_assert!(b <= a.max(0.001) + 1e-6, "{a} then {b}");
            prop_assert!((0.0..=GAIN_MAX).contains(&a));
        }
    }
}
