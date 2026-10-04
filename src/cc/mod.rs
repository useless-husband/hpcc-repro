//! Congestion control algorithms.
//!
//! Every algorithm is a per-flow state machine that only sees what a real
//! sender NIC would see: acknowledgements (with ECN echo, the echoed send
//! timestamp, and, for HPCC, the INT records copied by the receiver) and its
//! own timers. Its output is a pacing rate and, for window-based schemes, a
//! limit on in-flight bytes. The network model never peeks into CC state and
//! the CC never peeks into the network.

pub mod dcqcn;
pub mod dctcp;
pub mod hpcc;
pub mod timely;

use crate::packet::IntHop;
use crate::time::Time;

/// What the sender NIC enforces for a flow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ctl {
    /// Pacing rate in bit/s.
    pub rate_bps: u64,
    /// In-flight byte limit; 0 means "no window" (pure rate-based).
    pub win: u64,
}

/// Static per-flow parameters every algorithm can use.
#[derive(Clone, Copy, Debug)]
pub struct FlowEnv {
    /// NIC line rate (bit/s).
    pub line_rate_bps: u64,
    /// The base RTT `T` used for windows (HPCC §3.2: slightly above the
    /// maximum base RTT of the network; 13 us in the paper's simulations).
    pub base_rtt: Time,
    /// Payload bytes per full packet.
    pub mtu: u32,
}

impl FlowEnv {
    /// `W_init = B_NIC x T` (HPCC §3.2), in bytes.
    pub fn bdp_bytes(&self) -> f64 {
        // Integer arithmetic so that e.g. 100 Gbps x 13 us is exactly 162500.
        (self.line_rate_bps as u128 * self.base_rtt as u128 / (8 * crate::time::SEC as u128)) as f64
    }
}

/// Information an ACK gives the sender.
pub struct AckView<'a> {
    pub now: Time,
    /// Cumulative acknowledgement: next byte the receiver expects.
    pub ack_seq: u64,
    /// The sender's next sequence number to send (for once-per-RTT logic).
    pub snd_nxt: u64,
    /// ECN echo: the acknowledged data packet was CE-marked.
    pub ecn: bool,
    /// INT records echoed from the data packet (HPCC only).
    pub hops: &'a [IntHop],
    /// RTT sample: now minus the echoed send timestamp.
    pub rtt: Time,
}

/// Timers a CC asks for. Arming a timer of a kind that is already pending
/// replaces it (the old one is cancelled).
#[derive(Default, Debug)]
pub struct TimerReqs {
    pub reqs: [(u8, Time); 4],
    pub n: usize,
}

impl TimerReqs {
    pub fn arm(&mut self, kind: u8, delay: Time) {
        assert!(self.n < self.reqs.len());
        self.reqs[self.n] = (kind, delay);
        self.n += 1;
    }
    pub fn iter(&self) -> impl Iterator<Item = &(u8, Time)> {
        self.reqs[..self.n].iter()
    }
}

/// Which algorithm (and variant) a flow runs.
#[derive(Clone, Debug, PartialEq)]
pub enum CcKind {
    /// Line rate, no window: used to validate the network model.
    None,
    Hpcc(hpcc::HpccParams),
    Dcqcn(dcqcn::DcqcnParams),
    Dctcp(dctcp::DctcpParams),
    Timely(timely::TimelyParams),
}

impl CcKind {
    pub fn name(&self) -> String {
        match self {
            CcKind::None => "none".into(),
            CcKind::Hpcc(p) => match p.reaction {
                hpcc::Reaction::Hpcc => "hpcc".into(),
                hpcc::Reaction::PerAck => "hpcc-perack".into(),
                hpcc::Reaction::PerRtt => "hpcc-perrtt".into(),
            },
            CcKind::Dcqcn(p) => {
                if p.window {
                    "dcqcn+win".into()
                } else {
                    "dcqcn".into()
                }
            }
            CcKind::Dctcp(_) => "dctcp".into(),
            CcKind::Timely(p) => {
                if p.window {
                    "timely+win".into()
                } else {
                    "timely".into()
                }
            }
        }
    }

    /// Whether switches must stamp INT records into data packets.
    pub fn uses_int(&self) -> bool {
        matches!(self, CcKind::Hpcc(_))
    }
}

/// Per-flow CC state.
#[derive(Clone, Debug)]
pub enum CcState {
    None,
    Hpcc(hpcc::Hpcc),
    Dcqcn(dcqcn::Dcqcn),
    Dctcp(dctcp::Dctcp),
    Timely(timely::Timely),
}

impl CcState {
    pub fn new(kind: &CcKind, env: &FlowEnv) -> (CcState, Ctl) {
        match kind {
            CcKind::None => (
                CcState::None,
                Ctl {
                    rate_bps: env.line_rate_bps,
                    win: 0,
                },
            ),
            CcKind::Hpcc(p) => {
                let (s, c) = hpcc::Hpcc::new(p.clone(), env);
                (CcState::Hpcc(s), c)
            }
            CcKind::Dcqcn(p) => {
                let (s, c) = dcqcn::Dcqcn::new(p.clone(), env);
                (CcState::Dcqcn(s), c)
            }
            CcKind::Dctcp(p) => {
                let (s, c) = dctcp::Dctcp::new(p.clone(), env);
                (CcState::Dctcp(s), c)
            }
            CcKind::Timely(p) => {
                let (s, c) = timely::Timely::new(p.clone(), env);
                (CcState::Timely(s), c)
            }
        }
    }

    pub fn on_ack(&mut self, ack: &AckView, ctl: &mut Ctl, timers: &mut TimerReqs) {
        match self {
            CcState::None => {}
            CcState::Hpcc(s) => s.on_ack(ack, ctl),
            CcState::Dcqcn(s) => s.on_ack(ack, ctl, timers),
            CcState::Dctcp(s) => s.on_ack(ack, ctl),
            CcState::Timely(s) => s.on_ack(ack, ctl),
        }
    }

    pub fn on_timer(&mut self, kind: u8, now: Time, ctl: &mut Ctl, timers: &mut TimerReqs) {
        if let CcState::Dcqcn(s) = self {
            s.on_timer(kind, now, ctl, timers)
        }
    }
}

/// Clamps a rate to `[min, max]`.
pub(crate) fn clamp_rate(r: f64, min: u64, max: u64) -> u64 {
    if r.is_nan() || r < min as f64 {
        min
    } else if r > max as f64 {
        max
    } else {
        r as u64
    }
}
