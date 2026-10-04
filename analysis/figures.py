#!/usr/bin/env python3
"""Turns simulation outputs into SVG figures, a numbers file and an HTML page.

    python3 analysis/figures.py --results results/full --figures docs/figures \
        --html docs/results.html --numbers docs/results-numbers.json

Every number shown on the page is computed here from the run directories.
"""

import argparse
import csv
import html
import json
import math
import os
import sys

import matplotlib

matplotlib.use("svg")
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.ticker import FuncFormatter, NullFormatter  # noqa: E402

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from page import render_page  # noqa: E402

# Categorical palette (validated: adjacent CVD dE >= 9.1, normal >= 19.6).
# Colour follows the algorithm in every figure.
COLOR = {
    "hpcc": "#2a78d6",
    "dcqcn": "#eb6834",
    "dcqcn+win": "#1baf7a",
    "timely": "#eda100",
    "timely+win": "#e87ba4",
    "dctcp": "#008300",
    "hpcc-perack": "#4a3aa7",
    "hpcc-perrtt": "#e34948",
}
LABEL = {
    "hpcc": "HPCC",
    "dcqcn": "DCQCN",
    "dcqcn+win": "DCQCN+win",
    "timely": "TIMELY",
    "timely+win": "TIMELY+win",
    "dctcp": "DCTCP",
    "hpcc-perack": "per-ACK",
    "hpcc-perrtt": "per-RTT",
}
MARKER = {"hpcc": "o", "dcqcn": "s", "dcqcn+win": "^", "timely": "D", "timely+win": "v", "dctcp": "P",
          "hpcc-perack": "X", "hpcc-perrtt": "*"}
FIG11_CCS = ["hpcc", "dcqcn", "dcqcn+win", "timely", "timely+win", "dctcp"]
INK = "#0b0b0b"
INK2 = "#52514e"
GRID = "#e4e3df"
FLOW_COLORS = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100"]
WAI_RAMP = ["#a8c8f0", "#6fa3e3", "#2a78d6", "#1d559c", "#0f2f59"]  # one hue, light -> dark

plt.rcParams.update({
    "font.family": "DejaVu Sans",
    "font.size": 9,
    "axes.edgecolor": INK2,
    "axes.labelcolor": INK,
    "axes.titlesize": 9.5,
    "axes.titleweight": "bold",
    "axes.titlelocation": "left",
    "axes.grid": True,
    "axes.axisbelow": True,
    "grid.color": GRID,
    "grid.linewidth": 0.6,
    "axes.spines.top": False,
    "axes.spines.right": False,
    "xtick.color": INK2,
    "ytick.color": INK2,
    "legend.frameon": False,
    "lines.linewidth": 2.0,
    "lines.markersize": 5,
    "svg.fonttype": "path",
    "svg.hashsalt": "hpcc-repro",  # stable element ids -> stable diffs
    "figure.facecolor": "#fcfcfb",
    "axes.facecolor": "#fcfcfb",
    "savefig.facecolor": "#fcfcfb",
})


# ------------------------------------------------------------------ loading

def summary(d):
    with open(os.path.join(d, "summary.json")) as f:
        return json.load(f)


def exists(root, name):
    return os.path.exists(os.path.join(root, name, "summary.json"))


def pctl(a, p):
    """Same definition as the authors' fct_analysis.py: a[int(len(a) * p)]."""
    return a[min(int(len(a) * p), len(a) - 1)]


def measured_fcts(d):
    """(size, slowdown, censored) for background flows that started in the
    measurement window. Flows still running at the end of the run are kept
    with their elapsed time as a lower bound on their FCT (censored)."""
    s = summary(d)
    w0 = int(s["warmup_ps"])
    w1 = w0 + int(s["measure_ps"])
    out = []
    with open(os.path.join(d, "fct.csv")) as f:
        for r in csv.DictReader(f):
            st = int(r["start_ps"])
            if r["tag"] == "bg" and w0 <= st < w1:
                out.append((int(r["size"]), max(1.0, int(r["fct_ps"]) / int(r["ideal_ps"])), False))
    up = os.path.join(d, "unfinished.csv")
    if os.path.exists(up):
        end = int(s["end_time_ps"])
        with open(up) as f:
            for r in csv.DictReader(f):
                st = int(r["start_ps"])
                if r["tag"] == "bg" and r["measured"] == "true" and w0 <= st < w1:
                    out.append((int(r["size"]), max(1.0, (end - st) / int(r["ideal_ps"])), True))
    return out


