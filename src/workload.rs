//! Traffic: flow-size distributions, Poisson arrivals and incast.

use crate::rng::Rng;
use crate::time::{Time, SEC};

/// A piecewise-linear flow-size CDF: points `(bytes, percentile)`.
#[derive(Clone, Debug)]
pub struct SizeCdf {
    pub name: &'static str,
    pub points: &'static [(f64, f64)],
}

/// WebSearch flow sizes, from the DCTCP paper (Alizadeh et al., SIGCOMM 2010,
/// Figure 4), as tabulated in the HPCC artifact's
/// `traffic_gen/WebSearch_distribution.txt`. Mean 1.71 MB.
pub const WEB_SEARCH: SizeCdf = SizeCdf {
    name: "WebSearch",
    points: &[
        (0.0, 0.0),
        (10_000.0, 15.0),
        (20_000.0, 20.0),
        (30_000.0, 30.0),
        (50_000.0, 40.0),
        (80_000.0, 53.0),
        (200_000.0, 60.0),
        (1_000_000.0, 70.0),
        (2_000_000.0, 80.0),
        (5_000_000.0, 90.0),
        (10_000_000.0, 97.0),
        (30_000_000.0, 100.0),
    ],
};

/// FB_Hadoop flow sizes, from Roy et al., "Inside the Social Network's
/// (Datacenter) Network", SIGCOMM 2015 (Hadoop cluster), as tabulated in the
/// HPCC artifact's `traffic_gen/FbHdp_distribution.txt`. 90% of flows are
/// shorter than 120 KB (HPCC paper §5.3).
pub const FB_HADOOP: SizeCdf = SizeCdf {
    name: "FB_Hadoop",
    points: &[
        (0.0, 0.0),
        (100.0, 1.0),
        (200.0, 2.0),
        (300.0, 5.0),
        (350.0, 15.0),
        (400.0, 20.0),
        (500.0, 30.0),
        (600.0, 40.0),
        (700.0, 50.0),
        (1_000.0, 60.0),
        (2_000.0, 67.0),
        (7_000.0, 70.0),
        (30_000.0, 72.0),
        (50_000.0, 82.0),
        (80_000.0, 87.0),
        (120_000.0, 90.0),
        (300_000.0, 95.0),
        (1_000_000.0, 97.5),
        (2_000_000.0, 99.0),
        (10_000_000.0, 100.0),
    ],
};

impl SizeCdf {
    pub fn by_name(name: &str) -> Option<SizeCdf> {
        match name.to_ascii_lowercase().as_str() {
            "websearch" | "web_search" => Some(WEB_SEARCH),
            "fbhadoop" | "fb_hadoop" | "hadoop" => Some(FB_HADOOP),
            _ => None,
        }
    }

    /// Mean of the piecewise-linear distribution (trapezoid rule, exact).
    pub fn mean(&self) -> f64 {
        self.points
            .windows(2)
            .map(|w| (w[0].0 + w[1].0) / 2.0 * (w[1].1 - w[0].1))
            .sum::<f64>()
            / 100.0
    }

    /// Inverse CDF with linear interpolation (same sampling as the HPCC
    /// artifact's `custom_rand.py`), never below 1 byte.
    pub fn quantile(&self, pct: f64) -> u64 {
        for w in self.points.windows(2) {
            let ((x0, y0), (x1, y1)) = (w[0], w[1]);
            if pct <= y1 {
                let x = x0 + (x1 - x0) / (y1 - y0) * (pct - y0);
                return (x as u64).max(1);
            }
        }
        self.points.last().unwrap().0 as u64
    }

