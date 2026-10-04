//! DCQCN reaction point (sender).
//!
//! Zhu et al., "Congestion Control for Large-Scale RDMA Deployments",
//! SIGCOMM 2015 (Figure 7 / §3.3 equations):
//!   on CNP:        R_T = R_C;  R_C = R_C (1 - alpha/2);  alpha = (1-g) alpha + g
//!   no CNP for K:  alpha = (1-g) alpha
//!   increase:      fast recovery R_C = (R_T + R_C)/2 for F rounds, then
//!                  additive (R_T += R_AI) and hyper (R_T += R_HAI) increase.
//!
//! The HPCC paper evaluates DCQCN with "the parameters suggested by a major
//! NIC vendor" (§5.1) and names the vendor timers in §2.3: rate-increase
//! timer Ti = 300 us and rate-decrease interval Td = 4 us. The remaining
//! values are those in the authors' public simulator (`run.py`, cc=dcqcn):
//! alpha timer 1 us, g = 1/256, F = 1, R_AI = 20 Mbps and R_HAI = 200 Mbps at
//! 100 Gbps (scaled with the NIC rate), min rate 1 Gbps, timer-only increase
//! (no byte counter), CNPs carried as ECN echo on ACKs with at most one rate
//! decrease per Td.
//!
//! `window = true` is the paper's "DCQCN+win": the same in-flight limit HPCC
//! uses, W = W_init x R_C / line rate.

use super::{clamp_rate, AckView, Ctl, FlowEnv, TimerReqs};
use crate::time::{Time, US};

pub const TIMER_ALPHA: u8 = 0;
pub const TIMER_DECREASE: u8 = 1;
pub const TIMER_INCREASE: u8 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct DcqcnParams {
    pub g: f64,
    pub alpha_interval: Time,
    pub decrease_interval: Time,
    pub increase_interval: Time,
    pub fast_recovery_rounds: u32,
    pub rai_bps: u64,
    pub rhai_bps: u64,
    pub min_rate_bps: u64,
    pub window: bool,
}

