//! Command-line driver. Run `hpcc-sim help` for usage.

use hpcc_sim::experiments::{from_ns3_files, incast_n, micro, run, write_outputs, FctExperiment};
use hpcc_sim::time::{to_us, MS, NS, US};
use hpcc_sim::workload::SizeCdf;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:
  hpcc-sim fct    --cc NAME --workload fbhadoop|websearch --load 0.3 [--incast]
                  [--small] [--warmup-ms 1] [--measure-ms 5] [--tail-ms 15]
                  [--seed 1] [--wai BYTES] [--max-stage N] [--no-pfc]
                  [--acks-high-priority] [--export-ns3 DIR] --out DIR
  hpcc-sim incast --n 16 --cc NAME [--wai BYTES] [--duration-us 400] [--bin-ns 1000]
                  [--queue-sample-ns NS] --out DIR
  hpcc-sim micro  --kind longshort|incast|mice|fairshare --cc NAME --out DIR
  hpcc-sim ns3    --topology FILE --flows FILE --cc NAME [--base-rtt-ns 13000] [--stop-ms 100]
                  [--wai BYTES] [--max-stage N] [--trace-port NODE:PORT] [--sample-ns 1000]
                  [--queue-sample-ns NS] [--stats-from-ms MS] [--acks-high-priority] [--seed N] --out DIR
                  (runs a scenario given in the authors' ns-3 input format)
CC names: none hpcc hpcc-perack hpcc-perrtt dcqcn dcqcn+win dctcp timely timely+win";

struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, name: &str) -> bool {
        if let Some(i) = self.0.iter().position(|a| a == name) {
            self.0.remove(i);
            true
        } else {
            false
        }
    }
    fn val(&mut self, name: &str) -> Result<Option<String>, String> {
        if let Some(i) = self.0.iter().position(|a| a == name) {
            if i + 1 >= self.0.len() {
                return Err(format!("{name} needs a value"));
            }
            let v = self.0.remove(i + 1);
            self.0.remove(i);
            Ok(Some(v))
        } else {
            Ok(None)
        }
    }
    fn num<T: std::str::FromStr>(&mut self, name: &str, default: T) -> Result<T, String> {
        match self.val(name)? {
            Some(v) => v.parse().map_err(|_| format!("bad value for {name}: {v}")),
            None => Ok(default),
        }
    }
    fn req(&mut self, name: &str) -> Result<String, String> {
        self.val(name)?.ok_or_else(|| format!("missing {name}"))
    }
    fn done(&self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(format!("unexpected arguments: {:?}", self.0))
        }
    }
}

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.is_empty() || argv[0] == "help" || argv[0] == "--help" {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let cmd = argv.remove(0);
    match real_main(&cmd, Args(argv)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn export_ns3(cfg: &hpcc_sim::sim::SimConfig, dir: &std::path::Path) -> std::io::Result<()> {
    use std::fmt::Write as _;
    std::fs::create_dir_all(dir)?;
    let t = &cfg.topo;
    let mut s = format!("{} {} {}\n", t.n_nodes(), t.n_switches, t.links.len());
    s += &(t.n_hosts..t.n_nodes())
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    s.push('\n');
    for l in &t.links {
        let _ = writeln!(
            s,
            "{} {} {}Gbps {}ns 0",
            l.a,
            l.b,
            l.rate_bps / 1_000_000_000,
            l.delay / 1000
        );
    }
    std::fs::write(dir.join("topology.txt"), s)?;
    // Flows sorted by start; ns-3 time starts at 2 s like the authors' generator.
    let mut flows = cfg.flows.clone();
    flows.sort_by_key(|f| f.start);
    let mut s = format!("{}\n", flows.len());
    for f in &flows {
        let dport = if f.tag == hpcc_sim::workload::FlowTag::Incast {
            200
        } else {
            100
        };
        let _ = writeln!(
            s,
            "{} {} 3 {} {} {:.12}",
            f.src,
            f.dst,
            dport,
            f.size,
            2.0 + f.start as f64 * 1e-12
        );
    }
    std::fs::write(dir.join("flows.txt"), s)
}

fn real_main(cmd: &str, mut a: Args) -> Result<(), String> {
    let (cfg, meta, out) = match cmd {
        "fct" => {
            let cc = a.req("--cc")?;
            let wl = a.req("--workload")?;
            let cdf = SizeCdf::by_name(&wl).ok_or(format!("unknown workload {wl}"))?;
            let load: f64 = a.num("--load", 0.3)?;
            let incast = a.flag("--incast");
            let mut e = FctExperiment::new(&cc, cdf, load, incast);
            e.small_topology = a.flag("--small");
            e.warmup = (a.num("--warmup-ms", 1.0f64)? * MS as f64) as u64;
            e.measure = (a.num("--measure-ms", 5.0f64)? * MS as f64) as u64;
            e.tail = (a.num("--tail-ms", 15.0f64)? * MS as f64) as u64;
            e.seed = a.num("--seed", 1u64)?;
            e.wai = a
                .val("--wai")?
                .map(|v| v.parse().map_err(|_| "bad --wai".to_string()))
                .transpose()?;
            e.pfc = !a.flag("--no-pfc");
            e.acks_high_priority = a.flag("--acks-high-priority");
            e.max_stage = a
                .val("--max-stage")?
                .map(|v| v.parse().map_err(|_| "bad --max-stage".to_string()))
                .transpose()?;
            if let Some(d) = a.val("--export-ns3")? {
                // Write the same topology and flows in the input format of the
                // authors' ns-3 simulator, for cross-checking.
                let cfg = e.config()?;
                export_ns3(&cfg, std::path::Path::new(&d))
                    .map_err(|err| format!("export: {err}"))?;
            }
            let out = PathBuf::from(a.req("--out")?);
            (e.config()?, e.describe(), out)
        }
        "incast" => {
            let n: u32 = a.num("--n", 16)?;
            let cc = a.req("--cc")?;
            let wai: Option<f64> = a
                .val("--wai")?
                .map(|v| v.parse().map_err(|_| "bad --wai".to_string()))
                .transpose()?;
            let dur = a.num("--duration-us", 400u64)? * US;
            let bin = a.num("--bin-ns", 1000u64)? * NS;
            let qs: Option<u64> = a
                .val("--queue-sample-ns")?
                .map(|v| v.parse().map_err(|_| "bad --queue-sample-ns".to_string()))
                .transpose()?;
            let out = PathBuf::from(a.req("--out")?);
            let meta = vec![
                ("experiment", "incast".to_string()),
                ("n", n.to_string()),
                ("cc", cc.clone()),
                (
                    "wai",
                    wai.map(|w| w.to_string())
                        .unwrap_or_else(|| "default".into()),
                ),
            ];
            let mut cfg = incast_n(n, &cc, wai, dur, bin)?;
            cfg.queue_sample_interval = qs.map(|q| q * NS);
            (cfg, meta, out)
        }
        "micro" => {
            let kind = a.req("--kind")?;
            let cc = a.req("--cc")?;
            let out = PathBuf::from(a.req("--out")?);
            let meta = vec![
                ("experiment", "micro".to_string()),
                ("kind", kind.clone()),
                ("cc", cc.clone()),
            ];
            (micro(&kind, &cc)?, meta, out)
        }
        "ns3" => {
            let topo = PathBuf::from(a.req("--topology")?);
            let flows = PathBuf::from(a.req("--flows")?);
            let cc = a.req("--cc")?;
            let mut cfg = from_ns3_files(&topo, &flows, &cc, 2.0)?;
            cfg.seed = a.num("--seed", 1u64)?; // ECMP and ECN coin flips; flows come from the file
            cfg.base_rtt = a.num("--base-rtt-ns", 13_000u64)? * NS;
            cfg.stop_time = (a.num("--stop-ms", 100.0f64)? * MS as f64) as u64;
            if let hpcc_sim::cc::CcKind::Hpcc(p) = &mut cfg.cc {
                if let Some(w) = a.val("--wai")? {
                    p.w_ai = w.parse().map_err(|_| "bad --wai")?;
                }
                if let Some(m) = a.val("--max-stage")? {
                    p.max_stage = m.parse().map_err(|_| "bad --max-stage")?;
                }
            }
            if let Some(q) = a.val("--queue-sample-ns")? {
                cfg.queue_sample_interval =
                    Some(q.parse::<u64>().map_err(|_| "bad --queue-sample-ns")? * NS);
            }
            cfg.stats_from = (a.num("--stats-from-ms", 0.0f64)? * MS as f64) as u64;
            if a.flag("--acks-high-priority") {
                cfg.ack_class = hpcc_sim::sim::AckClass::Control;
            }
            if let Some(tp) = a.val("--trace-port")? {
                let (n, p) = tp.split_once(':').ok_or("--trace-port NODE:PORT")?;
                cfg.trace_ports = vec![(
                    n.parse().map_err(|_| "bad node")?,
                    p.parse().map_err(|_| "bad port")?,
                )];
                cfg.trace_bin = a.num("--sample-ns", 1000u64)? * NS;
            }
            let out = PathBuf::from(a.req("--out")?);
            let meta = vec![("experiment", "ns3-input".to_string()), ("cc", cc.clone())];
            (cfg, meta, out)
        }
        _ => return Err(format!("unknown command '{cmd}'")),
    };
    a.done()?;
    let label = meta
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let (sim, wall) = run(cfg);
    write_outputs(&sim, &out, &meta, wall)
        .map_err(|e| format!("writing {}: {e}", out.display()))?;
    let (ph, ps) = sim.pause_time();
    println!(
        "{label}: {} flows done, sim {:.3} ms, {} events in {:.1} s, drops {}, pause frames {}, host pause {:.1} us, switch pause {:.1} us, rtt p95 {:.1} us",
        sim.fct.len(),
        to_us(sim.end_time) / 1000.0,
        sim.events_processed(),
        wall,
        sim.counters.drops,
        sim.counters.pause_frames,
        to_us(ph),
        to_us(ps),
        to_us(sim.rtt_hist.quantile(0.95)),
    );
    Ok(())
}
