//! DCTCP sender.
//!
//! Alizadeh et al., "Data Center TCP (DCTCP)", SIGCOMM 2010, §3.1:
//!   alpha <- (1 - g) alpha + g F   once per window of data, F = fraction
//!                                  of ACKs with ECN echo in that window
//!   cwnd  <- cwnd (1 - alpha/2)    at most once per window, on ECN echo
//! with g = 1/16 and marking threshold K = 30 KB at 10 Gbps (scaled with link
//! rate, as the HPCC paper does in §5.1).
//!
//! As in the HPCC paper's evaluation (§5.1: "We remove the slow start phase
//! in DCTCP for fair comparisons"), flows start with cwnd = W_init (one BDP)
//! and grow by one MSS per window (congestion avoidance). Like the other
//! schemes in the HPCC framework the window is paced at cwnd / T and capped
//! at one BDP; this is how the authors' simulator models DCTCP too.

use super::{clamp_rate, AckView, Ctl, FlowEnv};

#[derive(Clone, Debug, PartialEq)]
pub struct DctcpParams {
    pub g: f64,
    pub min_rate_bps: u64,
}

impl Default for DctcpParams {
    fn default() -> Self {
        DctcpParams { g: 1.0 / 16.0, min_rate_bps: 1_000_000_000 }
    }
}

#[derive(Clone, Debug)]
pub struct Dctcp {
    p: DctcpParams,
    line_rate_bps: u64,
    mss: f64,
    w_init: f64,
    w_min: f64,
    pub cwnd: f64,
    pub alpha: f64,
    last_update_seq: u64,
    ecn_acks: u64,
    acks: u64,
    in_cwr: bool,
    high_seq: u64,
}

impl Dctcp {
    pub fn new(p: DctcpParams, env: &FlowEnv) -> (Dctcp, Ctl) {
        let w_init = env.bdp_bytes();
        let w_min = w_init * p.min_rate_bps as f64 / env.line_rate_bps as f64;
        let s = Dctcp {
            p,
            line_rate_bps: env.line_rate_bps,
            mss: env.mtu as f64,
            w_init,
            w_min,
            cwnd: w_init,
            alpha: 1.0,
            last_update_seq: 0,
            ecn_acks: 0,
            acks: 0,
            in_cwr: false,
            high_seq: 0,
        };
        let c = s.ctl();
        (s, c)
    }

    fn ctl(&self) -> Ctl {
        let rate = self.line_rate_bps as f64 * (self.cwnd / self.w_init);
        Ctl { rate_bps: clamp_rate(rate, self.p.min_rate_bps, self.line_rate_bps), win: self.cwnd.ceil() as u64 }
    }

    pub fn on_ack(&mut self, ack: &AckView, ctl: &mut Ctl) {
        self.acks += 1;
        self.ecn_acks += ack.ecn as u64;
        let mut new_window = false;
        if ack.ack_seq > self.last_update_seq {
            // A full window of data has been acknowledged: update alpha.
            new_window = true;
            if self.last_update_seq != 0 {
                let f = self.ecn_acks as f64 / self.acks as f64;
                self.alpha = (1.0 - self.p.g) * self.alpha + self.p.g * f;
            }
            self.last_update_seq = ack.snd_nxt;
            self.ecn_acks = 0;
            self.acks = 0;
        }
        if self.in_cwr && ack.ack_seq > self.high_seq {
            self.in_cwr = false;
        }
        if ack.ecn && !self.in_cwr {
            // Reduce once per window of data.
            self.cwnd = (self.cwnd * (1.0 - self.alpha / 2.0)).max(self.w_min);
            self.in_cwr = true;
            self.high_seq = ack.snd_nxt;
        } else if !self.in_cwr && new_window {
            // Congestion avoidance: one MSS per window.
            self.cwnd = (self.cwnd + self.mss).min(self.w_init);
        }
        *ctl = self.ctl();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{gbps, US};

    fn env() -> FlowEnv {
        FlowEnv { line_rate_bps: gbps(100), base_rtt: 13 * US, mtu: 1000 }
    }

    fn a(seq: u64, nxt: u64, ecn: bool) -> AckView<'static> {
        AckView { now: 0, ack_seq: seq, snd_nxt: nxt, ecn, hops: &[], rtt: 0 }
    }

    #[test]
    fn halves_on_first_mark_and_once_per_window() {
        let (mut d, mut ctl) = Dctcp::new(DctcpParams::default(), &env());
        assert_eq!(ctl.rate_bps, gbps(100));
        d.on_ack(&a(1000, 100_000, true), &mut ctl);
        assert!((d.cwnd - 81_250.0).abs() < 1e-6, "alpha starts at 1 -> halve");
        // More marks within the same window: no further reduction.
        for s in 2..50 {
            d.on_ack(&a(s * 1000, 100_000, true), &mut ctl);
        }
        assert!((d.cwnd - 81_250.0).abs() < 1e-6);
    }

    #[test]
    fn alpha_tracks_marked_fraction() {
        let (mut d, mut ctl) = Dctcp::new(DctcpParams::default(), &env());
        // Many windows with 25% of ACKs marked; alpha converges to 0.25.
        let mut seq = 0;
        for w in 0..400u64 {
            let nxt = (w + 1) * 100_000;
            for i in 0..100 {
                seq += 1000;
                d.on_ack(&a(seq, nxt, i % 4 == 0), &mut ctl);
            }
        }
        assert!((d.alpha - 0.25).abs() < 0.02, "alpha {}", d.alpha);
    }

    #[test]
    fn additive_increase_one_mss_per_window() {
        let (mut d, mut ctl) = Dctcp::new(DctcpParams::default(), &env());
        d.cwnd = 50_000.0;
        d.on_ack(&a(1, 10, false), &mut ctl);
        let c0 = d.cwnd;
        d.on_ack(&a(5, 10, false), &mut ctl);
        assert_eq!(d.cwnd, c0, "same window: no increase");
        d.on_ack(&a(11, 20, false), &mut ctl);
        assert_eq!(d.cwnd, c0 + 1000.0);
    }
}
