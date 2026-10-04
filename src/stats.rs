//! Measurement: flow completion times, histograms and counters.

use crate::time::Time;
use crate::workload::FlowTag;

/// Log-linear histogram for non-negative integers: values below 128 are
/// exact, larger values fall into buckets of relative width 1/128 (<0.8%).
#[derive(Clone, Debug)]
pub struct LogHist {
    counts: Vec<u64>,
    total: u64,
}

const SUB: u64 = 128;
const SUB_BITS: u32 = 7;

impl Default for LogHist {
    fn default() -> Self {
        LogHist {
            counts: vec![0; (SUB as usize) * 60],
            total: 0,
        }
    }
}

impl LogHist {
    #[inline]
    pub fn index(v: u64) -> usize {
        if v < SUB {
            v as usize
        } else {
            let e = 63 - v.leading_zeros(); // >= 7
            let sub = (v >> (e - SUB_BITS)) & (SUB - 1);
            (SUB + (e - SUB_BITS) as u64 * SUB + sub) as usize
        }
    }

    /// Smallest value that maps to bucket `i`.
    pub fn bucket_low(i: usize) -> u64 {
        let i = i as u64;
        if i < SUB {
            i
        } else {
            let e = (i - SUB) / SUB + SUB_BITS as u64;
            let sub = (i - SUB) % SUB;
            (1u64 << e) | (sub << (e - SUB_BITS as u64))
        }
    }

    #[inline]
    pub fn add(&mut self, v: u64) {
        self.add_n(v, 1);
    }

    #[inline]
    pub fn add_n(&mut self, v: u64, n: u64) {
        let i = Self::index(v);
        if i >= self.counts.len() {
            self.counts.resize(i + 1, 0);
        }
        self.counts[i] += n;
        self.total += n;
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// The `p`-quantile (0..=1), reported as the bucket's lower bound.
    pub fn quantile(&self, p: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let target = ((p * self.total as f64).ceil() as u64).clamp(1, self.total);
        let mut acc = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            acc += c;
            if acc >= target {
                return Self::bucket_low(i);
            }
        }
        unreachable!()
    }

    /// Non-empty buckets as (lower bound, count).
    pub fn buckets(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.counts
            .iter()
            .enumerate()
            .filter(|(_, c)| **c > 0)
            .map(|(i, c)| (Self::bucket_low(i), *c))
    }

    pub fn merge(&mut self, o: &LogHist) {
        for (i, c) in o.counts.iter().enumerate() {
            if *c > 0 {
                if i >= self.counts.len() {
                    self.counts.resize(i + 1, 0);
                }
                self.counts[i] += c;
            }
        }
        self.total += o.total;
    }
}

#[derive(Clone, Debug)]
pub struct FctRecord {
    pub flow: u32,
    pub src: u32,
    pub dst: u32,
    pub size: u64,
    pub start: Time,
    pub fct: Time,
    /// FCT of the same flow alone in the network at line rate (exact).
    pub ideal: Time,
    pub tag: FlowTag,
}

impl FctRecord {
    pub fn slowdown(&self) -> f64 {
        (self.fct as f64 / self.ideal as f64).max(1.0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Counters {
    /// Wire bytes of data packets put on host links (including retransmissions).
    pub data_bytes_injected: u64,
    pub data_pkts_injected: u64,
    /// Wire bytes of data packets that reached their destination host.
    pub data_bytes_received: u64,
    pub drops: u64,
    pub dropped_bytes: u64,
    /// ACK/NACK packets dropped (only possible when ACKs share the data
    /// class and PFC is off).
    pub ack_drops: u64,
    pub ecn_marks: u64,
    pub pause_frames: u64,
    pub resume_frames: u64,
    pub retransmit_events: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_bounds_roundtrip() {
        let mut v = 0u64;
        while v < 1 << 40 {
            let i = LogHist::index(v);
            let lo = LogHist::bucket_low(i);
            assert!(lo <= v, "v {v} lo {lo}");
            assert_eq!(LogHist::index(lo), i);
            if v >= SUB {
                assert!((v - lo) as f64 / v as f64 <= 1.0 / 128.0);
            }
            v = v * 3 / 2 + 1;
        }
    }

    #[test]
    fn quantiles() {
        let mut h = LogHist::default();
        for v in 0..100u64 {
            h.add(v);
        }
        assert_eq!(h.quantile(0.5), 49);
        assert_eq!(h.quantile(0.95), 94);
        assert_eq!(h.quantile(1.0), 99);
        assert_eq!(h.total(), 100);
        let mut g = LogHist::default();
        g.add_n(1_000_000, 10);
        h.merge(&g);
        assert_eq!(h.total(), 110);
        assert!(h.quantile(1.0) <= 1_000_000 && h.quantile(1.0) > 990_000);
    }
}
