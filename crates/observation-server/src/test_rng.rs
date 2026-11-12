//! A small seeded generator for the property tests: the same inputs on
//! every run, so a failure reproduces, and no dependency.

/// xorshift64*.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub(crate) fn u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub(crate) fn u128(&mut self) -> u128 {
        (u128::from(self.u64()) << 64) | u128::from(self.u64())
    }

    /// Uniform in `0..n` (`n > 0`).
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        self.u64() % n
    }

    /// Uniform in `[lo, hi)`.
    pub(crate) fn f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    pub(crate) fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, self.below(i as u64 + 1) as usize);
        }
    }
}
