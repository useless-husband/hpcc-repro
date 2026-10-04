//! Shared-buffer switch memory management with PFC.
//!
//! Model (one lossless data priority per port):
//!
//! * Every ingress port has a small guaranteed `reserve`; beyond it, bytes
//!   come from a shared pool. An ingress port may use at most a *dynamic
//!   threshold* of the pool: `alpha * (free shared buffer)` (Choudhury &
//!   Hahne, "Dynamic queue length thresholds for shared-memory packet
//!   switches", ToN 1998). The HPCC paper (§5.1) sets the threshold so that
//!   "PFC is triggered when an ingress queue consumes more than 11% of the
//!   free buffer": with `alpha = 1/8` a single congested ingress settles at
//!   `q = alpha * (B - q)`, i.e. `q = B/9 = 11%` of the buffer.
//! * When an ingress port crosses its threshold the switch sends PFC PAUSE
//!   upstream. Bytes that keep arriving while the pause propagates go to a
//!   per-port `headroom`, sized to the bytes that can be in flight (the
//!   authors' simulator uses 3 x rate x one-way delay).
//! * RESUME is sent once the headroom is empty and the ingress usage is
//!   `resume_offset` bytes below the threshold (hysteresis).
//! * Egress bytes per port are tracked for ECN marking (RED on egress
//!   queue length at dequeue).
//!
//! The bookkeeping rules mirror `SwitchMmu` in the authors' public ns-3
//! simulator (alibaba-edu/High-Precision-Congestion-Control,
//! `src/point-to-point/model/switch-mmu.cc`) so that PFC behaves like the
//! simulator used for the paper's Figure 11; the code here is an
//! independent re-implementation with explicit invariants.

#[derive(Clone, Debug)]
pub struct MmuConfig {
    pub buffer_bytes: u64,
    pub reserve: u64,
    pub resume_offset: u64,
    /// Headroom per port in bytes.
    pub headroom: Vec<u64>,
    /// Dynamic-threshold factor per port as a right shift: alpha = 2^-shift.
    pub alpha_shift: Vec<u32>,
    pub pfc_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct Mmu {
    pub cfg: MmuConfig,
    total_hdrm: u64,
    total_rsrv: u64,
    shared_used: u64,
    ingress: Vec<u64>,
    hdrm: Vec<u64>,
    egress: Vec<u64>,
    /// True while this switch has an outstanding PAUSE on the ingress port.
    paused: Vec<bool>,
}

/// Outcome of offering a packet to the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    Accepted,
    Dropped,
}

impl Mmu {
    pub fn new(cfg: MmuConfig) -> Self {
        let n = cfg.headroom.len();
        assert_eq!(cfg.alpha_shift.len(), n);
        let total_hdrm = if cfg.pfc_enabled {
            cfg.headroom.iter().sum()
        } else {
            0
        };
        let total_rsrv = cfg.reserve * n as u64;
        assert!(
            total_hdrm + total_rsrv < cfg.buffer_bytes,
            "buffer too small for headroom+reserve"
        );
        Mmu {
            cfg,
            total_hdrm,
            total_rsrv,
            shared_used: 0,
            ingress: vec![0; n],
            hdrm: vec![0; n],
            egress: vec![0; n],
            paused: vec![false; n],
        }
    }

    /// Dynamic PFC threshold of ingress `port` given the current free buffer.
    #[inline]
    pub fn threshold(&self, port: usize) -> u64 {
        let free = self
            .cfg
            .buffer_bytes
            .saturating_sub(self.total_hdrm + self.total_rsrv + self.shared_used);
        free >> self.cfg.alpha_shift[port]
    }

    /// Bytes of ingress `port` that come out of the shared pool.
    #[inline]
    pub fn shared_used_by(&self, port: usize) -> u64 {
        self.ingress[port].saturating_sub(self.cfg.reserve)
    }

    /// Admission control for a data packet arriving on `in_port`.
    pub fn admit(&self, in_port: usize, size: u64) -> Admit {
        let over_threshold = size + self.shared_used_by(in_port) > self.threshold(in_port);
        let fits_reserve = self.ingress[in_port] + size <= self.cfg.reserve;
        if self.cfg.pfc_enabled {
            if size + self.hdrm[in_port] > self.cfg.headroom[in_port] && over_threshold {
                return Admit::Dropped;
            }
        } else if over_threshold && !fits_reserve {
            // Lossy mode: no headroom, drop once the dynamic threshold is hit.
            return Admit::Dropped;
        }
        Admit::Accepted
    }

