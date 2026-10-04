//! Validation of the network model itself (no congestion control): timing,
//! PFC, buffer accounting, ECMP, byte conservation and determinism.

use hpcc_sim::cc::CcKind;
use hpcc_sim::experiments::cc_by_name;
use hpcc_sim::rng::Rng;
use hpcc_sim::sim::{ideal_fct, Sim, SimConfig};
use hpcc_sim::time::{gbps, ser_time, Time, MS, NS, US};
use hpcc_sim::topology::{chain, fat_tree, star, FatTreeParams, Topology};
use hpcc_sim::workload::{poisson, FlowSpec, FlowTag, FB_HADOOP};

fn flow(src: u32, dst: u32, size: u64, start: Time) -> FlowSpec {
    FlowSpec {
        src,
        dst,
        size,
        start,
        tag: FlowTag::Background,
    }
}

fn run_one(topo: Topology, cc: CcKind, flows: Vec<FlowSpec>) -> Sim {
    let mut cfg = SimConfig::new(topo, cc);
    cfg.flows = flows;
    cfg.stop_time = 100 * MS;
    let mut sim = Sim::new(cfg);
    sim.run();
    sim
}

#[test]
fn single_packet_latency_is_analytic() {
    // One 1-byte flow across one switch: 49-byte packet, 2 hops of 100G and
    // 1 us each way; the 60-byte ACK comes back the same way.
    let sim = run_one(star(2, gbps(100), US), CcKind::None, vec![flow(0, 1, 1, 0)]);
    let data = 2 * ser_time(49, gbps(100)) + 2 * US;
    let ack = 2 * ser_time(60, gbps(100)) + 2 * US;
    assert_eq!(sim.fct.len(), 1);
    assert_eq!(sim.fct[0].fct, data + ack);
    assert_eq!(
        data + ack,
        4 * US + 2 * 3_920 + 2 * 4_800,
        "49 B = 3.92 ns, 60 B = 4.8 ns at 100G"
    );
    // The RTT histogram (log buckets, <0.8% wide) holds that one sample.
    let q = sim.rtt_hist.quantile(1.0);
    assert!(
        q <= sim.fct[0].fct && (sim.fct[0].fct - q) * 128 <= sim.fct[0].fct,
        "rtt bucket {q}"
    );
}

#[test]
fn lone_flow_fct_equals_exact_ideal_everywhere() {
    // A lone line-rate flow must achieve exactly the store-and-forward
    // ideal, for odd sizes (partial last packet) and multi-rate paths.
    let sizes = [1u64, 999, 1000, 1001, 2048, 77_777, 1_000_000, 3_000_001];
    let topos = vec![
        (star(2, gbps(100), US), 0u32, 1u32),
        (chain(3, 1, gbps(25), gbps(100), 3 * US), 0, 2),
        (fat_tree(&FatTreeParams::small()), 0, 31), // cross-pod, 6 hops
        (fat_tree(&FatTreeParams::paper()), 5, 300),
    ];
    for (topo, s, d) in topos {
        for &size in &sizes {
            let sim = run_one(topo.clone(), CcKind::None, vec![flow(s, d, size, 7 * NS)]);
            let r = &sim.fct[0];
            assert_eq!(
                r.fct, r.ideal,
                "{} size {size}: fct {} ideal {}",
                topo.name, r.fct, r.ideal
            );
        }
    }
}

#[test]
fn ideal_matches_closed_form_for_uniform_path() {
    // For equal-rate hops and full packets, FCT = n*ser + (h-1)*ser + h*d
    // + h*(ser_ack + d) (pipeline fill plus one ACK).
    let h = 4;
    let path = vec![(gbps(100), US); h];
    let n = 10u64;
    let ser = ser_time(1048, gbps(100));
    let expect =
        n * ser + (h as u64 - 1) * ser + h as u64 * US + h as u64 * (ser_time(60, gbps(100)) + US);
    assert_eq!(ideal_fct(n * 1000, 1000, 48, 60, &path, &path), expect);
}

#[test]
fn ideal_steady_state_jump_is_exact_for_huge_flows() {
    // 10^9 full packets (1 TB): the recurrence skips the steady state, and
    // must still equal the closed form exactly.
    let h = 6;
    let path = vec![(gbps(100), US); h];
    let n = 1_000_000_000u64;
    let ser = ser_time(1048, gbps(100));
    let expect =
        n * ser + (h as u64 - 1) * ser + h as u64 * US + h as u64 * (ser_time(60, gbps(100)) + US);
    assert_eq!(ideal_fct(n * 1000, 1000, 48, 60, &path, &path), expect);
}

