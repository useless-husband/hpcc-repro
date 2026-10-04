//! Experiment presets matching the HPCC paper's evaluation, and result
//! output (CSV + JSON, no external dependencies).

use crate::cc::dcqcn::DcqcnParams;
use crate::cc::dctcp::DctcpParams;
use crate::cc::hpcc::{HpccParams, Reaction};
use crate::cc::timely::TimelyParams;
use crate::cc::CcKind;
use crate::rng::Rng;
use crate::sim::{Sim, SimConfig};
use crate::time::{gbps, Time, MS, US};
use crate::topology::{fat_tree, star, FatTreeParams};
use crate::workload::{incast, poisson, FlowSpec, FlowTag, SizeCdf};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

/// Builds a CC by name. `wai` overrides HPCC's W_AI (bytes).
pub fn cc_by_name(name: &str, line_rate_bps: u64, wai: Option<f64>) -> Result<CcKind, String> {
    let hp = |reaction| {
        let mut p = HpccParams {
            reaction,
            ..HpccParams::default()
        };
        if let Some(w) = wai {
            p.w_ai = w;
        }
        CcKind::Hpcc(p)
    };
    Ok(match name {
        "none" => CcKind::None,
        "hpcc" => hp(Reaction::Hpcc),
        "hpcc-perack" => hp(Reaction::PerAck),
        "hpcc-perrtt" => hp(Reaction::PerRtt),
        "dcqcn" => CcKind::Dcqcn(DcqcnParams::vendor(line_rate_bps, false)),
        "dcqcn+win" => CcKind::Dcqcn(DcqcnParams::vendor(line_rate_bps, true)),
        "dctcp" => CcKind::Dctcp(DctcpParams::default()),
        "timely" => CcKind::Timely(TimelyParams::paper(line_rate_bps, false)),
        "timely+win" => CcKind::Timely(TimelyParams::paper(line_rate_bps, true)),
        _ => return Err(format!("unknown cc '{name}'")),
    })
}

pub const ALL_CCS: [&str; 6] = [
    "hpcc",
    "dcqcn",
    "dcqcn+win",
    "timely",
    "timely+win",
    "dctcp",
];

/// Large-scale FCT experiment (paper §5.3, Figure 11; and Figure 10's
/// workload run in simulation).
#[derive(Clone, Debug)]
pub struct FctExperiment {
    pub small_topology: bool,
    pub workload: SizeCdf,
    pub load: f64,
    /// Add 60-to-1 incasts of 500 KB per sender at 2% of capacity (§5.3).
    pub incast: bool,
    /// Flows starting in [warmup, warmup + measure) are measured.
    pub warmup: Time,
    pub measure: Time,
    /// Extra simulated time after the measurement window for measured flows
    /// to finish while background traffic keeps arriving.
    pub tail: Time,
    pub cc: String,
    pub seed: u64,
    pub wai: Option<f64>,
    /// Overrides HPCC's maxStage (paper: 5; the authors' run.py default: 0).
    pub max_stage: Option<u32>,
    pub pfc: bool,
    pub queue_sample: Time,
}

impl FctExperiment {
    pub fn new(cc: &str, workload: SizeCdf, load: f64, incast: bool) -> Self {
        FctExperiment {
            small_topology: false,
            workload,
            load,
            incast,
            warmup: MS,
            measure: 5 * MS,
            tail: 15 * MS,
            cc: cc.into(),
            seed: 1,
            wai: None,
            max_stage: None,
            pfc: true,
            queue_sample: 10 * US,
        }
    }

    pub fn config(&self) -> Result<SimConfig, String> {
        let p = if self.small_topology {
            FatTreeParams::small()
        } else {
            FatTreeParams::paper()
        };
        let topo = fat_tree(&p);
        let mut cc = cc_by_name(&self.cc, p.host_rate_bps, self.wai)?;
        if let (Some(ms), CcKind::Hpcc(hp)) = (self.max_stage, &mut cc) {
            hp.max_stage = ms;
        }
        let hosts = p.n_hosts();
        // Traffic keeps arriving until the hard stop so that measured flows
        // never finish in an artificially empty network; flows that start
        // after the run ends simply never start.
        let stop = self.warmup + self.measure + self.tail;
        let mut rng = Rng::stream(self.seed, 1);
        let mut flows = poisson(
            hosts,
            &self.workload,
            self.load,
            p.host_rate_bps,
            stop,
            &mut rng,
        );
        if self.incast {
            let fan_in = 60.min(hosts - 1);
            let mut rng = Rng::stream(self.seed, 2);
            flows.extend(incast(
                hosts,
                fan_in,
                500_000,
                0.02,
                p.host_rate_bps,
                stop,
                &mut rng,
            ));
        }
        let mut cfg = SimConfig::new(topo, cc);
        cfg.seed = self.seed;
        cfg.pfc = self.pfc;
        cfg.flows = flows;
        cfg.stop_time = stop;
        cfg.measure = Some((self.warmup, self.warmup + self.measure));
        cfg.queue_sample_interval = Some(self.queue_sample);
        cfg.stats_from = self.warmup;
        cfg.base_rtt = 13 * US; // §5.1: T = 13 us for the simulations
        Ok(cfg)
    }