    /// Accounts an admitted packet (ingress `in_port`, egress `out_port`).
    pub fn enqueue(&mut self, in_port: usize, out_port: usize, size: u64) {
        let new_bytes = self.ingress[in_port] + size;
        if new_bytes <= self.cfg.reserve {
            self.ingress[in_port] = new_bytes;
        } else {
            let thresh = self.threshold(in_port);
            if self.cfg.pfc_enabled && new_bytes - self.cfg.reserve > thresh {
                self.hdrm[in_port] += size;
            } else {
                self.ingress[in_port] = new_bytes;
                self.shared_used += size.min(new_bytes - self.cfg.reserve);
            }
        }
        self.egress[out_port] += size;
    }

    /// Releases a packet's bytes when it leaves the egress queue. Headroom is
    /// drained first (it is the most recently admitted traffic).
    pub fn dequeue(&mut self, in_port: usize, out_port: usize, size: u64) {
        let from_hdrm = self.hdrm[in_port].min(size);
        let from_ingress = size - from_hdrm;
        let from_shared = from_ingress.min(self.shared_used_by(in_port));
        self.hdrm[in_port] -= from_hdrm;
        assert!(
            self.ingress[in_port] >= from_ingress,
            "ingress underflow on port {in_port}"
        );
        self.ingress[in_port] -= from_ingress;
        self.shared_used -= from_shared;
        assert!(
            self.egress[out_port] >= size,
            "egress underflow on port {out_port}"
        );
        self.egress[out_port] -= size;
    }

    /// Should a PAUSE be sent upstream on `port` now?
    #[inline]
    pub fn should_pause(&self, port: usize) -> bool {
        self.cfg.pfc_enabled
            && !self.paused[port]
            && (self.hdrm[port] > 0 || self.shared_used_by(port) >= self.threshold(port))
    }

    /// Should a RESUME be sent upstream on `port` now?
    #[inline]
    pub fn should_resume(&self, port: usize) -> bool {
        if !self.paused[port] {
            return false;
        }
        let used = self.shared_used_by(port);
        self.hdrm[port] == 0 && (used == 0 || used + self.cfg.resume_offset <= self.threshold(port))
    }

    pub fn set_paused(&mut self, port: usize, paused: bool) {
        self.paused[port] = paused;
    }

    pub fn is_paused(&self, port: usize) -> bool {
        self.paused[port]
    }

    pub fn egress_bytes(&self, port: usize) -> u64 {
        self.egress[port]
    }

    pub fn headroom_used(&self, port: usize) -> u64 {
        self.hdrm[port]
    }

    pub fn ingress_bytes(&self, port: usize) -> u64 {
        self.ingress[port]
    }

    /// Total bytes buffered (sum over egress queues).
    pub fn total_buffered(&self) -> u64 {
        self.egress.iter().sum()
    }

