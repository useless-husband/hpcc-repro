//! Command-line driver. Run `hpcc-sim help` for usage.

use hpcc_sim::experiments::{incast_n, micro, run, write_outputs, FctExperiment};
use hpcc_sim::time::{to_us, MS, NS, US};
use hpcc_sim::workload::SizeCdf;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:
  hpcc-sim fct    --cc NAME --workload fbhadoop|websearch --load 0.3 [--incast]
                  [--small] [--warmup-ms 1] [--measure-ms 5] [--tail-ms 15]
                  [--seed 1] [--wai BYTES] [--max-stage N] [--no-pfc] --out DIR
  hpcc-sim incast --n 16 --cc NAME [--wai BYTES] [--duration-us 400] [--bin-ns 1000]
                  [--queue-sample-ns NS] --out DIR
  hpcc-sim micro  --kind longshort|incast|mice|fairshare --cc NAME --out DIR
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
            e.max_stage = a
                .val("--max-stage")?
                .map(|v| v.parse().map_err(|_| "bad --max-stage".to_string()))
                .transpose()?;
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