    pub fn describe(&self) -> Vec<(&'static str, String)> {
        vec![
            ("experiment", "fct".into()),
            (
                "topology",
                if self.small_topology {
                    "fattree-small-32".into()
                } else {
                    "fattree-paper-320".into()
                },
            ),
            ("workload", self.workload.name.into()),
            ("load", format!("{}", self.load)),
            ("incast", format!("{}", self.incast)),
            ("warmup_ps", self.warmup.to_string()),
            ("measure_ps", self.measure.to_string()),
            ("cc", self.cc.clone()),
            ("seed", self.seed.to_string()),
            ("pfc", self.pfc.to_string()),
            (
                "wai",
                self.wai
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| "default".into()),
            ),
            (
                "max_stage",
                self.max_stage
                    .map(|w| w.to_string())
                    .unwrap_or_else(|| "default".into()),
            ),
        ]
    }
}

/// Paper §5.4 / Figures 13 and 14: `n` senders and one receiver on a single
/// 100 Gbps switch with 1 us links, all flows starting at time 0.
pub fn incast_n(
    n: u32,
    cc: &str,
    wai: Option<f64>,
    duration: Time,
    trace_bin: Time,
) -> Result<SimConfig, String> {
    let topo = star(n + 1, gbps(100), US);
    let mut cfg = SimConfig::new(topo, cc_by_name(cc, gbps(100), wai)?);
    // Base RTT here is about 4.2 us (2 x 2 x 1 us + serialization); T is
    // set slightly above it, as the paper prescribes.
    cfg.base_rtt = 4_500_000;
    cfg.flows = (0..n)
        .map(|s| FlowSpec {
            src: s,
            dst: n,
            size: 100_000_000,
            start: 0,
            tag: FlowTag::Micro,
        })
        .collect();
    cfg.stop_time = duration;
    cfg.trace_ports = vec![(n + 1, n as u16)];
    cfg.trace_bin = trace_bin;
    Ok(cfg)
}