    pub fn sample(&self, rng: &mut Rng) -> u64 {
        self.quantile(rng.uniform() * 100.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowTag {
    Background,
    Incast,
    /// Hand-placed flows of micro-benchmarks.
    Micro,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSpec {
    pub src: u32,
    pub dst: u32,
    pub size: u64,
    pub start: Time,
    pub tag: FlowTag,
}

/// Poisson flow arrivals at every host (as the HPCC artifact's
/// `traffic_gen.py`): each host starts flows with exponential inter-arrival
/// times whose mean makes its offered load `load x host_rate`; destinations
/// are uniform over the other hosts.
pub fn poisson(hosts: u32, cdf: &SizeCdf, load: f64, host_rate_bps: u64, duration: Time, rng: &mut Rng) -> Vec<FlowSpec> {
    assert!(hosts >= 2 && load > 0.0);
    let mean_gap_s = cdf.mean() * 8.0 / (host_rate_bps as f64 * load);
    let mut flows = Vec::new();
    for src in 0..hosts {
        let mut t = rng.exponential(mean_gap_s);
        loop {
            let start = (t * SEC as f64) as Time;
            if start >= duration {
                break;
            }
            let mut dst = rng.below(hosts as u64 - 1) as u32;
            if dst >= src {
                dst += 1;
            }
            flows.push(FlowSpec { src, dst, size: cdf.sample(rng), start, tag: FlowTag::Background });
            t += rng.exponential(mean_gap_s);
        }
    }
    flows.sort_by_key(|f| (f.start, f.src));
    flows
}

/// Incast events (HPCC paper §5.3): `fan_in` random senders each send
/// `size` bytes to one random receiver at the same instant. Events arrive as
/// a Poisson process whose byte rate is `load_fraction` of the total host
/// capacity (`hosts x host_rate`).
#[allow(clippy::too_many_arguments)]
pub fn incast(
    hosts: u32,
    fan_in: u32,
    size: u64,
    load_fraction: f64,
    host_rate_bps: u64,
    duration: Time,
    rng: &mut Rng,
) -> Vec<FlowSpec> {
    assert!(fan_in < hosts);
    let event_bytes = fan_in as f64 * size as f64;
    let capacity_bytes_per_s = hosts as f64 * host_rate_bps as f64 / 8.0;
    let mean_gap_s = event_bytes / (capacity_bytes_per_s * load_fraction);
    let mut flows = Vec::new();
    let mut t = rng.exponential(mean_gap_s);
    loop {
        let start = (t * SEC as f64) as Time;
        if start >= duration {
            break;
        }
        let dst = rng.below(hosts as u64) as u32;
        // Choose fan_in distinct senders other than dst (partial Fisher-Yates).
        let mut cand: Vec<u32> = (0..hosts).filter(|&h| h != dst).collect();
        for i in 0..fan_in as usize {
            let j = i + rng.below((cand.len() - i) as u64) as usize;
            cand.swap(i, j);
            flows.push(FlowSpec { src: cand[i], dst, size, start, tag: FlowTag::Incast });
        }
        t += rng.exponential(mean_gap_s);
    }
    flows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{gbps, MS};

    #[test]
    fn cdfs_are_valid() {
        for c in [WEB_SEARCH, FB_HADOOP] {
            assert_eq!(c.points[0].1, 0.0);
            assert_eq!(c.points.last().unwrap().1, 100.0);
            for w in c.points.windows(2) {
                assert!(w[1].0 > w[0].0 && w[1].1 > w[0].1, "{} not increasing", c.name);
            }
        }
    }

    #[test]
    fn means_match_hand_calculation() {
        // Trapezoid means of the tabulated CDFs.
        assert!((WEB_SEARCH.mean() - 1_711_250.0).abs() < 1.0, "{}", WEB_SEARCH.mean());
        assert!((FB_HADOOP.mean() - 120_420.75).abs() < 1.0, "{}", FB_HADOOP.mean());
    }

    #[test]
    fn hadoop_quantiles() {
        assert_eq!(FB_HADOOP.quantile(90.0), 120_000);
        assert_eq!(FB_HADOOP.quantile(50.0), 700);
        assert_eq!(FB_HADOOP.quantile(0.0), 1);
        let mut r = Rng::new(5);
        let n = 200_000;
        let short = (0..n).filter(|_| FB_HADOOP.sample(&mut r) <= 120_000).count();
        assert!((short as f64 / n as f64 - 0.90).abs() < 0.005);
    }

    #[test]
    fn poisson_offered_load_matches_target() {
        let mut r = Rng::new(9);
        let dur = 200 * MS;
        let flows = poisson(16, &FB_HADOOP, 0.5, gbps(100), dur, &mut r);
        let bytes: u64 = flows.iter().map(|f| f.size).sum();
        let offered = bytes as f64 * 8.0 / (dur as f64 / SEC as f64) / (16.0 * 100e9);
        assert!((offered - 0.5).abs() < 0.03, "offered load {offered}");
        assert!(flows.iter().all(|f| f.src != f.dst && f.start < dur));
        assert!(flows.windows(2).all(|w| w[0].start <= w[1].start));
    }

    #[test]
    fn incast_shape_and_load() {
        let mut r = Rng::new(10);
        let dur = 100 * MS;
        let flows = incast(320, 60, 500_000, 0.02, gbps(100), dur, &mut r);
        assert_eq!(flows.len() % 60, 0);
        for ev in flows.chunks(60) {
            let mut srcs: Vec<u32> = ev.iter().map(|f| f.src).collect();
            srcs.sort();
            srcs.dedup();
            assert_eq!(srcs.len(), 60, "distinct senders");
            assert!(ev.iter().all(|f| f.dst == ev[0].dst && f.src != f.dst && f.start == ev[0].start));
        }
        let bytes: u64 = flows.iter().map(|f| f.size).sum();
        let frac = bytes as f64 * 8.0 / 0.1 / (320.0 * 100e9);
        assert!((frac - 0.02).abs() < 0.004, "incast load {frac}");
    }
}
