#!/usr/bin/env python3
"""Runs the experiment matrix with a bounded number of parallel simulations.

    python3 analysis/run_experiments.py --set quick|full [--jobs 4] [--out results/quick]

Each run writes into its own directory; a run whose summary.json already
exists is skipped, so an interrupted `make repro` resumes where it stopped.
Only the Python standard library is used here.
"""

import argparse
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "release", "hpcc-sim")

FIG11_CCS = ["hpcc", "dcqcn", "dcqcn+win", "timely", "timely+win", "dctcp"]
FIG10_CCS = ["hpcc", "dcqcn", "dctcp"]


def matrix(which):
    """Returns a list of (run name, argv)."""
    full = which == "full"
    runs = []
    topo = [] if full else ["--small"]
    # Figure 11: FB_Hadoop, 30% load + 60-to-1 incast, and 50% load.
    win = ["--warmup-ms", "1", "--measure-ms", "5", "--tail-ms", "20"] if full else \
          ["--warmup-ms", "1", "--measure-ms", "10", "--tail-ms", "15"]
    ccs = FIG11_CCS if full else ["hpcc", "dcqcn", "dcqcn+win", "dctcp"]
    for tag, load, incast in [("30inc", "0.3", True), ("50", "0.5", False)]:
        for cc in ccs:
            argv = ["fct", "--cc", cc, "--workload", "fbhadoop", "--load", load] + win + topo
            if incast:
                argv.append("--incast")
            runs.append((f"fig11-fb{tag}-{cc}", argv))
    # Figure 10's workload (WebSearch at 30% and 50%) in simulation.
    wwin = ["--warmup-ms", "1", "--measure-ms", "10", "--tail-ms", "25"] if full else \
           ["--warmup-ms", "1", "--measure-ms", "20", "--tail-ms", "20"]
    for load in (["0.3", "0.5"] if full else ["0.5"]):
        for cc in (FIG10_CCS if full else ["hpcc", "dcqcn"]):
            argv = ["fct", "--cc", cc, "--workload", "websearch", "--load", load] + wwin + topo
            runs.append((f"fig10-ws{int(float(load) * 100)}-{cc}", argv))
    if full:
        # Seed-to-seed variation of the headline comparison.
        for seed in ["2", "3"]:
            for cc in ["hpcc", "dcqcn"]:
                argv = ["fct", "--cc", cc, "--workload", "fbhadoop", "--load", "0.5", "--seed", seed] + win
                runs.append((f"seed{seed}-fb50-{cc}", argv))
        # Sensitivity: maxStage = 0 (the authors' run.py default) vs 5 (paper text).
        argv = ["fct", "--cc", "hpcc", "--workload", "fbhadoop", "--load", "0.5", "--max-stage", "0"] + win
        runs.append(("sens-fb50-hpcc-maxstage0", argv))
        # Sensitivity: ACKs in a high-priority class (this simulator's original
        # model) instead of the data class (the authors' simulator).
        for cc in ["hpcc", "dcqcn"]:
            argv = ["fct", "--cc", cc, "--workload", "fbhadoop", "--load", "0.5", "--acks-high-priority"] + win
            runs.append((f"sens-fb50-{cc}-ackhigh", argv))
    # Figure 13: 16-to-1 incast, per-ACK vs per-RTT vs HPCC.
    for cc in ["hpcc", "hpcc-perack", "hpcc-perrtt"]:
        runs.append((f"fig13-{cc}", ["incast", "--n", "16", "--cc", cc, "--duration-us", "400", "--bin-ns", "2000"]))
    # Figure 14: W_AI sweep, 16-to-1, first 10 ms; queue sampled every 1 us.
    for wai in ["25", "50", "100", "150", "300"]:
        runs.append((f"fig14-wai{wai}", ["incast", "--n", "16", "--cc", "hpcc", "--wai", wai,
                                          "--duration-us", "10000", "--bin-ns", "100000", "--queue-sample-ns", "1000"]))
    # Figure 9: testbed micro-benchmarks, in simulation.
    for kind in ["longshort", "incast", "mice", "fairshare"]:
        for cc in ["hpcc", "dcqcn"]:
            runs.append((f"fig9-{kind}-{cc}", ["micro", "--kind", kind, "--cc", cc]))
    return runs


def run_one(out_root, name, argv):
    d = os.path.join(out_root, name)
    if os.path.exists(os.path.join(d, "summary.json")):
        return name, 0.0, "cached"
    t0 = time.time()
    p = subprocess.run([BIN] + argv + ["--out", d], capture_output=True, text=True)
    dt = time.time() - t0
    if p.returncode != 0:
        return name, dt, "FAILED: " + p.stderr.strip()[:400]
    return name, dt, p.stdout.strip().split(": ", 1)[-1][:160]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--set", choices=["quick", "full"], required=True)
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--out", default=None)
    ap.add_argument("--only", default=None, help="substring filter on run names")
    a = ap.parse_args()
    out = a.out or os.path.join(ROOT, "results", a.set)
    os.makedirs(out, exist_ok=True)
    if not os.path.exists(BIN):
        sys.exit(f"{BIN} not found: run `cargo build --release` first")
    runs = matrix(a.set)
    if a.only:
        runs = [r for r in runs if a.only in r[0]]
    t0 = time.time()
    failed = 0
    # Longest runs first so the pool stays busy.
    order = sorted(runs, key=lambda r: (not r[1][0] == "fct", "small" in " ".join(r[1])))
    with ThreadPoolExecutor(max_workers=a.jobs) as ex:
        futs = [ex.submit(run_one, out, n, argv) for n, argv in order]
        for f in as_completed(futs):
            name, dt, msg = f.result()
            failed += msg.startswith("FAILED")
            print(f"[{time.time() - t0:7.1f}s] {name:<34} {dt:6.1f}s  {msg}", flush=True)
    print(f"{len(runs)} runs, {failed} failed, {time.time() - t0:.1f} s wall with {a.jobs} jobs")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