/// The four testbed micro-benchmarks of Figure 9, run in simulation on a
/// single 25 Gbps switch (the testbed's NIC speed) with 1 us links.
pub fn micro(kind: &str, cc: &str) -> Result<SimConfig, String> {
    let rate = gbps(25);
    let mut flows = Vec::new();
    let (hosts, stop, bin) = match kind {
        // Fig. 9a/b: a long flow at line rate; a 1 MB flow joins at 1 ms
        // over the same bottleneck and leaves when done.
        "longshort" => {
            flows.push(FlowSpec {
                src: 0,
                dst: 2,
                size: 1 << 40,
                start: 0,
                tag: FlowTag::Micro,
            });
            flows.push(FlowSpec {
                src: 1,
                dst: 2,
                size: 1_000_000,
                start: MS,
                tag: FlowTag::Micro,
            });
            (3, 3 * MS, 20 * US)
        }
        // Fig. 9c/d: seven senders start at the same time towards the
        // receiver of a long-running flow.
        "incast" => {
            flows.push(FlowSpec {
                src: 0,
                dst: 8,
                size: 1 << 40,
                start: 0,
                tag: FlowTag::Micro,
            });
            for s in 1..8 {
                flows.push(FlowSpec {
                    src: s,
                    dst: 8,
                    size: 1_000_000,
                    start: MS,
                    tag: FlowTag::Micro,
                });
            }
            (9, 5 * MS, 20 * US)
        }
        // Fig. 9e/f: two elephants saturate a link; 1 KB mice keep being
        // sent through it (one every 20 us from a third host).
        "mice" => {
            flows.push(FlowSpec {
                src: 0,
                dst: 3,
                size: 1 << 40,
                start: 0,
                tag: FlowTag::Micro,
            });
            flows.push(FlowSpec {
                src: 1,
                dst: 3,
                size: 1 << 40,
                start: 0,
                tag: FlowTag::Micro,
            });
            let mut t = MS;
            while t < 10 * MS {
                flows.push(FlowSpec {
                    src: 2,
                    dst: 3,
                    size: 1000,
                    start: t,
                    tag: FlowTag::Background,
                });
                t += 20 * US;
            }
            (4, 10 * MS + 100 * US, 20 * US)
        }
        // Fig. 9g/h: four flows join a link one by one and then leave one by
        // one. The testbed used 1 s steps; here the step is 5 ms.
        "fairshare" => {
            let step = 5 * MS;
            for i in 0..4u32 {
                // Flow i runs from i*step to (7-i)*step at roughly its fair share.
                let start = i as u64 * step;
                let dur_steps = 7 - 2 * i as u64;
                let share: f64 = (0..dur_steps)
                    .map(|k| {
                        let t = i as u64 + k; // step index
                        let active = 4u64.min(t + 1).min(7 - t);
                        1.0 / active.max(1) as f64
                    })
                    .sum();
                let size = (rate as f64 / 8.0 * (step as f64 * 1e-12) * share) as u64;
                flows.push(FlowSpec {
                    src: i,
                    dst: 4,
                    size,
                    start,
                    tag: FlowTag::Micro,
                });
            }
            (5, 40 * MS, 100 * US)
        }
        _ => return Err(format!("unknown micro-benchmark '{kind}'")),
    };
    let topo = star(hosts, rate, US);
    let mut cfg = SimConfig::new(topo, cc_by_name(cc, rate, None)?);
    // Base RTT on this switch: ~4.8 us at 25 Gbps; T slightly above it.
    cfg.base_rtt = 5 * US;
    cfg.flows = flows;
    cfg.stop_time = stop;
    cfg.trace_ports = vec![(hosts, (hosts - 1) as u16)];
    cfg.trace_bin = bin;
    if kind == "mice" {
        cfg.queue_sample_interval = None;
        cfg.trace_bin = US;
    }
    Ok(cfg)
}

// ------------------------------------------------------------------ output

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

