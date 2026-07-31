//! Deterministic pseudo-random number generation for data generation.

/// A `splitmix64` generator.
///
/// Datagen determinism (validation contract `A-NNUE-015`) requires that a fixed seed
/// reproduces byte-identical output, so the generator must be explicit and seeded
/// rather than borrowed from the operating system. `splitmix64` is chosen because it
/// is a pure function of a single `u64` of state: a game's stream can be reconstructed
/// from its index alone, which is what lets games be generated out of order across
/// threads and still emit in a canonical order.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seeds the generator.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Derives an independent stream for `index` from a master `seed`.
    ///
    /// Seeding directly with `seed + index` would make consecutive games' first draws
    /// highly correlated, because `splitmix64` advances by a fixed increment. Mixing the
    /// index through one round first decorrelates them.
    pub fn for_index(seed: u64, index: u64) -> Self {
        let mut mixer = Self::new(seed);
        mixer.state = mixer.state.wrapping_add(index);
        let derived = mixer.next_u64();
        Self::new(derived)
    }

    /// Returns the next value in the stream.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut word = self.state;
        word = (word ^ (word >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        word = (word ^ (word >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        word ^ (word >> 31)
    }

    /// Returns a value in `0..bound`, or `None` when `bound` is zero.
    ///
    /// Uses Lemire's multiply-shift reduction. The modulo bias is bounded by
    /// `bound / 2^64` and is irrelevant at the bounds used here (legal move counts,
    /// never more than a few hundred), but the reduction is also faster than `%`.
    pub fn below(&mut self, bound: usize) -> Option<usize> {
        if bound == 0 {
            return None;
        }
        let draw = u128::from(self.next_u64());
        Some(((draw * bound as u128) >> 64) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn a_seed_reproduces_its_entire_stream() {
        let mut first = Rng::new(20_260_731);
        let mut second = Rng::new(20_260_731);
        for _ in 0..1024 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
    }

    #[test]
    fn distinct_seeds_produce_distinct_streams() {
        let mut first = Rng::new(1);
        let mut second = Rng::new(2);
        let differences = (0..64)
            .filter(|_| first.next_u64() != second.next_u64())
            .count();
        assert_eq!(differences, 64, "every draw should differ between seeds");
    }

    #[test]
    fn per_index_streams_are_independent_of_one_another() {
        let first: Vec<u64> = (0..8).map(|i| Rng::for_index(7, i).next_u64()).collect();
        let mut sorted = first.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), first.len(), "index streams must not collide");
    }

    #[test]
    fn below_stays_inside_its_bound_and_rejects_zero() {
        let mut rng = Rng::new(99);
        assert_eq!(rng.below(0), None);
        for _ in 0..4096 {
            let value = rng.below(37).expect("nonzero bound");
            assert!(value < 37);
        }
    }
}
