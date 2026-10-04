//! HPCC sender algorithm.
//!
//! Li et al., "HPCC: High Precision Congestion Control", SIGCOMM 2019,
//! Algorithm 1 (§3.2) and the parameters of §3.3/§5.1:
//! eta = 95%, maxStage = 5, W_AI = 80 bytes, T = 13 us (simulations).
//!
//! The window W limits in-flight bytes and the pacing rate is R = W / T
//! (§3.2). Line numbers in the comments refer to Algorithm 1 in the paper.
//!
//! Choices the paper leaves open, taken from the authors' public ns-3 code
//! (`rdma-hw.cc`, `UpdateRateHp`) and stated here so they can be checked:
//! * the first ACK of a flow only records the INT stack (there is no
//!   previous record to compute a rate from) and starts the first RTT;
//! * U starts at 1;
//! * W is clamped to [min_rate x T, W_init] (no faster than line rate).

use super::{clamp_rate, AckView, Ctl, FlowEnv};
use crate::packet::{IntHop, MAX_HOPS};

/// How the sender reacts to ACKs (§5.4, Figure 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// HPCC: react to every ACK relative to a reference window W^c that is
    /// updated once per RTT (Algorithm 1).
    Hpcc,
    /// React to every ACK and update the reference every ACK (overreacts).
    PerAck,
    /// React only once per RTT (slow).
    PerRtt,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HpccParams {
    /// Target utilisation eta.
    pub eta: f64,
    /// Number of additive-increase rounds before a multiplicative increase.
    pub max_stage: u32,
    /// Additive increase step W_AI in bytes.
    pub w_ai: f64,
    /// Lower bound on the pacing rate (authors' simulator: 1 Gbps).
    pub min_rate_bps: u64,
    pub reaction: Reaction,
}