def size_bins(rows, n=10):
    """Equal-count bins over flows sorted by size (as the authors' script)."""
    rows = sorted(rows, key=lambda r: r[0])
    out = []
    for i in range(n):
        b = rows[i * len(rows) // n:(i + 1) * len(rows) // n]
        if not b:
            continue
        sd = sorted(r[1] for r in b)
        out.append({"max_size": b[-1][0], "n": len(b), "censored": sum(r[2] for r in b),
                    "p50": pctl(sd, 0.5), "p95": pctl(sd, 0.95), "p99": pctl(sd, 0.99)})
    return out


def hist_cdf(path):
    vals, counts = [], []
    with open(path) as f:
        for r in csv.DictReader(f):
            vals.append(int(r["value"]))
            counts.append(int(r["count"]))
    tot = sum(counts)
    acc = 0
    cdf = []
    for c in counts:
        acc += c
        cdf.append(acc / tot)
    return vals, cdf


def hist_quantile(vals, cdf, p):
    for v, c in zip(vals, cdf):
        if c >= p:
            return v
    return vals[-1]


def read_series(path):
    with open(path) as f:
        rows = list(csv.reader(f))
    head = rows[0]
    cols = {h: [] for h in head}
    for r in rows[1:]:
        for h, v in zip(head, r):
            cols[h].append(int(v))
    return cols


def fmt_size(b):
    if b >= 1_000_000:
        return f"{b / 1e6:.0f}M" if b >= 9_500_000 else f"{b / 1e6:.1f}M"
    if b >= 1000:
        return f"{b / 1000:.0f}K"
    return str(b)


def plain_log_y(ax):
    """Log y-axis labelled 1, 2, 5, 10, ... instead of 10^0 notation."""
    ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _: f"{v:g}"))
    ax.yaxis.set_minor_formatter(FuncFormatter(lambda v, _: f"{v:g}" if str(f"{v:g}")[0] in "25" else ""))


def save(fig, path):
    for ax in fig.axes:
        if ax.get_yscale() == "log":
            plain_log_y(ax)
    fig.savefig(path, format="svg", bbox_inches="tight", metadata={"Date": None})
    png_dir = os.environ.get("FIG_PNG_DIR")  # optional raster copies for review
    if png_dir:
        os.makedirs(png_dir, exist_ok=True)
        fig.savefig(os.path.join(png_dir, os.path.basename(path)[:-4] + ".png"), dpi=110, bbox_inches="tight")
    plt.close(fig)


# ------------------------------------------------------------------ figures

