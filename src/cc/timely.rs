//! TIMELY sender.
//!
//! Mittal et al., "TIMELY: RTT-based Congestion Control for the Datacenter",
//! SIGCOMM 2015, Algorithm 1:
//!   rtt_diff = (1 - a) rtt_diff + a (new_rtt - prev_rtt)
//!   gradient = rtt_diff / minRTT
//!   new_rtt < T_low   -> R += delta
//!   new_rtt > T_high  -> R *= 1 - beta (1 - T_high / new_rtt)
//!   gradient <= 0     -> R += N delta (N = 5 after 5 increases in a row, else 1)
//!   otherwise         -> R *= 1 - beta gradient
//! Parameters: a = 0.875, beta = 0.8, T_low = 50 us, T_high = 500 us,
//! minRTT = 20 us (TIMELY paper / authors' simulator defaults); the HPCC
//! simulator scales delta with link rate (100 Mbps at 100 Gbps) and the
//! hyper-increase step to 500 Mbps. The rate is updated once per RTT
//! (once per window of data), like the authors' simulator does.
//!
//! `window = true` is the paper's "TIMELY+win".

use super::{clamp_rate, AckView, Ctl, FlowEnv};
use crate::time::{Time, US};

#[derive(Clone, Debug, PartialEq)]
pub struct TimelyParams {
    pub ewma: f64,
    pub beta: f64,
    pub t_low: Time,
    pub t_high: Time,
    pub min_rtt: Time,
    pub delta_bps: u64,
    pub hai_bps: u64,
    pub min_rate_bps: u64,
    pub window: bool,
}

impl TimelyParams {
    pub fn paper(line_rate_bps: u64, window: bool) -> Self {
        let scale = line_rate_bps as f64 / 10e9;
        TimelyParams {
            ewma: 0.875,
            beta: 0.8,
            t_low: 50 * US,
            t_high: 500 * US,
            min_rtt: 20 * US,
            delta_bps: (10e6 * scale) as u64,
            hai_bps: (50e6 * scale) as u64,
            min_rate_bps: 1_000_000_000,
            window,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Timely {
    p: TimelyParams,
    line_rate_bps: u64,
    w_init: f64,
    pub rate: f64,
    rtt_diff: f64,
    prev_rtt: Option<Time>,
    inc_stage: u32,
    last_update_seq: u64,
}

impl Timely {
    pub fn new(p: TimelyParams, env: &FlowEnv) -> (Timely, Ctl) {
        let s = Timely {
            p,
            line_rate_bps: env.line_rate_bps,
            w_init: env.bdp_bytes(),
            rate: env.line_rate_bps as f64,
            rtt_diff: 0.0,
            prev_rtt: None,
            inc_stage: 0,
            last_update_seq: 0,
        };
        let c = s.ctl();
        (s, c)
    }

    fn ctl(&self) -> Ctl {
        let rate = clamp_rate(self.rate, self.p.min_rate_bps, self.line_rate_bps);
        let win = if self.p.window {
            (self.w_init * rate as f64 / self.line_rate_bps as f64).ceil().max(1.0) as u64
        } else {
            0
        };
        Ctl { rate_bps: rate, win }
    }

    pub fn on_ack(&mut self, ack: &AckView, ctl: &mut Ctl) {
        if ack.ack_seq <= self.last_update_seq {
            return;
        }
        self.last_update_seq = ack.snd_nxt;
        let new_rtt = ack.rtt;
        let Some(prev) = self.prev_rtt else {
            self.prev_rtt = Some(new_rtt);
            return;
        };
        self.prev_rtt = Some(new_rtt);
        let new_diff = new_rtt as f64 - prev as f64;
        self.rtt_diff = (1.0 - self.p.ewma) * self.rtt_diff + self.p.ewma * new_diff;
        let gradient = self.rtt_diff / self.p.min_rtt as f64;
        if new_rtt < self.p.t_low {
            self.increase();
        } else if new_rtt > self.p.t_high {
            let c = 1.0 - self.p.beta * (1.0 - self.p.t_high as f64 / new_rtt as f64);
            self.decrease(c);
        } else if gradient <= 0.0 {
            self.increase();
        } else {
            self.decrease((1.0 - self.p.beta * gradient).max(0.0));
        }
        *ctl = self.ctl();
    }

    fn increase(&mut self) {
        let step = if self.inc_stage < 5 { self.p.delta_bps } else { self.p.hai_bps };
        self.rate = (self.rate + step as f64).min(self.line_rate_bps as f64);
        self.inc_stage += 1;
    }

    fn decrease(&mut self, c: f64) {
        self.rate = (self.rate * c).max(self.p.min_rate_bps as f64);
        self.inc_stage = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::gbps;

    fn env() -> FlowEnv {
        FlowEnv { line_rate_bps: gbps(100), base_rtt: 13 * US, mtu: 1000 }
    }

    fn a(seq: u64, nxt: u64, rtt: Time) -> AckView<'static> {
        AckView { now: 0, ack_seq: seq, snd_nxt: nxt, ecn: false, hops: &[], rtt }
    }

    #[test]
    fn high_rtt_cuts_rate() {
        let (mut t, mut ctl) = Timely::new(TimelyParams::paper(gbps(100), false), &env());
        t.on_ack(&a(10, 20, 600 * US), &mut ctl);
        t.on_ack(&a(30, 40, 1000 * US), &mut ctl);
        // c = 1 - 0.8 (1 - 500/1000) = 0.6
        assert!((t.rate - 60e9).abs() < 1.0, "{}", t.rate);
    }

    #[test]
    fn rising_rtt_in_band_cuts_by_gradient() {
        let (mut t, mut ctl) = Timely::new(TimelyParams::paper(gbps(100), false), &env());
        t.on_ack(&a(10, 20, 100 * US), &mut ctl);
        t.on_ack(&a(30, 40, 102 * US), &mut ctl);
        // diff = 0.875 * 2us; gradient = 1.75/20 = 0.0875; c = 1 - 0.07
        assert!((t.rate - 100e9 * (1.0 - 0.8 * 0.0875)).abs() < 10.0, "{}", t.rate);
    }

    #[test]
    fn low_rtt_increases_with_hai_after_five() {
        let (mut t, mut ctl) = Timely::new(TimelyParams::paper(gbps(100), true), &env());
        t.rate = 10e9;
        t.on_ack(&a(10, 20, 20 * US), &mut ctl);
        for k in 0..7u64 {
            t.on_ack(&a(30 + 20 * k, 40 + 20 * k, 20 * US), &mut ctl);
        }
        // 5 steps of 100 Mbps, then 2 of 500 Mbps.
        assert!((t.rate - (10e9 + 5.0 * 100e6 + 2.0 * 500e6)).abs() < 1.0, "{}", t.rate);
        assert!(ctl.win > 0);
    }
}