impl Default for HpccParams {
    fn default() -> Self {
        HpccParams {
            eta: 0.95,
            max_stage: 5,
            w_ai: 80.0,
            min_rate_bps: 1_000_000_000,
            reaction: Reaction::Hpcc,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hpcc {
    p: HpccParams,
    /// T in seconds.
    t: f64,
    line_rate_bps: u64,
    w_init: f64,
    w_min: f64,
    /// Current window W (bytes).
    pub w: f64,
    /// Reference window W^c (bytes).
    pub wc: f64,
    /// EWMA of the normalised in-flight bytes, U.
    pub u: f64,
    pub inc_stage: u32,
    pub last_update_seq: u64,
    /// L: link feedback from the last ACK processed.
    l: [IntHop; MAX_HOPS],
    l_n: usize,
    have_l: bool,
}

impl Hpcc {
    pub fn new(p: HpccParams, env: &FlowEnv) -> (Hpcc, Ctl) {
        let t = env.base_rtt as f64 * 1e-12;
        // W_init = B_NIC x T (§3.2), so flows start at line rate.
        let w_init = env.bdp_bytes();
        let w_min = p.min_rate_bps as f64 / 8.0 * t;
        let s = Hpcc {
            t,
            line_rate_bps: env.line_rate_bps,
            w_init,
            w_min,
            w: w_init,
            wc: w_init,
            u: 1.0,
            inc_stage: 0,
            last_update_seq: 0,
            l: [IntHop::default(); MAX_HOPS],
            l_n: 0,
            have_l: false,
            p,
        };
        let ctl = s.ctl();
        (s, ctl)
    }

    fn ctl(&self) -> Ctl {
        // Line 27: R = W / T, written as line_rate x W / W_init (W_init =
        // line_rate x T) so that W = W_init maps exactly to line rate.
        let rate = self.line_rate_bps as f64 * (self.w / self.w_init);
        Ctl {
            rate_bps: clamp_rate(rate, self.p.min_rate_bps, self.line_rate_bps),
            win: self.w.ceil() as u64,
        }
    }

    fn store_l(&mut self, hops: &[IntHop]) {
        let n = hops.len().min(MAX_HOPS);
        self.l[..n].copy_from_slice(&hops[..n]);
        self.l_n = n;
        self.have_l = true;
    }

    /// Lines 1-10: estimate the normalised in-flight bytes of the most
    /// congested link on the path and fold it into the EWMA U.
    pub fn measure_inflight(&mut self, hops: &[IntHop]) -> f64 {
        let mut u = 0.0;
        let mut tau_ps: u64 = 0;
        for (i, cur) in hops.iter().enumerate().take(self.l_n) {
            let last = &self.l[i];
            let dt = cur.ts.saturating_sub(last.ts);
            if dt == 0 {
                continue; // no new information about this hop
            }
            let b = cur.rate_bps as f64 / 8.0; // bytes per second
                                               // Line 4: txRate = (ack.L[i].txBytes - L[i].txBytes) / (ack.L[i].ts - L[i].ts)
            let tx_rate = cur.tx_bytes.saturating_sub(last.tx_bytes) as f64 / (dt as f64 * 1e-12);
            // Line 5: u' = min(ack.L[i].qlen, L[i].qlen) / (ack.L[i].B * T) + txRate / ack.L[i].B
            let qlen = cur.qlen.min(last.qlen) as f64;
            let u1 = qlen / (b * self.t) + tx_rate / b;
            // Lines 6-7: keep the most congested hop and its tau.
            if u1 > u {
                u = u1;
                tau_ps = dt;
            }
        }
        // Line 8: tau = min(tau, T)
        let tau = (tau_ps as f64 * 1e-12).min(self.t);
        // Line 9: U = (1 - tau/T) U + tau/T u
        self.u = (1.0 - tau / self.t) * self.u + tau / self.t * u;
        self.u
    }

    /// Lines 11-20.
    pub fn compute_wind(&mut self, u: f64, update_wc: bool) -> f64 {
        let eta = self.p.eta;
        let w;
        if u >= eta || self.inc_stage >= self.p.max_stage {
            // Line 13: W = W^c / (U/eta) + W_AI  (multiplicative step)
            w = self.clamp_w(self.wc / (u / eta) + self.p.w_ai);
            if update_wc {
                // Line 15
                self.inc_stage = 0;
                self.wc = w;
            }
        } else {
            // Line 17: W = W^c + W_AI  (additive step)
            w = self.clamp_w(self.wc + self.p.w_ai);
            if update_wc {
                // Line 19
                self.inc_stage += 1;
                self.wc = w;
            }
        }
        w
    }

    fn clamp_w(&self, w: f64) -> f64 {
        if w.is_nan() || w > self.w_init {
            self.w_init
        } else if w < self.w_min {
            self.w_min
        } else {
            w
        }
    }

    /// Lines 21-27: procedure NewAck.
    pub fn on_ack(&mut self, ack: &AckView, ctl: &mut Ctl) {
        if !self.have_l || ack.hops.len() != self.l_n {
            // First ACK (or the path length changed): nothing to diff
            // against yet. Record L and start the first RTT.
            self.store_l(ack.hops);
            self.last_update_seq = ack.snd_nxt;
            return;
        }
        match self.p.reaction {
            Reaction::Hpcc => {
                let u = self.measure_inflight(ack.hops);
                if ack.ack_seq > self.last_update_seq {
                    // Lines 22-24: a new RTT's worth of feedback; sync W^c.
                    self.w = self.compute_wind(u, true);
                    self.last_update_seq = ack.snd_nxt;
                } else {
                    // Line 26: react, but relative to the fixed W^c.
                    self.w = self.compute_wind(u, false);
                }
                self.store_l(ack.hops); // Line 27: L = ack.L
            }
            Reaction::PerAck => {
                let u = self.measure_inflight(ack.hops);
                self.w = self.compute_wind(u, true);
                self.store_l(ack.hops);
            }
            Reaction::PerRtt => {
                if ack.ack_seq > self.last_update_seq {
                    let u = self.measure_inflight(ack.hops);
                    self.w = self.compute_wind(u, true);
                    self.last_update_seq = ack.snd_nxt;
                    self.store_l(ack.hops);
                }
            }
        }
        *ctl = self.ctl();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{gbps, US};

    fn env() -> FlowEnv {
        FlowEnv {
            line_rate_bps: gbps(100),
            base_rtt: 13 * US,
            mtu: 1000,
        }
    }

    fn hop(ts_ns: u64, tx: u64, q: u64) -> IntHop {
        IntHop {
            ts: ts_ns * 1000,
            tx_bytes: tx,
            qlen: q,
            rate_bps: gbps(100),
        }
    }

    fn ack<'a>(hops: &'a [IntHop], seq: u64, nxt: u64) -> AckView<'a> {
        AckView {
            now: 0,
            ack_seq: seq,
            snd_nxt: nxt,
            ecn: false,
            hops,
            rtt: 0,
        }
    }

    #[test]
    fn starts_at_line_rate_with_bdp_window() {
        let (h, ctl) = Hpcc::new(HpccParams::default(), &env());
        assert_eq!(ctl.rate_bps, gbps(100));
        assert_eq!(ctl.win, 162_500, "W_init = 100 Gbps x 13 us");
        assert_eq!(h.u, 1.0);
    }

    #[test]
    fn w_ai_rule_of_thumb_gives_paper_value() {
        // §3.3: W_AI = W_init (1 - eta) / N with N = 100 flows -> ~80 bytes (§5.1).
        let w_ai = env().bdp_bytes() * (1.0 - 0.95) / 100.0;
        assert!((w_ai - 81.25).abs() < 1e-9);
    }

    #[test]
    fn single_congested_hop_sets_window_in_one_step() {
        // A link running at line rate with a queue of one BDP has U = 2:
        // the reference window must be cut to Wc / (2 / eta) + W_AI.
        let (mut h, mut ctl) = Hpcc::new(HpccParams::default(), &env());
        let bdp = 162_500u64;
        let l0 = [hop(0, 0, bdp)];
        h.on_ack(&ack(&l0, 1000, 10_000), &mut ctl);
        // 13 us later, 162500 bytes went out (line rate) and the queue stayed.
        let l1 = [hop(13_000, bdp, bdp)];
        h.on_ack(&ack(&l1, 20_000, 30_000), &mut ctl);
        // tau = T so U = u = 2 exactly.
        assert!((h.u - 2.0).abs() < 1e-9, "U = {}", h.u);
        let expect = 162_500.0 / (2.0 / 0.95) + 80.0;
        assert!((h.w - expect).abs() < 1e-6, "W = {}", h.w);
        assert_eq!(h.wc, h.w, "reference window synced on a new RTT");
        assert_eq!(h.inc_stage, 0);
    }

    #[test]
    fn same_rtt_acks_do_not_compound() {
        // Figure 5: two ACKs describing the same queue must not cut twice.
        let (mut h, mut ctl) = Hpcc::new(HpccParams::default(), &env());
        let bdp = 162_500u64;
        h.on_ack(&ack(&[hop(0, 0, bdp)], 1000, 10_000), &mut ctl);
        // Both ACKs below belong to the first RTT (seq <= last_update_seq).
        h.on_ack(&ack(&[hop(13_000, bdp, bdp)], 5_000, 12_000), &mut ctl);
        let w1 = h.w;
        assert!((w1 - (162_500.0 / (2.0 / 0.95) + 80.0)).abs() < 1e-6);
        h.on_ack(
            &ack(&[hop(13_100, bdp + 1250, bdp)], 6_000, 13_000),
            &mut ctl,
        );
        assert!(
            (h.w - w1).abs() / w1 < 0.01,
            "W must stay at W^c/(U/eta): {} vs {}",
            h.w,
            w1
        );
        assert_eq!(h.wc, 162_500.0, "W^c unchanged within the RTT");
    }

    #[test]
    fn per_ack_variant_compounds() {
        // The naive per-ACK reaction of Figure 13 does overreact.
        let p = HpccParams {
            reaction: Reaction::PerAck,
            ..HpccParams::default()
        };
        let (mut h, mut ctl) = Hpcc::new(p, &env());
        let bdp = 162_500u64;
        h.on_ack(&ack(&[hop(0, 0, bdp)], 1000, 10_000), &mut ctl);
        h.on_ack(&ack(&[hop(13_000, bdp, bdp)], 5_000, 12_000), &mut ctl);
        let w1 = h.w;
        h.on_ack(
            &ack(&[hop(13_100, bdp + 1250, bdp)], 6_000, 13_000),
            &mut ctl,
        );
        assert!(h.w < w1 * 0.6, "per-ACK must cut again: {} vs {}", h.w, w1);
    }

    #[test]
    fn additive_then_multiplicative_increase() {
        // An idle link (U well below eta) gives maxStage additive steps of
        // W_AI, then one multiplicative jump.
        let p = HpccParams {
            max_stage: 5,
            ..HpccParams::default()
        };
        let (mut h, mut ctl) = Hpcc::new(p, &env());
        h.wc = 20_000.0;
        h.u = 0.5;
        let mut seq = 1000;
        let mut t = 0;
        h.on_ack(&ack(&[hop(t, 0, 0)], seq, seq + 1), &mut ctl);
        let mut tx = 0;
        let mut ws = Vec::new();
        for _ in 0..7 {
            t += 13_000;
            tx += 40_625; // 25% utilisation over 13 us
            seq += 100_000;
            h.on_ack(&ack(&[hop(t, tx, 0)], seq, seq + 1), &mut ctl);
            ws.push(h.wc);
        }
        for i in 0..5 {
            let prev = if i == 0 { 20_000.0 } else { ws[i - 1] };
            assert!((ws[i] - (prev + 80.0)).abs() < 1e-6, "AI step {i}: {ws:?}");
        }
        // Stage 6: U = 0.25 -> W = Wc / (0.25/0.95) + W_AI (MI, x3.8).
        assert!(ws[5] > ws[4] * 3.5, "{ws:?}");
    }

    #[test]
    fn reacts_to_most_congested_hop() {
        let (mut h, mut ctl) = Hpcc::new(HpccParams::default(), &env());
        let bdp = 162_500u64;
        let first = [hop(0, 0, 0), hop(0, 0, 0), hop(0, 0, 0)];
        h.on_ack(&ack(&first, 1000, 10_000), &mut ctl);
        // Hop 2 (the last) has a queue of half a BDP; hops 0 and 1 are idle.
        let first2 = [hop(0, 0, 0), hop(0, 0, 0), hop(0, 0, bdp / 2)];
        h.store_l(&first2);
        let second = [
            hop(13_000, bdp / 4, 0),
            hop(13_000, bdp / 4, 0),
            hop(13_000, bdp, bdp / 2),
        ];
        let u = h.measure_inflight(&second);
        assert!(
            (u - 1.5).abs() < 1e-9,
            "u = {u}: must use the last hop (q=0.5 BDP, txRate=line)"
        );
    }

    #[test]
    fn window_never_exceeds_line_rate_or_drops_below_min() {
        let (mut h, mut ctl) = Hpcc::new(HpccParams::default(), &env());
        // Huge standing queue on both samples (qlen uses the min of the two).
        h.on_ack(&ack(&[hop(0, 0, 100_000_000)], 1, 2), &mut ctl);
        h.on_ack(&ack(&[hop(13_000, 162_500, 100_000_000)], 10, 20), &mut ctl);
        assert_eq!(ctl.rate_bps, 1_000_000_000);
        assert!(
            (h.w - 1625.0).abs() < 1e-6,
            "W clamped to min rate x T: {}",
            h.w
        );
        // The link goes idle: AI for maxStage rounds, then MI back to W_init.
        // (With exactly zero traffic u' = 0 never exceeds u = 0, tau stays 0
        // and U does not move: that is Algorithm 1 as written, and also the
        // authors' code. A sender's own ACKed packets make u' > 0 in practice.)
        for k in 0..8 {
            // Only this flow's own packets cross the link: 1000 B per T.
            h.on_ack(
                &ack(
                    &[hop(26_000 + k * 13_000, 162_500 + (k + 1) * 1000, 0)],
                    100 + k * 100,
                    150 + k * 100,
                ),
                &mut ctl,
            );
        }
        assert_eq!(ctl.rate_bps, gbps(100));
        assert!((h.w - 162_500.0).abs() < 1e-6);
    }
}
