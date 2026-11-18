//! Tiny deterministic PRNG (xorshift64*). Same seed -> same world.

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish number in `0..n` (n must be > 0).
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64)) as usize
    }
}
