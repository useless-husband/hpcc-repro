//! Analytic and behavioural checks of each congestion-control algorithm
//! running on the network model.

use hpcc_sim::experiments::{cc_by_name, incast_n};
use hpcc_sim::sim::{Sim, SimConfig};
use hpcc_sim::time::{gbps, Time, MS, US};
use hpcc_sim::topology::{fat_tree, star, FatTreeParams};
use hpcc_sim::workload::{FlowSpec, FlowTag};

fn long_flow(src: u32, dst: u32, start: Time) -> FlowSpec {
    FlowSpec {
        src,
        dst,
        size: 1 << 40,
        start,
        tag: FlowTag::Micro,
    }
}

/// Goodput (Gbps) of each Micro flow averaged over trace bins [a, b).
fn goodput(sim: &Sim, a: usize, b: usize) -> Vec<f64> {
    let bin_s = sim.cfg.trace_bin as f64 * 1e-12;
    sim.flows
        .iter()
        .filter(|f| f.spec.tag == FlowTag::Micro)
        .map(|f| {
            (a..b)
                .map(|i| *f.trace.get(i).unwrap_or(&0) as f64)
                .sum::<f64>()
                * 8.0
                / (bin_s * (b - a) as f64)
                / 1e9
        })
        .collect()
}

fn jain(v: &[f64]) -> f64 {
    let s: f64 = v.iter().sum();
    s * s / (v.len() as f64 * v.iter().map(|x| x * x).sum::<f64>())
}

fn percentile(v: &[u64], p: f64) -> u64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s[((s.len() - 1) as f64 * p) as usize]
}

/// Four long flows join a 100 Gbps bottleneck 1 ms apart.
fn four_flows(cc: &str) -> Sim {
    let mut cfg = SimConfig::new(
        star(5, gbps(100), US),
        cc_by_name(cc, gbps(100), None).unwrap(),
    );
    cfg.base_rtt = 4_500_000;
    cfg.flows = (0..4).map(|i| long_flow(i, 4, i as u64 * MS)).collect();
    cfg.stop_time = 30 * MS;
    cfg.trace_bin = MS;
    cfg.trace_ports = vec![(5, 4)];
    let mut sim = Sim::new(cfg);
    sim.run();
    sim
}

#[test]
fn every_cc_converges_to_fair_share() {
    for cc in [
        "hpcc",
        "dcqcn",
        "dcqcn+win",
        "dctcp",
        "timely",
        "timely+win",
    ] {
        let sim = four_flows(cc);
        let g = goodput(&sim, 20, 30);
        let total: f64 = g.iter().sum();
        let j = jain(&g);
        assert!(
            j > 0.97,
            "{cc}: Jain index {j:.3} over 20-30 ms, goodput {g:.1?}"
        );
        assert!(total > 80.0, "{cc}: total goodput {total:.1} Gbps");
    }
}

#[test]
fn hpcc_steady_state_queue_is_near_zero() {
    let sim = four_flows("hpcc");
    let q = &sim.port_trace[0][5..];
    assert!(
        percentile(q, 0.99) < 10_000,
        "HPCC q99 {} B",
        percentile(q, 0.99)
    );
    // ... while DCQCN keeps a standing queue around its ECN thresholds.
    let sim = four_flows("dcqcn");
    let q = &sim.port_trace[0][5..];
    assert!(
        percentile(q, 0.5) > 50_000,
        "DCQCN median queue {} B",
        percentile(q, 0.5)
    );
}

