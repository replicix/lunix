//! A small deterministic generator (splitmix64) and the draws the model needs.

/// A seeded generator; the same seed gives the same run.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    /// A generator from a seed.
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `1 +- spread`, uniform.
    pub fn jitter(&mut self, spread: f64) -> f64 {
        1.0 + spread * (2.0 * self.unit() - 1.0)
    }

    /// A standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Binomial(n, p): exact for small `n p (1 - p)`, a rounded normal beyond.
    pub fn binomial(&mut self, n: f64, p: f64) -> f64 {
        if n <= 0.0 || p <= 0.0 {
            return 0.0;
        }
        if p >= 1.0 {
            return n;
        }
        let var = n * p * (1.0 - p);
        if var > 25.0 {
            (n * p + var.sqrt() * self.normal()).round().clamp(0.0, n)
        } else if n <= 4096.0 {
            let mut k = 0.0;
            let mut i = 0.0;
            while i < n {
                if self.unit() < p {
                    k += 1.0;
                }
                i += 1.0;
            }
            k
        } else {
            // Poisson limit by inversion, for large n and tiny p.
            let l = (-(n * p)).exp();
            let (mut k, mut t) = (0.0, self.unit());
            let mut term = l;
            let mut cum = l;
            while t > cum && k < n {
                k += 1.0;
                term *= n * p / k;
                cum += term;
                if term < 1e-18 {
                    break;
                }
                t = t.min(1.0);
            }
            k
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_draws() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(7).next_u64(), Rng::new(8).next_u64());
    }

    #[test]
    fn binomial_has_the_right_mean() {
        let mut r = Rng::new(3);
        for &(n, p) in &[(500.0, 0.3), (50.0, 0.1), (100_000.0, 1e-5), (256.0, 0.9)] {
            let m: f64 = (0..2000).map(|_| r.binomial(n, p)).sum::<f64>() / 2000.0;
            assert!((m - n * p).abs() < 0.1 * n * p + 0.3, "{n} {p} {m}");
        }
    }
}