/// Writes `summary.json`, `fct.csv`, and (when traced) `trace.csv`,
/// `queue_hist.csv`, `rtt_hist.csv` into `dir`.
pub fn write_outputs(
    sim: &Sim,
    dir: &Path,
    meta: &[(&str, String)],
    wall_seconds: f64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let c = &sim.counters;
    let (pause_host, pause_sw) = sim.pause_time();
    let mut j = String::from("{\n");
    for (k, v) in meta {
        let _ = writeln!(j, "  \"{}\": \"{}\",", json_escape(k), json_escape(v));
    }
    let measured = sim.fct.len();
    let nums: Vec<(&str, f64)> = vec![
        ("wall_seconds", wall_seconds),
        ("events", sim.events_processed() as f64),
        ("end_time_ps", sim.end_time as f64),
        ("stats_from_ps", sim.cfg.stats_from as f64),
        ("flows_total", sim.flows.len() as f64),
        (
            "flows_started",
            sim.flows.iter().filter(|f| f.started).count() as f64,
        ),
        ("flows_finished", measured as f64),
        ("measured_flows", sim.measured_counts().0 as f64),
        ("measured_unfinished", sim.measured_counts().1 as f64),
        ("n_hosts", sim.n_hosts() as f64),
        ("n_switch_ports", sim.n_switch_ports() as f64),
        ("data_bytes_injected", c.data_bytes_injected as f64),
        ("data_pkts_injected", c.data_pkts_injected as f64),
        ("data_bytes_received", c.data_bytes_received as f64),
        ("drops", c.drops as f64),
        ("ecn_marks", c.ecn_marks as f64),
        ("pause_frames", c.pause_frames as f64),
        ("resume_frames", c.resume_frames as f64),
        ("retransmit_events", c.retransmit_events as f64),
        ("pause_time_host_ps", pause_host as f64),
        ("pause_time_switch_ps", pause_sw as f64),
        ("rtt_p50_ps", sim.rtt_hist.quantile(0.50) as f64),
        ("rtt_p95_ps", sim.rtt_hist.quantile(0.95) as f64),
        ("rtt_p99_ps", sim.rtt_hist.quantile(0.99) as f64),
        ("queue_samples", sim.queue_hist.total() as f64),
        ("queue_p50_bytes", sim.queue_hist.quantile(0.50) as f64),
        ("queue_p95_bytes", sim.queue_hist.quantile(0.95) as f64),
        ("queue_p99_bytes", sim.queue_hist.quantile(0.99) as f64),
        (
            "queue_max_bytes",
            sim.ports
                .iter()
                .skip(sim.n_hosts() as usize)
                .flatten()
                .map(|p| p.max_data_bytes)
                .max()
                .unwrap_or(0) as f64,
        ),
    ];
    for (i, (k, v)) in nums.iter().enumerate() {
        let sep = if i + 1 == nums.len() { "" } else { "," };
        let _ = writeln!(j, "  \"{k}\": {v}{sep}");
    }
    j.push_str("}\n");
    std::fs::write(dir.join("summary.json"), j)?;

    let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("fct.csv"))?);
    writeln!(w, "flow,src,dst,size,start_ps,fct_ps,ideal_ps,tag")?;
    let mut recs: Vec<_> = sim.fct.iter().collect();
    recs.sort_by_key(|r| r.flow);
    for r in recs {
        let tag = match r.tag {
            FlowTag::Background => "bg",
            FlowTag::Incast => "incast",
            FlowTag::Micro => "micro",
        };
        writeln!(
            w,
            "{},{},{},{},{},{},{},{}",
            r.flow, r.src, r.dst, r.size, r.start, r.fct, r.ideal, tag
        )?;
    }
    w.flush()?;

    for (name, h) in [
        ("queue_hist.csv", &sim.queue_hist),
        ("rtt_hist.csv", &sim.rtt_hist),
    ] {
        if h.total() > 0 {
            let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join(name))?);
            writeln!(w, "value,count")?;
            for (v, n) in h.buckets() {
                writeln!(w, "{v},{n}")?;
            }
            w.flush()?;
        }
    }

    // Flows that started but had not finished when the run stopped.
    let unfinished: Vec<_> = sim
        .flows
        .iter()
        .enumerate()
        .filter(|(_, f)| f.started && !f.finished)
        .collect();
    if !unfinished.is_empty() {
        let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("unfinished.csv"))?);
        writeln!(
            w,
            "flow,src,dst,size,start_ps,ideal_ps,tag,acked,rate_bps,win,measured,cc_state"
        )?;
        for (i, f) in unfinished {
            let st = f
                .cc_state()
                .map(|c| format!("{c:?}").replace(',', ";"))
                .unwrap_or_default();
            let st: String = st.chars().take(300).collect();
            let tag = match f.spec.tag {
                FlowTag::Background => "bg",
                FlowTag::Incast => "incast",
                FlowTag::Micro => "micro",
            };
            writeln!(
                w,
                "{i},{},{},{},{},{},{tag},{},{},{},{},{st}",
                f.spec.src,
                f.spec.dst,
                f.spec.size,
                f.spec.start,
                sim.ideal_fct(i as u32),
                f.snd_una,
                f.ctl.rate_bps,
                f.ctl.win,
                f.is_measured()
            )?;
        }
        w.flush()?;
    }
    let micro: Vec<_> = sim
        .flows
        .iter()
        .enumerate()
        .filter(|(_, f)| f.spec.tag == FlowTag::Micro)
        .collect();
    if !micro.is_empty() {
        // Delivered payload bytes per flow per trace bin.
        let bins = micro.iter().map(|(_, f)| f.trace.len()).max().unwrap_or(0);
        let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("trace.csv"))?);
        write!(w, "t_ps")?;
        for (i, _) in &micro {
            write!(w, ",flow{i}_bytes")?;
        }
        writeln!(w)?;
        for b in 0..bins {
            write!(w, "{}", b as u64 * sim.cfg.trace_bin)?;
            for (_, f) in &micro {
                write!(w, ",{}", f.trace.get(b).copied().unwrap_or(0))?;
            }
            writeln!(w)?;
        }
        w.flush()?;
    }
    if let Some(t) = sim.port_trace.first() {
        // Data-queue length of the traced port at each sample time.
        let iv = sim.cfg.queue_sample_interval.unwrap_or(sim.cfg.trace_bin);
        let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("queue_trace.csv"))?);
        writeln!(w, "t_ps,queue_bytes")?;
        for (k, v) in t.iter().enumerate() {
            writeln!(w, "{},{}", (k as u64 + 1) * iv, v)?;
        }
        w.flush()?;
    }
    Ok(())
}

/// Runs a configured simulation, timing it.
pub fn run(cfg: SimConfig) -> (Sim, f64) {
    let t0 = std::time::Instant::now();
    let mut sim = Sim::new(cfg);
    sim.run();
    (sim, t0.elapsed().as_secs_f64())
}
