#!/usr/bin/env python3
"""Cross-check against the authors' ns-3 simulator on identical inputs.

    # 1. this simulator, exporting its topology and flow list
    hpcc-sim fct --cc hpcc --workload fbhadoop --load 0.5 --small --warmup-ms 1 \
        --measure-ms 3 --tail-ms 10 --export-ns3 X/in --out X/ours-hpcc
    # 2. a config for the authors' binary (parameters as in their run.py,
    #    with the paper's maxStage = 5 and W_AI ~ 80 B)
    python3 tools/crosscheck_ns3.py config --cc hp --inputs X/in --out X/config_hp.txt
    BUILD_DIR/third X/config_hp.txt
    # 3. compare
    python3 tools/crosscheck_ns3.py compare X/ours-hpcc X/in/fct_hp.txt

Both sides are scored against the same exact ideal FCT (this simulator's), so
only the simulated FCTs differ. Standard library only.
"""
import argparse
import csv
import os


def pctl(a, p):
    return a[min(int(len(a) * p), len(a) - 1)]


def node(ip_hex):
    v = int(ip_hex, 16) - 0x0B000001
    return ((v >> 8) & 0xFF) + ((v >> 16) & 0xFF) * 256


CONFIG = """ENABLE_QCN 1
USE_DYNAMIC_PFC_THRESHOLD 1
PACKET_PAYLOAD_SIZE 1000
TOPOLOGY_FILE {inp}/topology.txt
FLOW_FILE {inp}/flows.txt
TRACE_FILE {inp}/trace.txt
TRACE_OUTPUT_FILE {inp}/mix_{cc}.tr
FCT_OUTPUT_FILE {inp}/fct_{cc}.txt
PFC_OUTPUT_FILE {inp}/pfc_{cc}.txt
SIMULATOR_STOP_TIME {stop}
CC_MODE {mode}
ALPHA_RESUME_INTERVAL 1
RATE_DECREASE_INTERVAL 4
CLAMP_TARGET_RATE 0
RP_TIMER 300
EWMA_GAIN 0.00390625
FAST_RECOVERY_TIMES 1
RATE_AI {ai}Mb/s
RATE_HAI {hai}Mb/s
MIN_RATE 1000Mb/s
DCTCP_RATE_AI 1000Mb/s
ERROR_RATE_PER_LINK 0.0000
L2_CHUNK_SIZE 4000
L2_ACK_INTERVAL 1
L2_BACK_TO_ZERO 0
HAS_WIN {win}
GLOBAL_T 1
VAR_WIN {win}
FAST_REACT {fast}
U_TARGET 0.95
MI_THRESH 5
INT_MULTI 4
MULTI_RATE 0
SAMPLE_FEEDBACK 0
PINT_LOG_BASE 1.01
PINT_PROB 1.0
RATE_BOUND 1
ACK_HIGH_PRIO {ackp}
LINK_DOWN 0 0 0
ENABLE_TRACE 0
KMAX_MAP 2 100000000000 1600 400000000000 6400
KMIN_MAP 2 100000000000 400 400000000000 1600
PMAX_MAP 2 100000000000 0.20 400000000000 0.20
BUFFER_SIZE 32
QLEN_MON_FILE {inp}/qlen_{cc}.txt
QLEN_MON_START 3000000000
QLEN_MON_END 3000000000
"""


def write_config(a):
    inp = os.path.abspath(a.inputs)
    if a.cc == "hp":
        # W_AI = RATE_AI x maxRtt: 52 Mbps x 12.24 us (the binary's maxRtt for the small tree) ~ 80 B.
        v = dict(mode=3, ai=52, hai=52, win=1, fast=1, ackp=0)
    else:
        v = dict(mode=1, ai=20, hai=200, win=0, fast=0, ackp=1)
    with open(os.path.join(inp, "trace.txt"), "w") as f:
        f.write("0\n")
    with open(a.out, "w") as f:
        f.write(CONFIG.format(inp=inp, cc=a.cc, stop=a.stop, **v))


def compare(a):
    w0, w1 = a.window_ms[0] * 1e9, a.window_ms[1] * 1e9
    ours = {}
    with open(f"{a.ours}/fct.csv") as f:
        for r in csv.DictReader(f):
            if r["tag"] == "bg" and w0 <= int(r["start_ps"]) < w1:
                key = (int(r["src"]), int(r["dst"]), int(r["size"]), round(int(r["start_ps"]) / 1000))
                ours[key] = (int(r["fct_ps"]) / 1000, int(r["ideal_ps"]) / 1000)
    theirs = {}
    with open(a.ns3_fct) as f:
        for line in f:
            sip, dip, sport, dport, size, start, fct, standalone = line.split()
            st = int(start) - 2_000_000_000
            theirs[(node(sip), node(dip), int(size), st)] = (int(fct), int(standalone))
    pairs = []
    for k, (fo, ideal) in ours.items():
        t = theirs.get(k) or theirs.get((k[0], k[1], k[2], k[3] - 1)) or theirs.get((k[0], k[1], k[2], k[3] + 1))
        if t:
            pairs.append((k[2], max(1, fo / ideal), max(1, t[0] / ideal), t[0] / max(t[1], 1)))
    print(f"matched {len(pairs)} of {len(ours)} measured flows")
    pairs.sort()
    n = len(pairs)
    print(f"{'bin (<= size)':>14} | {'ours p50':>8} {'ns3 p50':>8} | {'ours p95':>8} {'ns3 p95':>8} | {'ours p99':>8} {'ns3 p99':>8}")
    for i in range(10):
        b = pairs[i * n // 10:(i + 1) * n // 10]
        o = sorted(x[1] for x in b)
        t = sorted(x[2] for x in b)
        print(f"{b[-1][0]:>14} | {pctl(o, .5):8.2f} {pctl(t, .5):8.2f} | {pctl(o, .95):8.2f} {pctl(t, .95):8.2f} | "
              f"{pctl(o, .99):8.2f} {pctl(t, .99):8.2f}")
    o = sorted(x[1] for x in pairs)
    t = sorted(x[2] for x in pairs)
    print(f"{'all':>14} | {pctl(o, .5):8.2f} {pctl(t, .5):8.2f} | {pctl(o, .95):8.2f} {pctl(t, .95):8.2f} | "
          f"{pctl(o, .99):8.2f} {pctl(t, .99):8.2f}")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("config")
    c.add_argument("--cc", choices=["hp", "dcqcn"], required=True)
    c.add_argument("--inputs", required=True)
    c.add_argument("--out", required=True)
    c.add_argument("--stop", default="2.014", help="ns-3 stop time in s (flows start at 2 s)")
    m = sub.add_parser("compare")
    m.add_argument("ours")
    m.add_argument("ns3_fct")
    m.add_argument("--window-ms", nargs=2, type=float, default=[1.0, 4.0])
    a = ap.parse_args()
    if a.cmd == "config":
        write_config(a)
    else:
        compare(a)


if __name__ == "__main__":
    main()