    /// Checks the accounting invariants; returns a description on failure.
    pub fn check_invariants(&self) -> Result<(), String> {
        let ing: u64 = self.ingress.iter().sum::<u64>() + self.hdrm.iter().sum::<u64>();
        let eg: u64 = self.egress.iter().sum();
        if ing != eg {
            return Err(format!("ingress+headroom {ing} != egress {eg}"));
        }
        let shared: u64 = (0..self.ingress.len())
            .map(|p| self.shared_used_by(p))
            .sum();
        if shared != self.shared_used {
            return Err(format!(
                "shared_used {} != sum of per-port shared {shared}",
                self.shared_used
            ));
        }
        for (p, &h) in self.hdrm.iter().enumerate() {
            if h > self.cfg.headroom[p] && self.cfg.pfc_enabled {
                return Err(format!(
                    "headroom overflow on port {p}: {h} > {}",
                    self.cfg.headroom[p]
                ));
            }
        }
        if eg > self.cfg.buffer_bytes {
            return Err(format!("buffer overflow: {eg} > {}", self.cfg.buffer_bytes));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;
    use std::collections::VecDeque;

    fn cfg(ports: usize, buffer: u64, pfc: bool) -> MmuConfig {
        MmuConfig {
            buffer_bytes: buffer,
            reserve: 4 * 1024,
            resume_offset: 3 * 1024,
            headroom: vec![37_500; ports],
            alpha_shift: vec![3; ports],
            pfc_enabled: pfc,
        }
    }

    #[test]
    fn threshold_is_alpha_times_free_buffer() {
        let m = Mmu::new(cfg(4, 1_000_000, true));
        let free = 1_000_000 - 4 * 37_500 - 4 * 4096;
        assert_eq!(m.threshold(0), free >> 3);
    }

    #[test]
    fn single_congested_ingress_settles_near_one_ninth() {
        // Fill from one ingress port until PFC triggers; the shared usage
        // where the pause starts must be alpha/(1+alpha) of the pool.
        let mut m = Mmu::new(cfg(2, 32 << 20, true));
        let pool = (32u64 << 20) - 2 * 37_500 - 2 * 4096;
        while !m.should_pause(0) {
            assert_eq!(m.admit(0, 1048), Admit::Accepted);
            m.enqueue(0, 1, 1048);
        }
        let frac = m.shared_used_by(0) as f64 / pool as f64;
        assert!((frac - 1.0 / 9.0).abs() < 0.001, "frac {frac}");
    }

    #[test]
    fn pause_and_resume_boundaries() {
        let mut m = Mmu::new(cfg(2, 2_000_000, true));
        // Within the reserve nothing happens.
        m.enqueue(0, 1, 4096);
        assert!(!m.should_pause(0));
        // Fill until exactly the threshold: pause must trigger at >=.
        let mut n = 0;
        loop {
            let used = m.shared_used_by(0);
            let th = m.threshold(0);
            if used >= th {
                break;
            }
            assert!(
                !m.should_pause(0),
                "paused below threshold: used {used} < th {th}"
            );
            m.enqueue(0, 1, 1);
            n += 1;
        }
        assert!(n > 0);
        assert!(m.should_pause(0), "must pause at used == threshold");
        m.set_paused(0, true);
        assert!(!m.should_pause(0), "no duplicate pause");
        // Drain byte by byte; resume exactly when used + offset <= threshold.
        loop {
            let used = m.shared_used_by(0);
            let expect = used == 0 || used + 3 * 1024 <= m.threshold(0);
            assert_eq!(m.should_resume(0), expect, "used {used}");
            if expect {
                break;
            }
            m.dequeue(0, 1, 1);
        }
        m.check_invariants().unwrap();
    }

    #[test]
    fn headroom_absorbs_then_drops_only_when_full() {
        let mut m = Mmu::new(cfg(2, 400_000, true));
        // Push past the threshold; the extra goes to headroom.
        let mut accepted = 0u64;
        while m.admit(0, 1000) == Admit::Accepted {
            m.enqueue(0, 1, 1000);
            accepted += 1000;
            m.check_invariants().unwrap();
        }
        assert!(
            m.headroom_used(0) > 37_500 - 1000,
            "headroom nearly full when dropping"
        );
        assert!(m.headroom_used(0) <= 37_500);
        assert_eq!(m.total_buffered(), accepted);
    }

    #[test]
    fn randomised_accounting_invariants() {
        // Random multi-port traffic with FIFO egress queues: invariants hold
        // after every operation and everything returns to zero when drained.
        for seed in 0..30u64 {
            let mut rng = Rng::new(seed);
            let ports = 6;
            let pfc = seed % 3 != 0;
            let mut m = Mmu::new(cfg(ports, 600_000, pfc));
            let mut queues: Vec<VecDeque<(usize, u64)>> = vec![VecDeque::new(); ports];
            for step in 0..20_000 {
                if rng.below(2) == 0 {
                    let inp = rng.below(ports as u64) as usize;
                    let out = rng.below(ports as u64) as usize;
                    let size = 64 + rng.below(1000);
                    if m.admit(inp, size) == Admit::Accepted {
                        m.enqueue(inp, out, size);
                        queues[out].push_back((inp, size));
                    }
                } else {
                    let out = rng.below(ports as u64) as usize;
                    if let Some((inp, size)) = queues[out].pop_front() {
                        m.dequeue(inp, out, size);
                    }
                }
                if let Err(e) = m.check_invariants() {
                    panic!("seed {seed} step {step}: {e}");
                }
                for (p, queue) in queues.iter().enumerate() {
                    let q: u64 = queue.iter().map(|x| x.1).sum();
                    assert_eq!(q, m.egress_bytes(p), "seed {seed}");
                }
            }
            for (out, queue) in queues.iter_mut().enumerate() {
                while let Some((inp, size)) = queue.pop_front() {
                    m.dequeue(inp, out, size);
                }
            }
            m.check_invariants().unwrap();
            assert_eq!(m.total_buffered(), 0);
            for p in 0..ports {
                assert_eq!(m.ingress_bytes(p) + m.headroom_used(p), 0, "seed {seed}");
            }
        }
    }
}
