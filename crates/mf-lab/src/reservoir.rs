/// Deterministic SplitMix64 generator local to the experiment.
#[derive(Clone, Copy, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    pub fn index(&mut self, upper: u64) -> u64 {
        debug_assert!(upper > 0);
        ((u128::from(self.next_u64()) * u128::from(upper)) >> 64) as u64
    }
}

/// Fixed-capacity Algorithm R reservoir.
pub struct Reservoir<T> {
    capacity: usize,
    seen: u64,
    rng: SplitMix64,
    items: Vec<T>,
}

impl<T> Reservoir<T> {
    pub fn new(capacity: usize, seed: u64) -> Self {
        Self {
            capacity,
            seen: 0,
            rng: SplitMix64::new(seed),
            items: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, item: T) {
        self.seen += 1;
        if self.items.len() < self.capacity {
            self.items.push(item);
        } else if self.capacity > 0 {
            let index = self.rng.index(self.seen);
            if index < self.capacity as u64 {
                self.items[index as usize] = item;
            }
        }
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    pub const fn seen(&self) -> u64 {
        self.seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservoir_is_reproducible_and_never_exceeds_capacity() {
        let mut first = Reservoir::new(5, 123);
        let mut second = Reservoir::new(5, 123);
        for value in 0..100 {
            first.push(value);
            second.push(value);
        }

        assert_eq!(first.items(), second.items());
        assert_eq!(first.items().len(), 5);
        assert_eq!(first.seen(), 100);
    }

    #[test]
    fn zero_capacity_reservoir_stores_nothing() {
        let mut reservoir = Reservoir::new(0, 1);
        reservoir.push(42);
        assert!(reservoir.items().is_empty());
        assert_eq!(reservoir.seen(), 1);
    }
}