#[test]
fn single_long_flow_reaches_line_rate_for_every_cc() {
    // 10 MB alone on a 100G switch. Non-HPCC schemes have no reason to slow
    // down; HPCC keeps 5% headroom (eta) and pays 42 B of INT per packet.
    for name in [
        "none",
        "dcqcn",
        "dcqcn+win",
        "dctcp",
        "timely",
        "timely+win",
        "hpcc",
    ] {
        let cc = cc_by_name(name, gbps(100), None).unwrap();
        let mut cfg = SimConfig::new(star(2, gbps(100), US), cc);
        cfg.base_rtt = 4_500 * NS;
        cfg.flows = vec![flow(0, 1, 10_000_000, 0)];
        let mut sim = Sim::new(cfg);
        sim.run();
        let r = &sim.fct[0];
        let slow = r.fct as f64 / r.ideal as f64;
        if name == "hpcc" {
            let floor = (1090.0 / 1048.0) / 0.95;
            assert!(
                slow > floor * 0.98 && slow < floor * 1.05,
                "{name}: slowdown {slow:.4}, expected ~{floor:.4}"
            );
        } else {
            assert!(slow < 1.005, "{name}: slowdown {slow:.4}");
        }
    }
}

#[test]
fn pfc_prevents_all_drops_under_line_rate_incast() {
    // 15 line-rate senders (no CC) into one port: the switch must pause,
    // never drop, and conserve bytes; all flows complete.
    let flows: Vec<_> = (0..15)
        .map(|s| flow(s, 15, 2_000_000, s as u64 * 100 * NS))
        .collect();
    let mut cfg = SimConfig::new(star(16, gbps(100), US), CcKind::None);
    cfg.buffer_bytes = 4 << 20;
    cfg.flows = flows;
    let mut sim = Sim::new(cfg);
    let mut t = 0;
    while !sim.run_until(t) {
        sim.check_conservation()
            .unwrap_or_else(|e| panic!("t={t}: {e}"));
        t += 20 * US;
    }
    sim.run();
    assert_eq!(sim.counters.drops, 0);
    assert!(
        sim.counters.pause_frames > 0,
        "the test must actually trigger PFC"
    );
    assert_eq!(
        sim.counters.pause_frames, sim.counters.resume_frames,
        "every pause is resumed"
    );
    assert_eq!(sim.fct.len(), 15);
    sim.check_conservation().unwrap();
    assert_eq!(sim.live_packets(), 0, "nothing left in the network");
    let (host_pause, _) = sim.pause_time();
    assert!(host_pause > 0);
}

#[test]
fn without_pfc_the_same_incast_drops_and_go_back_n_recovers() {
    let flows: Vec<_> = (0..15)
        .map(|s| flow(s, 15, 2_000_000, s as u64 * 100 * NS))
        .collect();
    let mut cfg = SimConfig::new(star(16, gbps(100), US), CcKind::None);
    cfg.buffer_bytes = 4 << 20;
    cfg.pfc = false;
    cfg.flows = flows;
    let mut sim = Sim::new(cfg);
    let mut t = 0;
    while !sim.run_until(t) {
        sim.check_conservation()
            .unwrap_or_else(|e| panic!("t={t}: {e}"));
        t += 50 * US;
    }
    sim.run();
    assert!(sim.counters.drops > 0, "lossy mode must drop");
    assert_eq!(sim.counters.pause_frames, 0);
    assert_eq!(
        sim.fct.len(),
        15,
        "go-back-N delivers everything eventually"
    );
    sim.check_conservation().unwrap();
}

#[test]
fn pfc_propagates_hop_by_hop_without_loss() {
    // Incast into a host behind a 3-switch chain whose middle links are only
    // 100G: congestion at the last switch must spread back via PFC with no
    // drops anywhere, even with a small buffer.
    let topo = chain(3, 6, gbps(100), gbps(100), US);
    let dst = 17; // a host on the last switch
    let flows: Vec<_> = (0..12).map(|s| flow(s, dst, 1_000_000, 0)).collect();
    let mut cfg = SimConfig::new(topo, CcKind::None);
    cfg.buffer_bytes = 2 << 20;
    cfg.flows = flows;
    let mut sim = Sim::new(cfg);
    sim.run();
    assert_eq!(sim.counters.drops, 0);
    assert!(sim.counters.pause_frames > 0);
    let (host_pause, sw_pause) = sim.pause_time();
    assert!(
        host_pause > 0 && sw_pause > 0,
        "pauses reach both switches and hosts"
    );
    assert_eq!(sim.fct.len(), 12);
    sim.check_conservation().unwrap();
}

