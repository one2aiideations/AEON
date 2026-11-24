//! Deterministic random numbers.

use crate::F;

/// xoshiro256** seeded via splitmix64. The same seed gives the same stream on one machine.
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut sm = seed;
        let mut next = || -> u64 {
            sm = sm.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = sm;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        };
        Rng {
            s: [next(), next(), next(), next()],
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = (self.s[1].wrapping_mul(5)).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> F {
        (self.next_u64() >> 11) as F / 9007199254740992.0
    }

    /// Standard normal (Box-Muller, one sample per call).
    pub fn normal(&mut self) -> F {
        let u1 = (1.0 - self.uniform()).max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Uniform in 0..n (0 when n is 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }

    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.below(i + 1);
            v.swap(i, j);
        }
    }

    /// Samples an index in proportion to the positive entries of `p` (0 when there are none).
    pub fn categorical(&mut self, p: &[F]) -> usize {
        let total: F = p.iter().copied().filter(|v| *v > 0.0).sum();
        if p.is_empty() || total <= 0.0 {
            return 0;
        }
        let mut r = self.uniform() * total;
        for (i, &pi) in p.iter().enumerate() {
            if pi > 0.0 {
                if r < pi {
                    return i;
                }
                r -= pi;
            }
        }
        p.len() - 1
    }
}
