//! Small deterministic PRNG (SplitMix64). Self-contained so generated files
//! are byte-identical across platforms, Rust versions and dependency updates.

#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    /// Independent stream for `(seed, index)`, so blocks can be generated in
    /// parallel and still be identical to a sequential run.
    pub fn stream(seed: u64, index: u64) -> Self {
        let mut r = Rng(seed ^ index.wrapping_mul(0xA076_1D64_78BD_642F));
        r.u64();
        r
    }

    pub fn u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (n > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        ((self.u64() as u128 * n as u128) >> 64) as u64
    }

    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    pub fn chance(&mut self, num: u64, den: u64) -> bool {
        self.below(den) < num
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }

    /// Pick with integer weights.
    pub fn weighted<'a, T>(&mut self, items: &'a [(T, u32)]) -> &'a T {
        let total: u64 = items.iter().map(|(_, w)| *w as u64).sum();
        let mut x = self.below(total);
        for (it, w) in items {
            if x < *w as u64 {
                return it;
            }
            x -= *w as u64;
        }
        &items[items.len() - 1].0
    }
}
