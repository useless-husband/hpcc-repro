//! The network simulator: hosts with RDMA-style NICs, output-queued
//! shared-buffer switches with ECN, PFC and INT, and links with
//! serialization and propagation delay.
//!
//! Model summary (see docs/DESIGN.md for the reasoning):
//! * Links are full duplex. A port transmits one packet at a time; a packet
//!   takes `size / rate` to serialize and `delay` to propagate, and is
//!   received only when its last bit arrives (store-and-forward).
//! * Every port has two strict-priority queues: control (ACK, NACK, PFC
//!   frames) and data. Only data is subject to buffer admission, PFC and
//!   ECN, like the lossless RDMA class in the paper.
//! * Switch egress picks an ECMP next hop per flow; ECN is marked RED-style
//!   on the egress queue length at dequeue; INT records are appended at
//!   dequeue (HPCC paper §3.1, Fig. 7).
//! * A host NIC serves its flows round-robin. A flow may send when its pacing
//!   timer allows (rate) and its in-flight bytes are below its window.
//!   Receivers acknowledge every data packet and copy ECN and INT into the
//!   ACK (HPCC paper Fig. 4).

use crate::cc::{AckView, CcKind, CcState, Ctl, FlowEnv, TimerReqs};
use crate::event::EventQueue;
use crate::mmu::{Admit, Mmu, MmuConfig};
use crate::packet::{IntHop, Kind, Packet, PacketPool, MAX_HOPS};
use crate::rng::{mix64, Rng};
use crate::routing::{flow_hash, PortMap, Routes};
use crate::stats::{Counters, FctRecord, LogHist};
use crate::time::{exact_ps_per_byte, ser_time, Time, US};
use crate::topology::Topology;
use crate::workload::{FlowSpec, FlowTag};
use std::collections::VecDeque;

/// ECN marking profile; thresholds scale with each port's link rate as in
/// the HPCC paper §5.1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EcnProfile {
    Off,
    /// Kmin = 100 KB x Bw/25G, Kmax = 400 KB x Bw/25G, Pmax = 0.2.
    Dcqcn,
    /// Kmin = Kmax = 30 KB x Bw/10G (DCTCP paper's K), mark with prob. 1.
    Dctcp,
}

impl EcnProfile {
    pub fn for_cc(cc: &CcKind) -> EcnProfile {
        match cc {
            CcKind::Dcqcn(_) => EcnProfile::Dcqcn,
            CcKind::Dctcp(_) => EcnProfile::Dctcp,
            _ => EcnProfile::Off,
        }
    }