impl DcqcnParams {
    /// Vendor parameters for a NIC of `line_rate_bps` (AI/HAI scale with it).
    pub fn vendor(line_rate_bps: u64, window: bool) -> Self {
        let scale = line_rate_bps as f64 / 25e9;
        DcqcnParams {
            g: 1.0 / 256.0,
            alpha_interval: US,
            decrease_interval: 4 * US,
            increase_interval: 300 * US,
            fast_recovery_rounds: 1,
            rai_bps: (5e6 * scale) as u64,
            rhai_bps: (50e6 * scale) as u64,
            min_rate_bps: 1_000_000_000,
            window,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dcqcn {
    p: DcqcnParams,
    line_rate_bps: u64,
    w_init: f64,
    /// Current rate R_C and target rate R_T (bit/s).
    pub rc: f64,
    pub rt: f64,
    pub alpha: f64,
    alpha_cnp_arrived: bool,
    decrease_cnp_arrived: bool,
    pub stage: u32,
    first_cnp: bool,
}

impl Dcqcn {
    pub fn new(p: DcqcnParams, env: &FlowEnv) -> (Dcqcn, Ctl) {
        let line = env.line_rate_bps as f64;
        let s = Dcqcn {
            line_rate_bps: env.line_rate_bps,
            w_init: env.bdp_bytes(),
            rc: line,
            rt: line,
            alpha: 1.0,
            alpha_cnp_arrived: false,
            decrease_cnp_arrived: false,
            stage: 0,
            first_cnp: true,
            p,
        };
        let c = s.ctl();
        (s, c)
    }

    fn ctl(&self) -> Ctl {
        let rate = clamp_rate(self.rc, self.p.min_rate_bps, self.line_rate_bps);
        let win = if self.p.window {
            (self.w_init * rate as f64 / self.line_rate_bps as f64)
                .ceil()
                .max(1.0) as u64
        } else {
            0
        };
        Ctl {
            rate_bps: rate,
            win,
        }
    }

    /// An ACK with the ECN echo set is a CNP.
    pub fn on_ack(&mut self, ack: &AckView, ctl: &mut Ctl, timers: &mut TimerReqs) {
        if !ack.ecn {
            return;
        }
        self.alpha_cnp_arrived = true;
        self.decrease_cnp_arrived = true;
        if self.first_cnp {
            self.first_cnp = false;
            self.alpha = 1.0;
            self.alpha_cnp_arrived = false;
            self.rt = self.rc;
            timers.arm(TIMER_ALPHA, self.p.alpha_interval);
            // +1 ns so the first decrease runs after the alpha update that
            // falls on the same tick (as in the vendor model).
            timers.arm(TIMER_DECREASE, self.p.decrease_interval + 1_000);
        }
        *ctl = self.ctl();
    }

    pub fn on_timer(&mut self, kind: u8, _now: Time, ctl: &mut Ctl, timers: &mut TimerReqs) {
        match kind {
            TIMER_ALPHA => {
                if self.alpha_cnp_arrived {
                    self.alpha = (1.0 - self.p.g) * self.alpha + self.p.g;
                } else {
                    self.alpha *= 1.0 - self.p.g;
                }
                self.alpha_cnp_arrived = false;
                timers.arm(TIMER_ALPHA, self.p.alpha_interval);
            }
            TIMER_DECREASE => {
                timers.arm(TIMER_DECREASE, self.p.decrease_interval);
                if self.decrease_cnp_arrived {
                    // Target rate is only clamped when the flow had started
                    // increasing again (CLAMP_TARGET_RATE = 0 in the vendor model).
                    if self.stage != 0 {
                        self.rt = self.rc;
                    }
                    self.rc = (self.rc * (1.0 - self.alpha / 2.0)).max(self.p.min_rate_bps as f64);
                    self.stage = 0;
                    self.decrease_cnp_arrived = false;
                    timers.arm(TIMER_INCREASE, self.p.increase_interval);
                }
            }
            TIMER_INCREASE => {
                timers.arm(TIMER_INCREASE, self.p.increase_interval);
                let line = self.line_rate_bps as f64;
                if self.stage < self.p.fast_recovery_rounds {
                    // fast recovery
                } else if self.stage == self.p.fast_recovery_rounds {
                    self.rt = (self.rt + self.p.rai_bps as f64).min(line);
                } else {
                    self.rt = (self.rt + self.p.rhai_bps as f64).min(line);
                }
                self.rc = (self.rc + self.rt) / 2.0;
                self.stage += 1;
            }
            _ => unreachable!("unknown DCQCN timer {kind}"),
        }
        *ctl = self.ctl();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::gbps;

    fn env() -> FlowEnv {
        FlowEnv {
            line_rate_bps: gbps(100),
            base_rtt: 13 * US,
            mtu: 1000,
        }
    }

    fn cnp() -> AckView<'static> {
        AckView {
            now: 0,
            ack_seq: 0,
            snd_nxt: 0,
            ecn: true,
            hops: &[],
            rtt: 0,
        }
    }

    #[test]
    fn vendor_parameters_at_100g() {
        let p = DcqcnParams::vendor(gbps(100), false);
        assert_eq!(p.rai_bps, 20_000_000);
        assert_eq!(p.rhai_bps, 200_000_000);
    }

    #[test]
    fn first_decrease_halves_rate() {
        let (mut d, mut ctl) = Dcqcn::new(DcqcnParams::vendor(gbps(100), false), &env());
        assert_eq!(ctl.win, 0, "plain DCQCN has no window");
        let mut t = TimerReqs::default();
        d.on_ack(&cnp(), &mut ctl, &mut t);
        assert_eq!(t.n, 2);
        // The first alpha tick sees no *new* CNP; then the decrease runs.
        d.on_timer(TIMER_ALPHA, 0, &mut ctl, &mut TimerReqs::default());
        let a = d.alpha;
        assert!((a - (1.0 - 1.0 / 256.0)).abs() < 1e-12);
        d.on_timer(TIMER_DECREASE, 0, &mut ctl, &mut TimerReqs::default());
        assert!((d.rc - 100e9 * (1.0 - a / 2.0)).abs() < 1.0);
        assert_eq!(ctl.rate_bps, (100e9 * (1.0 - a / 2.0)) as u64);
    }

    #[test]
    fn alpha_rises_while_cnps_keep_arriving() {
        // alpha <- (1 - g) alpha + g for every alpha period that saw a CNP.
        let (mut d, mut ctl) = Dcqcn::new(DcqcnParams::vendor(gbps(100), false), &env());
        d.on_ack(&cnp(), &mut ctl, &mut TimerReqs::default());
        d.alpha = 0.5;
        for _ in 0..10 {
            d.on_ack(&cnp(), &mut ctl, &mut TimerReqs::default());
            d.on_timer(TIMER_ALPHA, 0, &mut ctl, &mut TimerReqs::default());
        }
        let g = 1.0 / 256.0;
        let mut expect = 0.5;
        for _ in 0..10 {
            expect = (1.0 - g) * expect + g;
        }
        assert!(
            (d.alpha - expect).abs() < 1e-12,
            "alpha {} expected {expect}",
            d.alpha
        );
    }

    #[test]
    fn no_cnp_no_decrease_and_alpha_decays() {
        let (mut d, mut ctl) = Dcqcn::new(DcqcnParams::vendor(gbps(100), false), &env());
        d.on_ack(&cnp(), &mut ctl, &mut TimerReqs::default());
        d.on_timer(TIMER_DECREASE, 0, &mut ctl, &mut TimerReqs::default());
        let r = d.rc;
        for _ in 0..100 {
            d.on_timer(TIMER_ALPHA, 0, &mut ctl, &mut TimerReqs::default());
            d.on_timer(TIMER_DECREASE, 0, &mut ctl, &mut TimerReqs::default());
        }
        assert_eq!(d.rc, r, "no CNP since the last decrease: rate unchanged");
        assert!(d.alpha < 0.7);
    }

    #[test]
    fn recovery_goes_back_to_line_rate() {
        let p = DcqcnParams::vendor(gbps(100), true);
        let (mut d, mut ctl) = Dcqcn::new(p, &env());
        d.on_ack(&cnp(), &mut ctl, &mut TimerReqs::default());
        d.on_timer(TIMER_DECREASE, 0, &mut ctl, &mut TimerReqs::default());
        assert!(ctl.rate_bps < gbps(51));
        assert!(
            ctl.win < 162_500 / 2 + 1000,
            "DCQCN+win window follows the rate"
        );
        // Fast recovery halves the gap to R_T each round.
        d.on_timer(TIMER_INCREASE, 0, &mut ctl, &mut TimerReqs::default());
        assert!(ctl.rate_bps > gbps(74));
        for _ in 0..2000 {
            d.on_timer(TIMER_INCREASE, 0, &mut ctl, &mut TimerReqs::default());
        }
        assert_eq!(ctl.rate_bps, gbps(100));
        assert_eq!(ctl.win, 162_500);
    }
}
