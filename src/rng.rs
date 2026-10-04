//! Deterministic random numbers.
//!
//! The simulator must produce bit-identical output for the same seed on any
//! machine, so it does not use the platform's `ln`/`exp` (libm results may
//! differ in the last bit between macOS and glibc). Everything here uses
//! only IEEE-754 `+ - * /`, which are exactly specified.

/// xoshiro256** (Blackman & Vigna, 2018), seeded through SplitMix64.
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

/// SplitMix64 finaliser; also used as the ECMP hash mixer.
#[inline]
pub fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut x = seed;
        let mut s = [0u64; 4];
        for v in &mut s {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            *v = mix64(x);
        }
        if s == [0; 4] {
            s[0] = 1;
        }
        Rng { s }
    }

    /// An independent stream derived from `seed` and a stream label.
    pub fn stream(seed: u64, label: u64) -> Self {
        Rng::new(mix64(
            seed ^ mix64(label.wrapping_mul(0xD6E8_FEB8_6659_FD93)),
        ))
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform in [0, 1) with 53 random bits.
    #[inline]
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform integer in [0, n), unbiased (rejection sampling).
    pub fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0);
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let v = self.next_u64();
            if v < zone {
                return v % n;
            }
        }
    }

    /// Exponential with the given mean (inverse transform).
    pub fn exponential(&mut self, mean: f64) -> f64 {
        // 1 - U is in (0, 1], so the log is finite.
        -det_ln(1.0 - self.uniform()) * mean
    }
}

/// Natural logarithm using only exactly-rounded IEEE operations, so the
/// result is identical on every platform. Accurate to a few ulps for normal
/// positive inputs, which is far more than a traffic generator needs.
pub fn det_ln(x: f64) -> f64 {
    assert!(x > 0.0 && x.is_finite(), "det_ln domain: {x}");
    const LN2: f64 = std::f64::consts::LN_2;
    let bits = x.to_bits();
    let mut exp = ((bits >> 52) & 0x7ff) as i64;
    let mut mant_bits = bits & ((1u64 << 52) - 1);
    if exp == 0 {
        // Subnormal: normalise.
        let shift = mant_bits.leading_zeros() as i64 - 11;
        mant_bits = (mant_bits << shift) & ((1u64 << 52) - 1);
        exp = 1 - shift;
    }
    let mut e = exp - 1023;
    // m in [1, 2)
    let mut m = f64::from_bits(mant_bits | (1023u64 << 52));
    // Move m into [sqrt(1/2), sqrt(2)) so the series argument stays small.
    if m > std::f64::consts::SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    // ln(m) = 2 * atanh(s), s = (m - 1) / (m + 1), |s| < 0.172
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut term = s;
    let mut sum = 0.0;
    let mut k = 1.0;
    for _ in 0..14 {
        sum += term / k;
        term *= s2;
        k += 2.0;
    }
    2.0 * sum + e as f64 * LN2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = Rng::new(43);
        assert_ne!(Rng::new(42).next_u64(), c.next_u64());
    }

    #[test]
    fn known_first_values() {
        // Pinned so that any accidental change to the generator is caught:
        // every experiment's output depends on this sequence.
        let mut r = Rng::new(1);
        let v: Vec<u64> = (0..3).map(|_| r.next_u64()).collect();
        assert_eq!(v, KNOWN);
    }
    const KNOWN: [u64; 3] = [
        18001451845637162709,
        15091038358276433251,
        7540168338507695658,
    ];

    #[test]
    fn matches_reference_xoshiro256starstar() {
        // Reference outputs of xoshiro256** for state {1, 2, 3, 4}
        // (prng.di.unimi.it reference implementation).
        let mut r = Rng { s: [1, 2, 3, 4] };
        let v: Vec<u64> = (0..4).map(|_| r.next_u64()).collect();
        assert_eq!(v, [11520, 0, 1509978240, 1215971899390074240]);
    }

    #[test]
    fn det_ln_matches_libm_closely() {
        let mut r = Rng::new(7);
        for _ in 0..100_000 {
            let x = r.uniform() * 1e6 + 1e-300;
            let d = det_ln(x);
            let l = x.ln();
            assert!(
                (d - l).abs() <= 4.0 * f64::EPSILON * l.abs().max(1.0),
                "x={x} det={d} libm={l}"
            );
        }
        assert_eq!(det_ln(1.0), 0.0);
        assert!((det_ln(f64::MIN_POSITIVE / 8.0) - (f64::MIN_POSITIVE / 8.0).ln()).abs() < 1e-12);
    }

    #[test]
    fn uniform_and_below_ranges() {
        let mut r = Rng::new(3);
        let mut counts = [0u32; 10];
        for _ in 0..100_000 {
            let u = r.uniform();
            assert!((0.0..1.0).contains(&u));
            counts[r.below(10) as usize] += 1;
        }
        for c in counts {
            assert!((9_500..10_500).contains(&c), "{counts:?}");
        }
    }

    #[test]
    fn exponential_mean() {
        let mut r = Rng::new(11);
        let n = 200_000;
        let mean: f64 = (0..n).map(|_| r.exponential(5.0)).sum::<f64>() / n as f64;
        assert!((mean - 5.0).abs() < 0.05, "mean {mean}");
    }
}