def fig11(root, figdir, nums):
    cases = [("fb30inc", "(a) FB_Hadoop, 30% load + 60-to-1 incast"), ("fb50", "(b) FB_Hadoop, 50% load")]
    present = [c for c in cases if any(exists(root, f"fig11-{c[0]}-{cc}") for cc in FIG11_CCS)]
    if not present:
        return None
    fig, axes = plt.subplots(1, len(present), figsize=(4.4 * len(present), 3.4), sharey=True, squeeze=False)
    nums["fig11"] = {}
    for ax, (case, title) in zip(axes[0], present):
        nums["fig11"][case] = {}
        labels = None
        for cc in FIG11_CCS:
            d = os.path.join(root, f"fig11-{case}-{cc}")
            if not exists(root, f"fig11-{case}-{cc}"):
                continue
            bins = size_bins(measured_fcts(d))
            s = summary(d)
            pause_ports = (s["pause_time_host_ps"] + s["pause_time_switch_ps"])
            n_ports = s["n_hosts"] + s["n_switch_ports"]
            span = s["end_time_ps"] - s["stats_from_ps"]
            short = [r for r in measured_fcts(d) if r[0] <= 120_000]
            sd_short = sorted(r[1] for r in short)
            nums["fig11"][case][cc] = {
                "bins": bins,
                "p95_short_le_120k": pctl(sd_short, 0.95) if sd_short else None,
                "rtt_p95_us": s["rtt_p95_ps"] / 1e6,
                "rtt_p99_us": s["rtt_p99_ps"] / 1e6,
                "pause_frac_all_ports": pause_ports / (n_ports * span),
                "pause_frac_hosts": s["pause_time_host_ps"] / (s["n_hosts"] * span),
                "pause_frac_switch_ports": s["pause_time_switch_ps"] / (s["n_switch_ports"] * span),
                "pause_frames": s["pause_frames"],
                "drops": s["drops"],
                "measured_flows": s["measured_flows"],
                "measured_unfinished": s["measured_unfinished"],
                "wall_seconds": s["wall_seconds"],
                "events": s["events"],
                "end_time_ms": s["end_time_ps"] / 1e9,
            }
            xs = list(range(len(bins)))
            labels = [fmt_size(b["max_size"]) for b in bins]
            ax.plot(xs, [b["p95"] for b in bins], color=COLOR[cc], marker=MARKER[cc], label=LABEL[cc])
        ax.set_yscale("log")
        ax.set_title(title)
        ax.set_xticks(range(len(labels)))
        ax.set_xticklabels(labels, rotation=0, fontsize=7.5)
        ax.set_xlabel("flow size bin (largest flow in each 10% of flows)")
        ax.set_ylim(1, 200)
    axes[0][0].set_ylabel("95th-percentile FCT slowdown")
    axes[0][-1].legend(loc="upper left", ncol=2, fontsize=8)
    save(fig, os.path.join(figdir, "fig11_fct.svg"))

    # PFC pause and latency: two separate one-axis charts per case.
    fig, axes = plt.subplots(2, len(present), figsize=(4.4 * len(present), 4.6), squeeze=False)
    for col, (case, title) in enumerate(present):
        ccs = [cc for cc in FIG11_CCS if cc in nums["fig11"][case]]
        xs = range(len(ccs))
        pause = [100 * nums["fig11"][case][cc]["pause_frac_all_ports"] for cc in ccs]
        lat = [nums["fig11"][case][cc]["rtt_p95_us"] for cc in ccs]
        ax = axes[0][col]
        ax.bar(xs, pause, color=[COLOR[c] for c in ccs], width=0.7, edgecolor="#fcfcfb", linewidth=2)
        for x, v in zip(xs, pause):
            ax.text(x, v, f"{v:.2f}", ha="center", va="bottom", fontsize=7.5, color=INK)
        ax.set_title(("(a) 30% load + incast" if case == "fb30inc" else "(b) 50% load") + ": time paused by PFC")
        ax.set_ylabel("time paused (% of port-time)")
        ax.set_xticks(list(xs))
        ax.set_xticklabels([LABEL[c] for c in ccs], fontsize=7.5, rotation=20)
        ax.set_ylim(0, max(max(pause) * 1.3, 0.5))
        ax = axes[1][col]
        ax.bar(xs, lat, color=[COLOR[c] for c in ccs], width=0.7, edgecolor="#fcfcfb", linewidth=2)
        for x, v in zip(xs, lat):
            ax.text(x, v, f"{v:.0f}", ha="center", va="bottom", fontsize=7.5, color=INK)
        ax.set_yscale("log")
        ax.set_ylim(5, max(lat) * 3)
        ax.set_title(("(c)" if case == "fb30inc" else "(d)") + " 95th-percentile packet RTT")
        ax.set_ylabel("RTT (us, log scale)")
        ax.set_xticks(list(xs))
        ax.set_xticklabels([LABEL[c] for c in ccs], fontsize=7.5, rotation=20)
    fig.tight_layout()
    save(fig, os.path.join(figdir, "fig11_pfc_latency.svg"))
    return True