    /// (Kmin, Kmax, Pmax) for a port of the given rate. KB = 1000 bytes.
    pub fn thresholds(&self, rate_bps: u64) -> Option<(u64, u64, f64)> {
        let r = rate_bps as u128;
        match self {
            EcnProfile::Off => None,
            EcnProfile::Dcqcn => Some((
                (100_000u128 * r / 25_000_000_000) as u64,
                (400_000u128 * r / 25_000_000_000) as u64,
                0.2,
            )),
            EcnProfile::Dctcp => {
                let k = (30_000u128 * r / 10_000_000_000) as u64;
                Some((k, k, 1.0))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub topo: Topology,
    pub cc: CcKind,
    pub seed: u64,
    /// Payload bytes per full data packet.
    pub mtu: u32,
    /// Header bytes of a data packet (Ethernet + IP + UDP + RoCE).
    pub hdr_bytes: u32,
    /// Wire size of an ACK/NACK without INT.
    pub ack_bytes: u32,
    /// Extra bytes carried by data packets and ACKs when INT is on.
    pub int_bytes: u32,
    pub pfc_frame_bytes: u32,
    /// Shared buffer per switch.
    pub buffer_bytes: u64,
    pub pfc: bool,
    pub ecn: EcnProfile,
    /// T, the base RTT the CC algorithms use for windows.
    pub base_rtt: Time,
    pub flows: Vec<FlowSpec>,
    /// Hard stop.
    pub stop_time: Time,
    /// If set, flows that start inside this window are "measured"; the run
    /// ends as soon as all of them have completed.
    pub measure: Option<(Time, Time)>,
    /// Sample every switch egress data queue at this interval.
    pub queue_sample_interval: Option<Time>,
    /// Record a time series of these (node, port) data queues.
    pub trace_ports: Vec<(u32, u16)>,
    /// Bin width for per-flow delivered-bytes time series of Micro flows and
    /// for the traced port queue time series.
    pub trace_bin: Time,
    /// Go-back-N retransmission timeout (only armed when PFC is off).
    pub rto: Time,
}

impl SimConfig {
    pub fn new(topo: Topology, cc: CcKind) -> Self {
        let ecn = EcnProfile::for_cc(&cc);
        SimConfig {
            topo,
            cc,
            seed: 1,
            mtu: 1000,
            hdr_bytes: 48,
            ack_bytes: 60,
            int_bytes: 42,
            pfc_frame_bytes: 64,
            buffer_bytes: 32 << 20,
            pfc: true,
            ecn,
            base_rtt: 13 * US,
            flows: Vec::new(),
            stop_time: Time::MAX,
            measure: None,
            queue_sample_interval: None,
            trace_ports: Vec::new(),
            trace_bin: 10 * US,
            rto: 500 * US,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Ev {
    TxDone { node: u32, port: u16 },
    Arrive { node: u32, port: u16, pkt: u32 },
    NicWake { host: u32 },
    FlowStart,
    Timer { flow: u32, kind: u8, gen: u32 },
    Sample,
}

const TIMER_RTO: u8 = 3;

pub struct Port {
    pub peer: u32,
    pub peer_port: u16,
    pub rate_bps: u64,
    pub delay: Time,
    ps_per_byte: u64,
    ctrl_q: VecDeque<u32>,
    data_q: VecDeque<u32>,
    pub ctrl_bytes: u64,
    pub data_bytes: u64,
    busy: bool,
    /// Data priority paused by the peer's PFC.
    pub paused: bool,
    /// Cumulative bytes transmitted (the INT txBytes counter).
    pub tx_bytes: u64,
    paused_since: Time,
    pub paused_total: Time,
    pub pauses_received: u64,
    ecn: Option<(u64, u64, f64)>,
    pub max_data_bytes: u64,
}

impl Port {
    #[inline]
    fn ser(&self, bytes: u32) -> Time {
        if self.ps_per_byte > 0 {
            bytes as u64 * self.ps_per_byte
        } else {
            ser_time(bytes as u64, self.rate_bps)
        }
    }
}

struct SwitchState {
    mmu: Mmu,
    ecmp_seed: u64,
    rng: Rng,
}

#[derive(Default)]
struct HostState {
    active: Vec<u32>,
    rr: usize,
    wake_at: Option<Time>,
}

pub struct Flow {
    pub spec: FlowSpec,
    pub hash: u64,
    pub ack_hash: u64,
    pub snd_nxt: u64,
    pub snd_una: u64,
    pub ctl: Ctl,
    next_avail: Time,
    last_pkt_bytes: u32,
    /// Allocated when the flow starts (most flows of a long run have not).
    cc: Option<Box<CcState>>,
    timer_gen: [u32; 4],
    pub started: bool,
    pub finished: bool,
    pub rcv_nxt: u64,
    last_nack_seq: u64,
    nack_until: Time,
    last_progress: Time,
    measured: bool,
    /// Delivered payload bytes per trace bin (Micro flows only).
    pub trace: Vec<u64>,
}

impl Flow {
    pub fn is_measured(&self) -> bool {
        self.measured
    }

    pub fn cc_state(&self) -> Option<&CcState> {
        self.cc.as_deref()
    }
}

pub struct Sim {
    pub cfg: SimConfig,
    q: EventQueue<Ev>,
    pool: PacketPool,
    pub ports: Vec<Vec<Port>>,
    switches: Vec<SwitchState>,
    hosts: Vec<HostState>,
    pub flows: Vec<Flow>,
    routes: Routes,
    n_hosts: u32,
    next_flow: usize,
    measured_left: usize,
    int_enabled: bool,
    pub fct: Vec<FctRecord>,
    pub counters: Counters,
    /// Sampled switch egress data-queue lengths (bytes).
    pub queue_hist: LogHist,
    /// Per-packet RTT samples (ps), measured at the sender on each ACK.
    pub rtt_hist: LogHist,
    /// Time series of traced port queues: one Vec per traced port.
    pub port_trace: Vec<Vec<u64>>,
    pub end_time: Time,
}

impl Sim {
    pub fn new(mut cfg: SimConfig) -> Sim {
        cfg.topo.validate().expect("invalid topology");
        assert!(cfg.mtu > 0 && cfg.base_rtt > 0);
        let topo = &cfg.topo;
        let pm = PortMap::new(topo);
        let routes = Routes::new(topo, &pm);
        let n_hosts = topo.n_hosts;
        let nic_rate = topo.links.iter().filter(|l| topo.is_host(l.a) || topo.is_host(l.b)).map(|l| l.rate_bps).min().unwrap();
        let mut ports = Vec::with_capacity(topo.n_nodes() as usize);
        for (node, plist) in pm.ports.iter().enumerate() {
            let is_switch = node as u32 >= n_hosts;
            let v: Vec<Port> = plist
                .iter()
                .map(|&(peer, peer_port, li)| {
                    let l = &topo.links[li as usize];
                    Port {
                        peer,
                        peer_port,
                        rate_bps: l.rate_bps,
                        delay: l.delay,
                        ps_per_byte: exact_ps_per_byte(l.rate_bps).unwrap_or(0),
                        ctrl_q: VecDeque::new(),
                        data_q: VecDeque::new(),
                        ctrl_bytes: 0,
                        data_bytes: 0,
                        busy: false,
                        paused: false,
                        tx_bytes: 0,
                        paused_since: 0,
                        paused_total: 0,
                        pauses_received: 0,
                        ecn: if is_switch { cfg.ecn.thresholds(l.rate_bps) } else { None },
                        max_data_bytes: 0,
                    }
                })
                .collect();
            ports.push(v);
        }
        let mut switches = Vec::new();
        for s in n_hosts..topo.n_nodes() {
            let plist = &ports[s as usize];
            // PFC headroom: 3 x rate x one-way delay; dynamic-threshold alpha
            // 1/8 for NIC-rate ports, doubled per doubling of the port rate
            // (authors' simulator, scratch/third.cc).
            let headroom = plist.iter().map(|p| (p.rate_bps as u128 * p.delay as u128 * 3 / (8 * crate::time::SEC as u128)) as u64).collect();
            let alpha_shift = plist
                .iter()
                .map(|p| {
                    let mut shift = 3u32;
                    let mut r = p.rate_bps;
                    while r > nic_rate && shift > 0 {
                        shift -= 1;
                        r /= 2;
                    }
                    shift
                })
                .collect();
            let mmu = Mmu::new(MmuConfig {
                buffer_bytes: cfg.buffer_bytes,
                reserve: 4 * 1024,
                resume_offset: 3 * 1024,
                headroom,
                alpha_shift,
                pfc_enabled: cfg.pfc,
            });
            switches.push(SwitchState {
                mmu,
                ecmp_seed: mix64(cfg.seed ^ mix64(s as u64 + 0x5157)),
                rng: Rng::stream(cfg.seed, 0xEC4 + s as u64),
            });
        }
        // Flows sorted by start time (stable), ids are positions.
        let mut specs = std::mem::take(&mut cfg.flows);
        specs.sort_by_key(|f| f.start);
        let mut measured_left = 0;
        let flows: Vec<Flow> = specs
            .into_iter()
            .enumerate()
            .map(|(i, spec)| {
                assert!(spec.src < n_hosts && spec.dst < n_hosts && spec.src != spec.dst && spec.size > 0, "bad flow {spec:?}");
                let measured = match cfg.measure {
                    Some((a, b)) => spec.start >= a && spec.start < b,
                    None => true,
                };
                measured_left += measured as usize;
                // Ports: sport = flow id (unique), dport = 100 + tag.
                let dport = 100 + spec.tag as u32;
                Flow {
                    hash: flow_hash(spec.src, spec.dst, i as u32, dport),
                    ack_hash: flow_hash(spec.dst, spec.src, dport, i as u32),
                    spec,
                    snd_nxt: 0,
                    snd_una: 0,
                    ctl: Ctl { rate_bps: 0, win: 0 },
                    next_avail: 0,
                    last_pkt_bytes: 0,
                    cc: None,
                    timer_gen: [0; 4],
                    started: false,
                    finished: false,
                    rcv_nxt: 0,
                    last_nack_seq: u64::MAX,
                    nack_until: 0,
                    last_progress: 0,
                    measured,
                    trace: Vec::new(),
                }
            })
            .collect();
        let int_enabled = cfg.cc.uses_int();
        let mut q = EventQueue::new();
        if let Some(f) = flows.first() {
            q.schedule(f.spec.start, Ev::FlowStart);
        }
        if let Some(iv) = cfg.queue_sample_interval.or(if cfg.trace_ports.is_empty() { None } else { Some(cfg.trace_bin) }) {
            q.schedule(iv, Ev::Sample);
        }
        let n_trace = cfg.trace_ports.len();
        Sim {
            q,
            pool: PacketPool::default(),
            ports,
            switches,
            hosts: (0..n_hosts).map(|_| HostState::default()).collect(),
            flows,
            routes,
            n_hosts,
            next_flow: 0,
            measured_left,
            int_enabled,
            fct: Vec::new(),
            counters: Counters::default(),
            queue_hist: LogHist::default(),
            rtt_hist: LogHist::default(),
            port_trace: vec![Vec::new(); n_trace],
            end_time: 0,
            cfg,
        }
    }

    pub fn now(&self) -> Time {
        self.q.now()
    }

    pub fn events_processed(&self) -> u64 {
        self.q.processed()
    }

    /// Runs until all (measured) flows complete or the stop time passes.
    pub fn run(&mut self) {
        self.run_until(self.cfg.stop_time);
        self.finish_stats();
    }

    /// Processes events with time <= `t` (or until the run is complete).
    /// Returns true if the run is complete.
    pub fn run_until(&mut self, t: Time) -> bool {
        loop {
            if self.measured_left == 0 {
                return true;
            }
            match self.q.peek_time() {
                None => return true,
                Some(nt) if nt > t => return false,
                _ => {}
            }
            let (_, ev) = self.q.pop().unwrap();
            self.handle(ev);
        }
    }

    fn finish_stats(&mut self) {
        let now = self.q.now();
        self.end_time = now;
        for plist in &mut self.ports {
            for p in plist {
                if p.paused {
                    p.paused_total += now - p.paused_since;
                    p.paused_since = now;
                }
            }
        }
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::TxDone { node, port } => {
                self.ports[node as usize][port as usize].busy = false;
                if node < self.n_hosts {
                    self.host_try_send(node);
                } else {
                    self.switch_try_tx(node, port);
                }
            }
            Ev::Arrive { node, port, pkt } => {
                if node < self.n_hosts {
                    self.host_receive(node, pkt);
                } else {
                    self.switch_receive(node, port, pkt);
                }
            }
            Ev::NicWake { host } => {
                let now = self.q.now();
                if self.hosts[host as usize].wake_at == Some(now) {
                    self.hosts[host as usize].wake_at = None;
                }
                self.host_try_send(host);
            }
            Ev::FlowStart => {
                let id = self.next_flow;
                self.next_flow += 1;
                if let Some(f) = self.flows.get(self.next_flow) {
                    self.q.schedule(f.spec.start, Ev::FlowStart);
                }
                self.start_flow(id as u32);
            }
            Ev::Timer { flow, kind, gen } => self.on_timer(flow, kind, gen),
            Ev::Sample => self.sample(),
        }
    }

    // ---------------------------------------------------------------- links

    fn transmit(&mut self, node: u32, port: u16, pkt: u32) {
        let now = self.q.now();
        let size = self.pool.get(pkt).size;
        let p = &mut self.ports[node as usize][port as usize];
        debug_assert!(!p.busy);
        p.busy = true;
        p.tx_bytes += size as u64;
        let ser = p.ser(size);
        let (peer, peer_port, delay) = (p.peer, p.peer_port, p.delay);
        self.q.schedule(now + ser, Ev::TxDone { node, port });
        self.q.schedule(now + ser + delay, Ev::Arrive { node: peer, port: peer_port, pkt });
    }

    fn on_pfc_frame(&mut self, node: u32, port: u16, kind: Kind) {
        let now = self.q.now();
        let p = &mut self.ports[node as usize][port as usize];
        match kind {
            Kind::Pause => {
                if !p.paused {
                    p.paused = true;
                    p.paused_since = now;
                }
                p.pauses_received += 1;
            }
            Kind::Resume => {
                if p.paused {
                    p.paused = false;
                    p.paused_total += now - p.paused_since;
                }
            }
            _ => unreachable!(),
        }
    }

    // ---------------------------------------------------------------- hosts

    fn start_flow(&mut self, id: u32) {
        let now = self.q.now();
        let line = self.ports[self.flows[id as usize].spec.src as usize][0].rate_bps;
        let env = FlowEnv { line_rate_bps: line, base_rtt: self.cfg.base_rtt, mtu: self.cfg.mtu };
        let (cc, ctl) = CcState::new(&self.cfg.cc, &env);
        let f = &mut self.flows[id as usize];
        f.cc = Some(Box::new(cc));
        f.ctl = ctl;
        f.started = true;
        f.next_avail = now;
        f.last_progress = now;
        let src = f.spec.src;
        self.hosts[src as usize].active.push(id);
        if !self.cfg.pfc {
            self.arm_timer(id, TIMER_RTO, self.cfg.rto);
        }
        self.host_try_send(src);
    }

    fn host_try_send(&mut self, h: u32) {
        let now = self.q.now();
        let port = &mut self.ports[h as usize][0];
        if port.busy {
            return;
        }
        if let Some(pkt) = port.ctrl_q.pop_front() {
            port.ctrl_bytes -= self.pool.get(pkt).size as u64;
            self.transmit(h, 0, pkt);
            return;
        }
        if port.paused {
            return;
        }
        let hs = &mut self.hosts[h as usize];
        let n = hs.active.len();
        let mut chosen = None;
        let mut wake = Time::MAX;
        for k in 0..n {
            let idx = (hs.rr + k) % n;
            let f = &self.flows[hs.active[idx] as usize];
            if f.snd_nxt >= f.spec.size {
                continue; // everything sent, waiting for ACKs
            }
            if f.ctl.win > 0 && f.snd_nxt - f.snd_una >= f.ctl.win {
                continue; // window-limited; an ACK will wake us
            }
            if f.next_avail > now {
                wake = wake.min(f.next_avail);
                continue;
            }
            chosen = Some(idx);
            break;
        }
        if let Some(idx) = chosen {
            hs.rr = (idx + 1) % n;
            let id = hs.active[idx];
            self.send_data(id);
        } else if wake != Time::MAX && hs.wake_at.is_none_or(|w| wake < w) {
            hs.wake_at = Some(wake);
            self.q.schedule(wake, Ev::NicWake { host: h });
        }
    }

    fn send_data(&mut self, id: u32) {
        let now = self.q.now();
        let int = if self.int_enabled { self.cfg.int_bytes } else { 0 };
        let f = &mut self.flows[id as usize];
        let payload = (f.spec.size - f.snd_nxt).min(self.cfg.mtu as u64) as u32;
        let size = payload + self.cfg.hdr_bytes + int;
        let pkt = Packet {
            kind: Kind::Data,
            ecn: false,
            n_hops: 0,
            in_port: 0,
            flow: id,
            src: f.spec.src,
            dst: f.spec.dst,
            size,
            payload,
            seq: f.snd_nxt,
            ts: now,
            hash: f.hash,
            hops: [IntHop::default(); MAX_HOPS],
        };
        f.snd_nxt += payload as u64;
        f.next_avail = now + ser_time(size as u64, f.ctl.rate_bps);
        f.last_pkt_bytes = size;
        let src = f.spec.src;
        self.counters.data_bytes_injected += size as u64;
        self.counters.data_pkts_injected += 1;
        let idx = self.pool.alloc(pkt);
        self.transmit(src, 0, idx);
    }

    fn host_receive(&mut self, h: u32, pkt: u32) {
        let kind = self.pool.get(pkt).kind;
        match kind {
            Kind::Pause | Kind::Resume => {
                self.pool.free(pkt);
                self.on_pfc_frame(h, 0, kind);
                if kind == Kind::Resume {
                    self.host_try_send(h);
                }
            }
            Kind::Data => self.receive_data(h, pkt),
            Kind::Ack => self.receive_ack(pkt),
            Kind::Nack => self.receive_nack(pkt),
        }
    }

    fn receive_data(&mut self, h: u32, pkt: u32) {
        let now = self.q.now();
        let int = if self.int_enabled { self.cfg.int_bytes } else { 0 };
        let (ack_size, nack_size) = (self.cfg.ack_bytes + int, self.cfg.ack_bytes);
        let p = self.pool.get(pkt);
        debug_assert_eq!(p.dst, h);
        let (fid, seq, payload, size) = (p.flow, p.seq, p.payload, p.size);
        self.counters.data_bytes_received += size as u64;
        let trace_bin = self.cfg.trace_bin;
        let f = &mut self.flows[fid as usize];
        let reply = if seq == f.rcv_nxt {
            f.rcv_nxt += payload as u64;
            if f.spec.tag == FlowTag::Micro {
                let bin = (now / trace_bin) as usize;
                if f.trace.len() <= bin {
                    f.trace.resize(bin + 1, 0);
                }
                f.trace[bin] += payload as u64;
            }
            Some(Kind::Ack)
        } else if seq > f.rcv_nxt {
            // Out of order: only after a drop (PFC off). Go-back-N NACK, at
            // most one per expected sequence number per 100 us.
            if f.last_nack_seq != f.rcv_nxt || now >= f.nack_until {
                f.last_nack_seq = f.rcv_nxt;
                f.nack_until = now + 100 * US;
                Some(Kind::Nack)
            } else {
                None
            }
        } else {
            None // duplicate
        };
        let (rcv_nxt, ack_hash, sender) = (f.rcv_nxt, f.ack_hash, f.spec.src);
        match reply {
            None => self.pool.free(pkt),
            Some(kind) => {
                // Turn the data packet into its ACK in place: the INT stack
                // and ECN mark are copied by the receiver (HPCC Fig. 4).
                let p = self.pool.get_mut(pkt);
                p.kind = kind;
                p.size = if kind == Kind::Ack { ack_size } else { nack_size };
                p.payload = 0;
                p.seq = rcv_nxt;
                p.src = h;
                p.dst = sender;
                p.hash = ack_hash;
                if kind == Kind::Nack {
                    p.n_hops = 0;
                }
                let port = &mut self.ports[h as usize][0];
                port.ctrl_q.push_back(pkt);
                port.ctrl_bytes += p.size as u64;
                self.host_try_send(h);
            }
        }
    }

    fn receive_ack(&mut self, pkt: u32) {
        let now = self.q.now();
        let fid = self.pool.get(pkt).flow;
        if self.flows[fid as usize].finished {
            self.pool.free(pkt);
            return;
        }
        let p = self.pool.get(pkt);
        let f = &mut self.flows[fid as usize];
        let rtt = now - p.ts;
        self.rtt_hist.add(rtt);
        if p.seq > f.snd_una {
            f.snd_una = p.seq;
            f.last_progress = now;
        }
        let view = AckView { now, ack_seq: p.seq, snd_nxt: f.snd_nxt, ecn: p.ecn, hops: p.int_hops(), rtt };
        let old = f.ctl;
        let mut timers = TimerReqs::default();
        f.cc.as_mut().expect("ack for a flow that never started").on_ack(&view, &mut f.ctl, &mut timers);
        self.pool.free(pkt);
        self.after_cc_update(fid, old, &timers);
        let f = &self.flows[fid as usize];
        if f.snd_una >= f.spec.size {
            self.finish_flow(fid);
        }
        let src = self.flows[fid as usize].spec.src;
        self.host_try_send(src);
    }

    fn receive_nack(&mut self, pkt: u32) {
        let now = self.q.now();
        let p = self.pool.get(pkt);
        let (fid, seq) = (p.flow, p.seq);
        self.pool.free(pkt);
        let f = &mut self.flows[fid as usize];
        if f.finished {
            return;
        }
        if seq > f.snd_una {
            f.snd_una = seq;
            f.last_progress = now;
        }
        if f.snd_nxt > seq {
            f.snd_nxt = seq.max(f.snd_una);
            self.counters.retransmit_events += 1;
        }
        let src = f.spec.src;
        self.host_try_send(src);
    }

    /// Applies a CC decision: re-arms timers and, like the authors' NIC
    /// model, shifts the pending pacing slot when the rate changes.
    fn after_cc_update(&mut self, fid: u32, old: Ctl, timers: &TimerReqs) {
        let f = &mut self.flows[fid as usize];
        if f.ctl.rate_bps != old.rate_bps && f.last_pkt_bytes > 0 {
            let old_gap = ser_time(f.last_pkt_bytes as u64, old.rate_bps);
            let new_gap = ser_time(f.last_pkt_bytes as u64, f.ctl.rate_bps);
            f.next_avail = (f.next_avail + new_gap).saturating_sub(old_gap);
        }
        for &(kind, delay) in timers.iter() {
            self.arm_timer(fid, kind, delay);
        }
    }

    fn arm_timer(&mut self, fid: u32, kind: u8, delay: Time) {
        let f = &mut self.flows[fid as usize];
        f.timer_gen[kind as usize] = f.timer_gen[kind as usize].wrapping_add(1);
        let gen = f.timer_gen[kind as usize];
        let t = self.q.now() + delay;
        self.q.schedule(t, Ev::Timer { flow: fid, kind, gen });
    }

    fn on_timer(&mut self, fid: u32, kind: u8, gen: u32) {
        let now = self.q.now();
        let f = &mut self.flows[fid as usize];
        if f.finished || f.timer_gen[kind as usize] != gen {
            return; // cancelled
        }
        if kind == TIMER_RTO {
            if f.snd_una < f.snd_nxt && now - f.last_progress >= self.cfg.rto {
                f.snd_nxt = f.snd_una;
                f.last_progress = now;
                self.counters.retransmit_events += 1;
            }
            let src = f.spec.src;
            self.arm_timer(fid, TIMER_RTO, self.cfg.rto);
            self.host_try_send(src);
            return;
        }
        let old = f.ctl;
        let mut timers = TimerReqs::default();
        f.cc.as_mut().expect("timer for a flow that never started").on_timer(kind, now, &mut f.ctl, &mut timers);
        let src = f.spec.src;
        self.after_cc_update(fid, old, &timers);
        self.host_try_send(src);
    }

    fn finish_flow(&mut self, fid: u32) {
        let now = self.q.now();
        let ideal = self.ideal_fct(fid);
        let f = &mut self.flows[fid as usize];
        f.finished = true;
        for g in &mut f.timer_gen {
            *g = g.wrapping_add(1);
        }
        self.fct.push(FctRecord {
            flow: fid,
            src: f.spec.src,
            dst: f.spec.dst,
            size: f.spec.size,
            start: f.spec.start,
            fct: now - f.spec.start,
            ideal,
            tag: f.spec.tag,
        });
        if f.measured {
            self.measured_left -= 1;
        }
        let hs = &mut self.hosts[f.spec.src as usize];
        let pos = hs.active.iter().position(|&x| x == fid).expect("finished flow not active");
        hs.active.remove(pos);
        if hs.rr > pos {
            hs.rr -= 1;
        }
        if hs.rr >= hs.active.len() {
            hs.rr = 0;
        }
    }

    // ------------------------------------------------------------- switches

    fn switch_receive(&mut self, s: u32, in_port: u16, pkt: u32) {
        let p = self.pool.get(pkt);
        match p.kind {
            Kind::Pause | Kind::Resume => {
                let kind = p.kind;
                self.pool.free(pkt);
                self.on_pfc_frame(s, in_port, kind);
                if kind == Kind::Resume {
                    self.switch_try_tx(s, in_port);
                }
            }
            Kind::Ack | Kind::Nack => {
                let sw = &self.switches[(s - self.n_hosts) as usize];
                let out = self.routes.pick(s, p.dst, p.hash, sw.ecmp_seed);
                let size = p.size as u64;
                let port = &mut self.ports[s as usize][out as usize];
                port.ctrl_q.push_back(pkt);
                port.ctrl_bytes += size;
                self.switch_try_tx(s, out);
            }
            Kind::Data => {
                let si = (s - self.n_hosts) as usize;
                let out = self.routes.pick(s, p.dst, p.hash, self.switches[si].ecmp_seed);
                let size = p.size as u64;
                let mmu = &mut self.switches[si].mmu;
                if mmu.admit(in_port as usize, size) == Admit::Dropped {
                    self.counters.drops += 1;
                    self.counters.dropped_bytes += size;
                    self.pool.free(pkt);
                    return;
                }
                mmu.enqueue(in_port as usize, out as usize, size);
                let send_pause = mmu.should_pause(in_port as usize);
                if send_pause {
                    mmu.set_paused(in_port as usize, true);
                }
                self.pool.get_mut(pkt).in_port = in_port;
                let port = &mut self.ports[s as usize][out as usize];
                port.data_q.push_back(pkt);
                port.data_bytes += size;
                port.max_data_bytes = port.max_data_bytes.max(port.data_bytes);
                if send_pause {
                    self.send_pfc(s, in_port, Kind::Pause);
                }
                self.switch_try_tx(s, out);
            }
        }
    }

    fn send_pfc(&mut self, s: u32, port: u16, kind: Kind) {
        let pkt = self.pool.alloc(Packet::control(kind, self.cfg.pfc_frame_bytes));
        match kind {
            Kind::Pause => self.counters.pause_frames += 1,
            _ => self.counters.resume_frames += 1,
        }
        let p = &mut self.ports[s as usize][port as usize];
        p.ctrl_q.push_back(pkt);
        p.ctrl_bytes += self.cfg.pfc_frame_bytes as u64;
        self.switch_try_tx(s, port);
    }

    fn switch_try_tx(&mut self, s: u32, port_i: u16) {
        let now = self.q.now();
        let port = &mut self.ports[s as usize][port_i as usize];
        if port.busy {
            return;
        }
        if let Some(pkt) = port.ctrl_q.pop_front() {
            port.ctrl_bytes -= self.pool.get(pkt).size as u64;
            self.transmit(s, port_i, pkt);
            return;
        }
        if port.paused {
            return;
        }
        let Some(pkt) = port.data_q.pop_front() else { return };
        let p = self.pool.get_mut(pkt);
        let size = p.size as u64;
        port.data_bytes -= size;
        let si = (s - self.n_hosts) as usize;
        let sw = &mut self.switches[si];
        let in_port = p.in_port as usize;
        sw.mmu.dequeue(in_port, port_i as usize, size);
        // ECN: RED on the egress queue length at dequeue.
        if let Some((kmin, kmax, pmax)) = port.ecn {
            let qlen = sw.mmu.egress_bytes(port_i as usize);
            let mark = if qlen > kmax {
                true
            } else if qlen > kmin {
                sw.rng.uniform() < pmax * (qlen - kmin) as f64 / (kmax - kmin) as f64
            } else {
                false
            };
            if mark && !p.ecn {
                p.ecn = true;
                self.counters.ecn_marks += 1;
            }
        }
        // INT: stamp the egress port's state as the packet leaves (Fig. 7).
        if self.int_enabled && (p.n_hops as usize) < MAX_HOPS {
            p.hops[p.n_hops as usize] =
                IntHop { ts: now, tx_bytes: port.tx_bytes, qlen: port.data_bytes + port.ctrl_bytes, rate_bps: port.rate_bps };
            p.n_hops += 1;
        }
        let resume = sw.mmu.should_resume(in_port);
        if resume {
            sw.mmu.set_paused(in_port, false);
        }
        self.transmit(s, port_i, pkt);
        if resume {
            self.send_pfc(s, in_port as u16, Kind::Resume);
        }
    }

    // ---------------------------------------------------------------- stats

    fn sample(&mut self) {
        let now = self.q.now();
        if self.cfg.queue_sample_interval.is_some() {
            for s in self.n_hosts as usize..self.ports.len() {
                for p in &self.ports[s] {
                    self.queue_hist.add(p.data_bytes);
                }
            }
        }
        for (i, &(n, pi)) in self.cfg.trace_ports.iter().enumerate() {
            self.port_trace[i].push(self.ports[n as usize][pi as usize].data_bytes);
        }
        let iv = self.cfg.queue_sample_interval.unwrap_or(self.cfg.trace_bin);
        self.q.schedule(now + iv, Ev::Sample);
    }

    /// FCT of flow `fid` if it were alone in the network, sending at line
    /// rate without congestion control or INT: an exact store-and-forward
    /// recurrence over every packet and every hop of its actual path, plus
    /// the ACK of each packet on the actual reverse path.
    pub fn ideal_fct(&self, fid: u32) -> Time {
        let f = &self.flows[fid as usize];
        let fwd = self.path(f.spec.src, f.spec.dst, f.hash);
        let rev = self.path(f.spec.dst, f.spec.src, f.ack_hash);
        ideal_fct(f.spec.size, self.cfg.mtu, self.cfg.hdr_bytes, self.cfg.ack_bytes, &fwd, &rev)
    }

    /// Links (rate, delay) on the ECMP path a packet with `hash` takes.
    pub fn path(&self, src: u32, dst: u32, hash: u64) -> Vec<(u64, Time)> {
        let mut v = Vec::new();
        let mut node = src;
        let mut port = 0u16;
        loop {
            let p = &self.ports[node as usize][port as usize];
            v.push((p.rate_bps, p.delay));
            node = p.peer;
            if node == dst {
                return v;
            }
            assert!(node >= self.n_hosts, "path reached a wrong host");
            let sw = &self.switches[(node - self.n_hosts) as usize];
            port = self.routes.pick(node, dst, hash, sw.ecmp_seed);
        }
    }

    /// Byte conservation: every data byte injected by a host is either
    /// delivered to a host, dropped by a switch, or still in the network.
    pub fn check_conservation(&self) -> Result<(), String> {
        let in_net: u64 = self.pool.live_packets().filter(|p| p.kind == Kind::Data).map(|p| p.size as u64).sum();
        let c = &self.counters;
        if c.data_bytes_injected != c.data_bytes_received + c.dropped_bytes + in_net {
            return Err(format!(
                "injected {} != received {} + dropped {} + in network {}",
                c.data_bytes_injected, c.data_bytes_received, c.dropped_bytes, in_net
            ));
        }
        // Every switch's buffer accounting matches its queues.
        for (si, sw) in self.switches.iter().enumerate() {
            sw.mmu.check_invariants().map_err(|e| format!("switch {si}: {e}"))?;
            let queued: u64 = self.ports[self.n_hosts as usize + si].iter().map(|p| p.data_bytes).sum();
            if queued != sw.mmu.total_buffered() {
                return Err(format!("switch {si}: queues hold {queued} B but MMU accounts {}", sw.mmu.total_buffered()));
            }
        }
        Ok(())
    }

    /// Total time the data priority was paused, summed over host NIC ports
    /// and over switch ports.
    pub fn pause_time(&self) -> (Time, Time) {
        let mut host = 0;
        let mut sw = 0;
        for (n, plist) in self.ports.iter().enumerate() {
            for p in plist {
                if (n as u32) < self.n_hosts {
                    host += p.paused_total;
                } else {
                    sw += p.paused_total;
                }
            }
        }
        (host, sw)
    }

    pub fn switch_mmu_headroom_peak_ok(&self) -> bool {
        self.switches.iter().all(|s| s.mmu.check_invariants().is_ok())
    }

    pub fn n_switch_ports(&self) -> usize {
        self.ports[self.n_hosts as usize..].iter().map(|p| p.len()).sum()
    }

    pub fn n_hosts(&self) -> u32 {
        self.n_hosts
    }

    /// (measured flows, measured flows still unfinished).
    pub fn measured_counts(&self) -> (usize, usize) {
        (self.flows.iter().filter(|f| f.measured).count(), self.measured_left)
    }

    pub fn live_packets(&self) -> usize {
        self.pool.live_count()
    }
}

/// See [`Sim::ideal_fct`].
pub fn ideal_fct(size: u64, mtu: u32, hdr: u32, ack: u32, fwd: &[(u64, Time)], rev: &[(u64, Time)]) -> Time {
    let mut free_f = vec![0 as Time; fwd.len()];
    let mut free_r = vec![0 as Time; rev.len()];
    let mut sent = 0u64;
    let mut last_ack = 0;
    while sent < size {
        let payload = (size - sent).min(mtu as u64);
        sent += payload;
        let wire = payload + hdr as u64;
        // The sender's first link is busy back-to-back.
        let mut t = free_f[0];
        for (j, &(rate, delay)) in fwd.iter().enumerate() {
            let start = t.max(free_f[j]);
            let end = start + ser_time(wire, rate);
            free_f[j] = end;
            t = end + delay;
        }
        for (j, &(rate, delay)) in rev.iter().enumerate() {
            let start = t.max(free_r[j]);
            let end = start + ser_time(ack as u64, rate);
            free_r[j] = end;
            t = end + delay;
        }
        last_ack = t;
    }
    last_ack
}