#[test]
fn hpcc_drains_incast_in_a_few_rtts_without_overreacting() {
    // Paper §5.4 / Figure 13: 16-to-1 incast through one 100G switch.
    let run = |cc: &str| {
        let mut sim = Sim::new(incast_n(16, cc, None, 400 * US, 5 * US).unwrap());
        sim.run();
        sim
    };
    let hpcc = run("hpcc");
    let per_ack = run("hpcc-perack");
    let per_rtt = run("hpcc-perrtt");
    // Queue samples every 5 us.
    let q_at = |s: &Sim, us: usize| s.port_trace[0][us / 5 - 1];
    assert!(
        q_at(&hpcc, 10) > 500_000,
        "the initial burst builds a queue"
    );
    assert!(
        q_at(&hpcc, 150) < 20_000,
        "HPCC drained by 150 us: {}",
        q_at(&hpcc, 150)
    );
    assert!(
        q_at(&per_rtt, 100) > 4 * q_at(&hpcc, 100).max(50_000),
        "per-RTT reaction is slower"
    );
    let tot = |s: &Sim| goodput(s, 30, 80).iter().sum::<f64>();
    assert!(
        tot(&hpcc) > 80.0,
        "HPCC keeps the link busy: {:.1}",
        tot(&hpcc)
    );
    assert!(
        tot(&per_ack) < 0.85 * tot(&hpcc),
        "per-ACK overreacts: {:.1} vs {:.1}",
        tot(&per_ack),
        tot(&hpcc)
    );
}

#[test]
fn hpcc_reacts_to_the_last_hop_of_a_multi_hop_path() {
    // 8 senders in other pods send to one receiver: the bottleneck is the
    // receiver's ToR downlink, 5 switches into a 6-link path. HPCC must see
    // it through INT and keep the queue low after the initial burst.
    let topo = fat_tree(&FatTreeParams::small());
    let mut cfg = SimConfig::new(topo, cc_by_name("hpcc", gbps(100), None).unwrap());
    cfg.flows = (0..8).map(|s| long_flow(s, 31, 0)).collect();
    cfg.stop_time = 2 * MS;
    cfg.trace_bin = 10 * US;
    let tor = 32 + 3; // ToR of hosts 24..31
    cfg.trace_ports = vec![(tor, 7)];
    let mut sim = Sim::new(cfg);
    sim.run();
    let q = &sim.port_trace[0][30..]; // after 300 us
    assert!(
        percentile(q, 0.95) < 20_000,
        "q95 {} B",
        percentile(q, 0.95)
    );
    let g: f64 = goodput(&sim, 50, 200).iter().sum();
    assert!(g > 80.0, "bottleneck stays utilised: {g:.1} Gbps");
}

#[test]
fn a_sending_window_stops_dcqcn_from_triggering_pfc() {
    // Paper §5.3: "just adding a sending window to DCQCN ... reduces PFCs to
    // almost zero". The paper's incast (60 senders x 500 KB) into one port of
    // a switch with an 8 MB buffer: without a window, in-flight bytes are
    // unbounded and the ingress queues cross the PFC threshold; with a
    // window (T = 4.5 us here) each sender has at most ~56 KB in flight.
    let run = |cc: &str| {
        let mut cfg = SimConfig::new(
            star(61, gbps(100), US),
            cc_by_name(cc, gbps(100), None).unwrap(),
        );
        cfg.buffer_bytes = 8 << 20;
        cfg.base_rtt = 4_500_000;
        cfg.flows = (0..60)
            .map(|s| FlowSpec {
                src: s,
                dst: 60,
                size: 500_000,
                start: 0,
                tag: FlowTag::Background,
            })
            .collect();
        let mut sim = Sim::new(cfg);
        sim.run();
        assert_eq!(sim.fct.len(), 60);
        assert_eq!(sim.counters.drops, 0);
        sim
    };
    assert!(
        run("dcqcn").counters.pause_frames > 0,
        "plain DCQCN must hit PFC"
    );
    assert_eq!(run("dcqcn+win").counters.pause_frames, 0);
    assert_eq!(run("hpcc").counters.pause_frames, 0);
}

#[test]
fn dctcp_holds_the_queue_near_its_marking_threshold() {
    // DCTCP with K = 30 KB x 100/10 = 300 KB: 16 long flows keep a standing
    // queue around K (the DCTCP paper's operating point), well below the
    // ~16 x W_init the windows alone would allow.
    let mut sim = Sim::new(incast_n(16, "dctcp", None, 3 * MS, 100 * US).unwrap());
    sim.run();
    let q = &sim.port_trace[0][10..];
    let med = percentile(q, 0.5);
    assert!(
        (200_000..=400_000).contains(&med),
        "median queue {med} B, K = 300 KB"
    );
    let g: f64 = goodput(&sim, 10, 30).iter().sum();
    assert!(g > 90.0, "DCTCP keeps the link full: {g:.1} Gbps");
}