def fig10(root, figdir, nums):
    loads = [l for l in ["ws30", "ws50"] if any(exists(root, f"fig10-{l}-{cc}") for cc in ["hpcc", "dcqcn"])]
    if not loads:
        return None
    ccs_all = ["hpcc", "dcqcn", "dctcp"]
    fig, axes = plt.subplots(2, len(loads), figsize=(4.4 * len(loads), 6.4), squeeze=False)
    nums["fig10"] = {}
    for col, l in enumerate(loads):
        nums["fig10"][l] = {}
        ax = axes[0][col]
        labels = None
        for cc in ccs_all:
            name = f"fig10-{l}-{cc}"
            if not exists(root, name):
                continue
            d = os.path.join(root, name)
            rows = measured_fcts(d)
            bins = size_bins(rows)
            tiny = sorted(r[1] for r in rows if r[0] < 3000)
            vals, cdf = hist_cdf(os.path.join(d, "queue_hist.csv"))
            nonzero_total = 1 - cdf[0] if vals[0] == 0 else 1.0
            s = summary(d)
            nums["fig10"][l][cc] = {
                "bins": bins,
                "p99_under_3kb": pctl(tiny, 0.99) if tiny else None,
                "n_under_3kb": len(tiny),
                "queue_p50": hist_quantile(vals, cdf, 0.5),
                "queue_p95": hist_quantile(vals, cdf, 0.95),
                "queue_p99": hist_quantile(vals, cdf, 0.99),
                "queue_p999": hist_quantile(vals, cdf, 0.999),
                "queue_nonzero_frac": nonzero_total,
                "rtt_p95_us": s["rtt_p95_ps"] / 1e6,
                "measured_flows": s["measured_flows"],
                "measured_unfinished": s["measured_unfinished"],
                "pause_frames": s["pause_frames"],
            }
            xs = list(range(len(bins)))
            labels = [fmt_size(b["max_size"]) for b in bins]
            for p, ls in [("p50", "-"), ("p95", "--"), ("p99", ":")]:
                ax.plot(xs, [b[p] for b in bins], color=COLOR[cc], ls=ls, marker=MARKER[cc],
                        markersize=3.5, label=f"{LABEL[cc]} {p.replace('p50', 'median')}")
            # queue CDF (tail): x = bytes (log), y = CDF
            axq = axes[1][col]
            xs_q = [max(v, 1) / 1000 for v in vals]
            axq.step(xs_q, [100 * c for c in cdf], where="post", color=COLOR[cc], label=LABEL[cc])
        ax.set_yscale("log")
        ax.set_xticks(range(len(labels)))
        ax.set_xticklabels(labels, fontsize=7.5)
        ax.set_xlabel("flow size bin")
        ax.set_ylabel("FCT slowdown")
        pct = l[2:]
        ax.set_title(f"({'a' if col == 0 else 'b'}) WebSearch, {pct}% load: median / 95th / 99th")
        axq = axes[1][col]
        axq.set_xscale("log")
        axq.set_xlim(1, 40_000)
        axq.set_ylim(80, 100.5)
        axq.set_xlabel("switch egress queue (KB, log scale; empty queue drawn at 1 KB)")
        axq.set_ylabel("CDF of samples (%) - top 20% shown")
        axq.set_title(f"({'c' if col == 0 else 'd'}) queue length, {pct}% load")
        axq.legend(loc="lower right", fontsize=8)
    handles, labels_ = axes[0][0].get_legend_handles_labels()
    fig.legend(handles, labels_, loc="upper left", bbox_to_anchor=(0.06, 0.0), ncol=3, fontsize=7.5)
    fig.tight_layout()
    save(fig, os.path.join(figdir, "fig10_websearch.svg"))
    return True


def fig13(root, figdir, nums):
    variants = [v for v in ["hpcc-perack", "hpcc-perrtt", "hpcc"] if exists(root, f"fig13-{v}")]
    if not variants:
        return None
    fig, axes = plt.subplots(1, 2, figsize=(8.8, 3.2))
    nums["fig13"] = {}
    for v in variants:
        d = os.path.join(root, f"fig13-{v}")
        tr = read_series(os.path.join(d, "trace.csv"))
        t = tr["t_ps"]
        bin_ps = t[1] - t[0]
        flows = [k for k in tr if k.startswith("flow")]
        tot = [sum(tr[k][i] for k in flows) * 8 / (bin_ps * 1e-12) / 1e9 for i in range(len(t))]
        q = read_series(os.path.join(d, "queue_trace.csv"))
        axes[0].plot([x / 1e6 for x in t], tot, color=COLOR[v], label=LABEL[v], lw=1.6)
        axes[1].plot([x / 1e6 for x in q["t_ps"]], [b / 1000 for b in q["queue_bytes"]], color=COLOR[v], label=LABEL[v], lw=1.6)

        def at(us):
            i = min(range(len(q["t_ps"])), key=lambda k: abs(q["t_ps"][k] - us * 1e6))
            return q["queue_bytes"][i] / 1000

        def mean_thr(a, b):
            sel = [x for x, tt in zip(tot, t) if a * 1e6 <= tt < b * 1e6]
            return sum(sel) / len(sel)

        nums["fig13"][v] = {
            "queue_kb_at": {str(us): at(us) for us in [10, 50, 100, 150, 200, 300]},
            "peak_queue_kb": max(q["queue_bytes"]) / 1000,
            "thr_gbps_50_400": mean_thr(50, 400),
            "thr_gbps_min_after_50": min(x for x, tt in zip(tot, t) if tt >= 50e6),
            "drain_us": next((q["t_ps"][i] / 1e6 for i in range(len(q["t_ps"]))
                              if q["t_ps"][i] > 20e6 and q["queue_bytes"][i] < 10_000), None),
        }
    axes[0].set_title("(a) total goodput of the 16 flows")
    axes[0].set_xlabel("time (us)")
    axes[0].set_ylabel("Gbps (2 us bins)")
    axes[0].set_ylim(0, 105)
    axes[1].set_title("(b) bottleneck queue length")
    axes[1].set_xlabel("time (us)")
    axes[1].set_ylabel("KB")
    axes[0].legend(loc="lower right")
    fig.tight_layout()
    save(fig, os.path.join(figdir, "fig13_reaction.svg"))
    return True