#[test]
fn byte_conservation_randomised() {
    // Random workloads on random small topologies, with and without PFC and
    // with different CCs; conservation and buffer accounting are checked at
    // many points during each run. Seeds are printed on failure.
    for seed in 0..12u64 {
        let mut rng = Rng::new(seed);
        let topo = match seed % 3 {
            0 => star(8, gbps(100), US),
            1 => chain(3, 3, gbps(100), gbps(100), US),
            _ => fat_tree(&FatTreeParams::small()),
        };
        let n = topo.n_hosts;
        let ccs = ["none", "hpcc", "dcqcn", "dctcp", "timely"];
        let cc_name = ccs[rng.below(ccs.len() as u64) as usize];
        let mut cfg = SimConfig::new(topo, cc_by_name(cc_name, gbps(100), None).unwrap());
        cfg.seed = seed;
        cfg.pfc = seed % 4 != 3;
        cfg.buffer_bytes = 2 << 20;
        cfg.flows = poisson(n, &FB_HADOOP, 0.6, gbps(100), 2 * MS, &mut rng);
        cfg.stop_time = 60 * MS;
        let mut sim = Sim::new(cfg);
        let mut t = 0;
        while !sim.run_until(t) {
            if let Err(e) = sim.check_conservation() {
                panic!("seed {seed} ({cc_name}) t={t}: {e}");
            }
            t += 37 * US;
        }
        sim.run();
        sim.check_conservation()
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(
            sim.fct.len(),
            sim.flows.len(),
            "seed {seed} ({cc_name}): all flows complete"
        );
        if sim.cfg.pfc {
            assert_eq!(sim.counters.drops, 0, "seed {seed}: PFC on means no drops");
        }
    }
}

#[test]
fn ecmp_keeps_flows_on_one_path_and_uses_all_paths() {
    // Many flows between two pods: no reordering (no NACK ever needed with
    // PFC on), and the ToR's four uplinks all carry traffic.
    let topo = fat_tree(&FatTreeParams::paper());
    let flows: Vec<_> = (0..64)
        .map(|i| flow(i % 16, 64 + (i % 16), 50_000, i as u64 * US))
        .collect();
    let mut cfg = SimConfig::new(topo, CcKind::None);
    cfg.flows = flows;
    let mut sim = Sim::new(cfg);
    sim.run();
    assert_eq!(sim.counters.retransmit_events, 0);
    let tor = 320usize;
    // ToR ports 0..16 are hosts, 16..20 the uplinks.
    let up: Vec<u64> = sim.ports[tor][16..20].iter().map(|p| p.tx_bytes).collect();
    assert!(up.iter().all(|&b| b > 0), "uplink usage {up:?}");
    // Same seed -> same paths: the ideal (path-based) FCTs match a rerun.
    let mut cfg2 = SimConfig::new(fat_tree(&FatTreeParams::paper()), CcKind::None);
    cfg2.flows = (0..64)
        .map(|i| flow(i % 16, 64 + (i % 16), 50_000, i as u64 * US))
        .collect();
    let mut sim2 = Sim::new(cfg2);
    sim2.run();
    let a: Vec<u64> = sim.ports[tor][16..20].iter().map(|p| p.tx_bytes).collect();
    let b: Vec<u64> = sim2.ports[tor][16..20].iter().map(|p| p.tx_bytes).collect();
    assert_eq!(a, b);
}

fn digest(sim: &Sim) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x100000001b3);
    };
    let mut recs: Vec<_> = sim.fct.iter().collect();
    recs.sort_by_key(|r| r.flow);
    for r in recs {
        eat(r.flow as u64);
        eat(r.fct);
        eat(r.ideal);
    }
    let c = &sim.counters;
    for v in [
        c.data_bytes_injected,
        c.data_bytes_received,
        c.ecn_marks,
        c.pause_frames,
        sim.events_processed(),
    ] {
        eat(v);
    }
    h
}

fn small_run(seed: u64, cc: &str) -> Sim {
    let topo = fat_tree(&FatTreeParams::small());
    let mut rng = Rng::stream(seed, 1);
    let mut cfg = SimConfig::new(topo, cc_by_name(cc, gbps(100), None).unwrap());
    cfg.seed = seed;
    cfg.flows = poisson(32, &FB_HADOOP, 0.5, gbps(100), MS, &mut rng);
    cfg.stop_time = 50 * MS;
    let mut sim = Sim::new(cfg);
    sim.run();
    sim
}

#[test]
fn identical_seed_identical_output() {
    for cc in ["hpcc", "dcqcn", "dctcp"] {
        let a = digest(&small_run(7, cc));
        let b = digest(&small_run(7, cc));
        assert_eq!(a, b, "{cc}: same seed must give identical results");
        let c = digest(&small_run(8, cc));
        assert_ne!(a, c, "{cc}: different seed should differ");
    }
}

#[test]
fn golden_digest() {
    // Pinned on macOS/arm64; CI recomputes it on Linux/x86-64. The model uses
    // only exactly-rounded IEEE operations (and its own logarithm), so the
    // digest must be identical on every platform. If an intentional model
    // change alters it, update the constants and say why in the commit.
    let got: Vec<u64> = ["hpcc", "dcqcn", "dctcp", "timely"]
        .iter()
        .map(|cc| digest(&small_run(7, cc)))
        .collect();
    println!("golden digests: {got:?}");
    assert_eq!(got, GOLDEN);
}

const GOLDEN: [u64; 4] = [
    8052488282225703949,
    2596658737050271722,
    10046574938894289012,
    12674327727610662015,
];
