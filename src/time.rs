//! Simulated time.
//!
//! Time is an integer number of picoseconds. At 400 Gbit/s one byte takes
//! exactly 20 ps, at 100 Gbit/s 80 ps and at 25 Gbit/s 320 ps, so every
//! serialization delay in the paper's topologies is an exact integer and no
//! rounding error can accumulate along a path. A `u64` of picoseconds covers
//! about 213 days of simulated time.

/// Simulated time in picoseconds.
pub type Time = u64;

pub const PS: Time = 1;
pub const NS: Time = 1_000;
pub const US: Time = 1_000_000;
pub const MS: Time = 1_000_000_000;
pub const SEC: Time = 1_000_000_000_000;

/// Bits per second for a link of `gbps` Gbit/s.
pub const fn gbps(g: u64) -> u64 {
    g * 1_000_000_000
}

/// Time to put `bytes` on a wire of `rate_bps`, rounded up to the next
/// picosecond (rounding up never lets a sender exceed its rate).
#[inline]
pub fn ser_time(bytes: u64, rate_bps: u64) -> Time {
    debug_assert!(rate_bps > 0);
    let num = bytes as u128 * 8 * SEC as u128;
    num.div_ceil(rate_bps as u128) as Time
}

/// If `rate_bps` divides 8e12, one byte takes an integer number of
/// picoseconds and serialization is a single multiplication.
pub fn exact_ps_per_byte(rate_bps: u64) -> Option<u64> {
    let num = 8 * SEC;
    if rate_bps > 0 && num.is_multiple_of(rate_bps) {
        Some(num / rate_bps)
    } else {
        None
    }
}

/// Converts picoseconds to (fractional) microseconds for reports.
pub fn to_us(t: Time) -> f64 {
    t as f64 / US as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialization_is_exact_for_paper_rates() {
        assert_eq!(ser_time(1000, gbps(100)), 80 * NS);
        assert_eq!(ser_time(1048, gbps(100)), 83_840);
        assert_eq!(ser_time(1048, gbps(400)), 20_960);
        assert_eq!(ser_time(1048, gbps(25)), 335_360);
        assert_eq!(exact_ps_per_byte(gbps(100)), Some(80));
        assert_eq!(exact_ps_per_byte(gbps(400)), Some(20));
        assert_eq!(exact_ps_per_byte(gbps(25)), Some(320));
    }

    #[test]
    fn serialization_rounds_up() {
        // 1 byte at 3 bit/s is 8/3 s; rounding up gives 2666666666667 ps.
        assert_eq!(ser_time(1, 3), 2_666_666_666_667);
        assert_eq!(exact_ps_per_byte(3), None);
    }
}