def jain(v):
    s = sum(v)
    q = sum(x * x for x in v)
    return s * s / (len(v) * q) if q > 0 else float("nan")


def fig14(root, figdir, nums):
    wais = [w for w in ["25", "50", "100", "150", "300"] if exists(root, f"fig14-wai{w}")]
    if not wais:
        return None
    fig, axes = plt.subplots(1, 2, figsize=(8.8, 3.2))
    nums["fig14"] = {}
    for w, color in zip(wais, WAI_RAMP):
        d = os.path.join(root, f"fig14-wai{w}")
        tr = read_series(os.path.join(d, "trace.csv"))
        flows = [k for k in tr if k.startswith("flow")]
        t = tr["t_ps"]
        js = [jain([tr[k][i] for k in flows]) for i in range(len(t))]
        axes[0].plot([x / 1e9 + 0.05 for x in t], js, color=color, label=f"W_AI = {w} B", lw=1.6)
        q = read_series(os.path.join(d, "queue_trace.csv"))["queue_bytes"]
        qs = sorted(q)
        cdf = [(i + 1) / len(qs) for i in range(len(qs))]
        axes[1].plot([max(v, 1) / 1000 for v in qs], [100 * c for c in cdf], color=color, label=f"W_AI = {w} B", lw=1.6)
        last = [tr[k][-20:] for k in flows]
        per_flow = [sum(x) * 8 / (len(x) * (t[1] - t[0]) * 1e-12) / 1e9 for x in last]
        nums["fig14"][w] = {
            "queue_p95_kb": pctl(qs, 0.95) / 1000,
            "queue_p99_kb": pctl(qs, 0.99) / 1000,
            "jain_first_ms": jain([sum(tr[k][:10]) for k in flows]),
            "jain_last_2ms": jain(per_flow),
            "goodput_last_2ms_gbps": sum(per_flow),
        }
    axes[0].set_title("(a) fairness of the 16 flows over time")
    axes[0].set_xlabel("time (ms)")
    axes[0].set_ylabel("Jain's index (100 us bins)")
    axes[0].set_ylim(0.85, 1.005)
    axes[0].legend(loc="lower right", fontsize=8, ncol=2)
    axes[1].set_title("(b) queue length, sampled every 1 us")
    axes[1].set_xscale("log")
    axes[1].set_xlim(1, 2000)
    axes[1].set_ylim(80, 100.5)
    axes[1].set_xlabel("KB (log scale; empty queue drawn at 1 KB)")
    axes[1].set_ylabel("CDF (%) - top 20% shown")
    fig.tight_layout()
    save(fig, os.path.join(figdir, "fig14_wai.svg"))
    return True


def micro_traces(root, kind, cc):
    d = os.path.join(root, f"fig9-{kind}-{cc}")
    if not exists(root, f"fig9-{kind}-{cc}"):
        return None
    tr = read_series(os.path.join(d, "trace.csv"))
    q = read_series(os.path.join(d, "queue_trace.csv"))
    return d, tr, q


