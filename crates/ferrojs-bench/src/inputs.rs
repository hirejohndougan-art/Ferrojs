//! Deterministic benchmark inputs. No randomness from the host: the same
//! inputs every run, so instruction counts only change when code does.

/// A small xorshift generator with a fixed seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// How many values each value-encoding benchmark processes.
pub const N: usize = 1024;

/// A mix of doubles a JS program sees: small integers, fractions, large
/// values, -0, NaN and infinities.
pub fn numbers() -> Vec<f64> {
    let mut rng = Rng::new(0x5EED);
    (0..N)
        .map(|i| match i % 8 {
            0 => (rng.next_u64() % 1000) as f64,
            1 => -((rng.next_u64() % 100_000) as f64),
            2 => (rng.next_u64() % 10_000) as f64 / 7.0,
            3 => f64::from_bits(rng.next_u64() >> 2),
            4 => -0.0,
            5 => f64::NAN,
            6 => f64::INFINITY,
            _ => 4_294_967_296.0 + i as f64,
        })
        .collect()
}

/// Raw 64-bit patterns: about half are valid encodings of every kind, the
/// rest are random and mostly rejected.
pub fn bit_patterns() -> Vec<u64> {
    let mut rng = Rng::new(0xB175);
    (0..N)
        .map(|i| {
            let r = rng.next_u64();
            match i % 4 {
                0 => r,
                1 => (0xFFF9 + (r % 7)) << 48 | (r & 0xFFFF_FFFF),
                2 => f64::to_bits((r % 1_000_000) as f64 * 0.25),
                _ => (0xFFFC << 48) | (r & 0xFFFF_FFFF),
            }
        })
        .collect()
}
