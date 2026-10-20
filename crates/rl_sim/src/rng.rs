//! Deterministic randomness: bullet spread and spawn choice.
//!
//! Anything random in the simulation must come out the same on the server
//! and on a client predicting it, so it is never drawn from a global source.
//! Instead each use seeds a [`Rng`] from what both sides know — the tick,
//! the player, the shot — and draws from that.

/// A small, fast pseudo-random generator (`SplitMix64`), seeded explicitly.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator seeded from `parts`, mixed so that nearby seeds (tick
    /// 5 and tick 6) give unrelated sequences.
    #[must_use]
    pub fn seeded(parts: &[u64]) -> Self {
        let mut rng = Self(0x9e37_79b9_7f4a_7c15);
        for &part in parts {
            rng.0 ^= part;
            rng.next_u64();
        }
        rng
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number uniformly in `0..1`.
    pub fn unit(&mut self) -> f32 {
        // The top 24 bits fill an f32 mantissa exactly.
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// An index uniformly in `0..len` (0 for an empty range).
    pub fn below(&mut self, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        (self.next_u64() % len as u64) as usize
    }

    /// A point uniformly inside the unit disc.
    pub fn in_disc(&mut self) -> (f32, f32) {
        let radius = self.unit().sqrt();
        let angle = self.unit() * std::f32::consts::TAU;
        (radius * angle.cos(), radius * angle.sin())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_sequence() {
        let mut a = Rng::seeded(&[7, 3]);
        let mut b = Rng::seeded(&[7, 3]);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(
            Rng::seeded(&[7, 3]).next_u64(),
            Rng::seeded(&[7, 4]).next_u64()
        );
    }

    #[test]
    fn draws_stay_in_range() {
        let mut rng = Rng::seeded(&[1]);
        for _ in 0..1000 {
            let unit = rng.unit();
            assert!((0.0..1.0).contains(&unit));
            assert!(rng.below(5) < 5);
            let (x, y) = rng.in_disc();
            assert!(x * x + y * y <= 1.0 + 1e-6);
        }
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn units_are_spread_over_the_range() {
        let mut rng = Rng::seeded(&[42]);
        let mut buckets = [0u32; 10];
        for _ in 0..10_000 {
            buckets[(rng.unit() * 10.0) as usize] += 1;
        }
        assert!(buckets.iter().all(|&count| (800..1200).contains(&count)));
    }
}