def fig9(root, figdir, nums):
    nums["fig9"] = {}
    made = False
    # (a)-(d): throughput (top) and buffer (bottom) for HPCC and DCQCN.
    for kind, names, title in [("longshort", ["long flow", "short flow (1 MB)"], "long-short"),
                               ("incast", ["long flow"], "7-to-1 incast onto a long flow")]:
        if not all(micro_traces(root, kind, cc) for cc in ["hpcc", "dcqcn"]):
            continue
        made = True
        fig, axes = plt.subplots(2, 2, figsize=(8.8, 4.8), sharex=True)
        nums["fig9"][kind] = {}
        for col, cc in enumerate(["hpcc", "dcqcn"]):
            d, tr, q = micro_traces(root, kind, cc)
            t = [x / 1e9 for x in tr["t_ps"]]
            bin_s = (tr["t_ps"][1] - tr["t_ps"][0]) * 1e-12
            flows = [k for k in tr if k.startswith("flow")]
            series = {}
            for i, k in enumerate(flows[:len(names)]):
                g = [b * 8 / bin_s / 1e9 for b in tr[k]]
                series[names[i]] = g
                axes[0][col].plot(t, g, color=FLOW_COLORS[i], label=names[i], lw=1.5)
            if kind == "incast":
                others = [sum(tr[k][i] for k in flows[1:]) * 8 / bin_s / 1e9 for i in range(len(t))]
                axes[0][col].plot(t, others, color=FLOW_COLORS[1], label="7 incast flows (total)", lw=1.5)
            qt = [x / 1e9 for x in q["t_ps"]]
            axes[1][col].plot(qt, [b / 1000 for b in q["queue_bytes"]], color=INK2, lw=1.3)
            axes[0][col].set_title(f"({'abcd'[col + (2 if kind == 'incast' else 0)]}) {LABEL[cc]}: {title}")
            axes[0][col].set_ylabel("goodput (Gbps)")
            axes[0][col].set_ylim(0, 27)
            axes[1][col].set_ylabel("bottleneck buffer (KB)")
            axes[1][col].set_xlabel("time (ms)")
            long = series[names[0]]
            info = {"peak_buffer_kb": max(q["queue_bytes"]) / 1000}
            if kind == "longshort":
                # When does the long flow get back to 95% of its pre-join rate?
                ends = [r for r in csv.DictReader(open(os.path.join(d, "fct.csv")))]
                short_end = (int(ends[0]["start_ps"]) + int(ends[0]["fct_ps"])) / 1e9 if ends else None
                pre = sum(long[int(0.5e-3 / bin_s):int(0.9e-3 / bin_s)]) / max(1, int(0.4e-3 / bin_s))
                rec = None
                after = None
                if short_end:
                    k0 = next(i for i, tt in enumerate(t) if tt > short_end)
                    # First bin from which the long flow stays >= 95% of its
                    # pre-join rate for 5 consecutive bins (100 us); a single
                    # spike while the queue drains does not count.
                    for i in range(k0, len(t) - 5):
                        if all(long[j] >= 0.95 * pre for j in range(i, i + 5)):
                            rec = (t[i] - short_end) * 1000
                            break
                    w = long[k0:k0 + int(1e-3 / bin_s)]
                    after = sum(w) / len(w) / pre if w else None
                info.update({"short_flow_end_ms": short_end, "long_rate_before_gbps": pre,
                             "recovery_us_to_95pct": rec, "long_rate_next_1ms_frac": after})
            nums["fig9"][kind][cc] = info
        axes[0][0].legend(loc="lower left", fontsize=7.5)
        fig.tight_layout()
        save(fig, os.path.join(figdir, f"fig9_{kind}.svg"))
    # (e)-(f): mice latency and queue CDFs.
    if all(micro_traces(root, "mice", cc) for cc in ["hpcc", "dcqcn"]):
        made = True
        fig, axes = plt.subplots(1, 2, figsize=(8.8, 3.0))
        nums["fig9"]["mice"] = {}
        for cc in ["hpcc", "dcqcn"]:
            d, tr, q = micro_traces(root, "mice", cc)
            lat = sorted(int(r["fct_ps"]) / 1e6 for r in csv.DictReader(open(os.path.join(d, "fct.csv")))
                         if r["tag"] == "bg")
            axes[0].plot(lat, [100 * (i + 1) / len(lat) for i in range(len(lat))], color=COLOR[cc], label=LABEL[cc])
            qs = sorted(b / 1000 for b in q["queue_bytes"])
            axes[1].plot(qs, [100 * (i + 1) / len(qs) for i in range(len(qs))], color=COLOR[cc], label=LABEL[cc])
            nums["fig9"]["mice"][cc] = {"latency_p50_us": pctl(lat, 0.5), "latency_p99_us": pctl(lat, 0.99),
                                        "queue_p50_kb": pctl(qs, 0.5), "queue_p99_kb": pctl(qs, 0.99)}
        axes[0].set_title("(e) 1 KB mice: completion time")
        axes[0].set_xlabel("us")
        axes[0].set_ylabel("CDF (%)")
        axes[0].legend(loc="lower right")
        axes[1].set_title("(f) bottleneck queue, sampled every 1 us")
        axes[1].set_xlabel("KB")
        axes[1].set_ylabel("CDF (%)")
        fig.tight_layout()
        save(fig, os.path.join(figdir, "fig9_mice.svg"))
    # (g)-(h): four flows join and leave.
    if all(micro_traces(root, "fairshare", cc) for cc in ["hpcc", "dcqcn"]):
        made = True
        fig, axes = plt.subplots(1, 2, figsize=(8.8, 3.0), sharey=True)
        nums["fig9"]["fairshare"] = {}
        for col, cc in enumerate(["hpcc", "dcqcn"]):
            d, tr, q = micro_traces(root, "fairshare", cc)
            t = [x / 1e9 for x in tr["t_ps"]]
            bin_s = (tr["t_ps"][1] - tr["t_ps"][0]) * 1e-12
            flows = [k for k in tr if k.startswith("flow")]
            for i, k in enumerate(flows):
                axes[col].plot(t, [b * 8 / bin_s / 1e9 for b in tr[k]], color=FLOW_COLORS[i], label=f"flow {i + 1}", lw=1.3)
            # Jain's index while all four run (15-20 ms window).
            a, b = int(15e-3 / bin_s), int(20e-3 / bin_s)
            nums["fig9"]["fairshare"][cc] = {"jain_4flows": jain([sum(tr[k][a:b]) for k in flows])}
            axes[col].set_title(f"({'gh'[col]}) {LABEL[cc]}: 4 flows join every 5 ms, then leave")
            axes[col].set_xlabel("time (ms)")
        axes[0].set_ylabel("goodput (Gbps, 100 us bins)")
        axes[0].legend(loc="upper right", fontsize=7.5)
        fig.tight_layout()
        save(fig, os.path.join(figdir, "fig9_fairshare.svg"))
    return made or None


def extras(root, nums):
    """Seed variation and the maxStage sensitivity run."""
    out = {}
    for name in sorted(os.listdir(root)) if os.path.isdir(root) else []:
        if name.startswith("seed") or name.startswith("sens-"):
            if not exists(root, name):
                continue
            d = os.path.join(root, name)
            bins = size_bins(measured_fcts(d))
            s = summary(d)
            out[name] = {"bins": bins, "rtt_p95_us": s["rtt_p95_ps"] / 1e6, "pause_frames": s["pause_frames"]}
    nums["extras"] = out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True)
    ap.add_argument("--figures", required=True)
    ap.add_argument("--html", required=True)
    ap.add_argument("--numbers", required=True)
    ap.add_argument("--label", default="full")
    a = ap.parse_args()
    os.makedirs(a.figures, exist_ok=True)
    nums = {"set": a.label}
    made = {}
    for name, fn in [("fig11", fig11), ("fig10", fig10), ("fig13", fig13), ("fig14", fig14), ("fig9", fig9)]:
        made[name] = fn(a.results, a.figures, nums)
    extras(a.results, nums)
    with open(a.numbers, "w") as f:
        json.dump(nums, f, indent=1, sort_keys=True)
    rel = os.path.relpath(a.figures, os.path.dirname(os.path.abspath(a.html)))
    with open(a.html, "w") as f:
        docs = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs")
        f.write(render_page(nums, rel, a.label, os.path.relpath(docs, os.path.dirname(os.path.abspath(a.html)))))
    print(f"figures: {sorted(os.listdir(a.figures))}")
    print(f"wrote {a.html} and {a.numbers}")


if __name__ == "__main__":
    main()
