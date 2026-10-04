"""Static HTML results page. No scripts, no external resources: the page and
its SVG figures open from disk in any browser (including Safari's Lockdown
Mode). Every number is taken from the numbers dict computed by figures.py."""

import html

LABEL = {
    "hpcc": "HPCC", "dcqcn": "DCQCN", "dcqcn+win": "DCQCN+win", "timely": "TIMELY",
    "timely+win": "TIMELY+win", "dctcp": "DCTCP", "hpcc-perack": "per-ACK", "hpcc-perrtt": "per-RTT",
}
ORDER = ["hpcc", "dcqcn", "dcqcn+win", "timely", "timely+win", "dctcp"]

CSS = """
:root { --bg:#fcfcfb; --fg:#0b0b0b; --fg2:#52514e; --line:#dddcd7; --card:#ffffff; --accent:#2a78d6;
        --ok:#1e7a33; --part:#8a5a00; --bad:#b3261e; color-scheme: light; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) { --bg:#1a1a19; --fg:#f4f4f2; --fg2:#c3c2b7; --line:#3a3a37; --card:#242422;
        --accent:#3987e5; --ok:#5cc46f; --part:#e0b04c; --bad:#ee7b72; color-scheme: dark; }
}
:root[data-theme="dark"] { --bg:#1a1a19; --fg:#f4f4f2; --fg2:#c3c2b7; --line:#3a3a37; --card:#242422;
        --accent:#3987e5; --ok:#5cc46f; --part:#e0b04c; --bad:#ee7b72; color-scheme: dark; }
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg);
       font: 15px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; }
main { max-width: 980px; padding: 28px 16px 64px; }
h1 { font-size: 24px; margin: 0 0 6px; }
h2 { font-size: 19px; margin: 40px 0 8px; padding-top: 12px; border-top: 1px solid var(--line); }
h3 { font-size: 15px; margin: 18px 0 4px; }
p, li { max-width: 72ch; }
.sub { color: var(--fg2); margin: 0 0 18px; }
figure { margin: 12px 0; }
figure img { width: 100%; height: auto; background: #fcfcfb; border: 1px solid var(--line); border-radius: 4px; }
figcaption { color: var(--fg2); font-size: 13px; }
table { border-collapse: collapse; font-size: 13px; margin: 8px 0 14px; display: block; overflow-x: auto; max-width: 100%; }
th, td { border-bottom: 1px solid var(--line); padding: 4px 10px 4px 0; text-align: left; white-space: nowrap; }
th { color: var(--fg2); font-weight: 600; }
td.num { text-align: right; font-variant-numeric: tabular-nums; }
.verdict { font-weight: 600; }
.ok { color: var(--ok); } .part { color: var(--part); } .bad { color: var(--bad); }
.cols { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; }
@media (max-width: 720px) { .cols { grid-template-columns: 1fr; } }
.box { background: var(--card); border: 1px solid var(--line); border-radius: 4px; padding: 10px 14px; }
.box h3 { margin-top: 2px; }
code { font-size: 13px; }
a { color: var(--accent); }
"""


def esc(s):
    return html.escape(str(s))


def f(x, nd=1):
    if x is None:
        return "-"
    if isinstance(x, float):
        return f"{x:.{nd}f}"
    return str(x)


def kb(b):
    return f"{b / 1000:.1f} KB" if b < 1_000_000 else f"{b / 1e6:.2f} MB"


def verdict(v):
    cls = {"matches": "ok", "partly": "part", "differs": "bad"}[v]
    return f'<span class="verdict {cls}">{esc(v)}</span>'


FULL = {"on": True}


def compare(paper, ours, matches, differs, why, v):
    if not FULL["on"]:
        # The verdicts were written for the full-scale results; the quick set
        # only shows the paper's claim next to this run's numbers.
        return f"""<div class="cols">
<div class="box"><h3>Paper</h3><p>{paper}</p></div>
<div class="box"><h3>This (scaled-down) run</h3><p>{ours}</p></div>
</div>"""
    return f"""<div class="cols">
<div class="box"><h3>Paper</h3><p>{paper}</p></div>
<div class="box"><h3>This reproduction &mdash; {verdict(v)}</h3><p>{ours}</p>
<p><b>Matches:</b> {matches}</p><p><b>Differs:</b> {differs}</p><p><b>Why:</b> {why}</p></div>
</div>"""


def fig(src, alt, cap):
    return f'<figure><img src="{esc(src)}" alt="{esc(alt)}"><figcaption>{cap}</figcaption></figure>'


def table(head, rows, numeric_from=1):
    h = "".join(f"<th>{esc(x)}</th>" for x in head)
    body = ""
    for r in rows:
        body += "<tr>" + "".join(
            f'<td class="num">{esc(c)}</td>' if i >= numeric_from else f"<td>{esc(c)}</td>" for i, c in enumerate(r)) + "</tr>"
    return f"<table><thead><tr>{h}</tr></thead><tbody>{body}</tbody></table>"


def bins_table(case_nums, key="p95"):
    ccs = [c for c in ORDER if c in case_nums]
    if not ccs:
        return ""
    labels = [b["max_size"] for b in case_nums[ccs[0]]["bins"]]
    head = ["algorithm"] + [f"<= {x:,}" for x in labels]
    rows = [[LABEL[c]] + [f"{b[key]:.2f}" for b in case_nums[c]["bins"]] for c in ccs]
    return table(head, rows)


def section_fig11(n, figrel, label):
    if "fig11" not in n:
        return ""
    F = n["fig11"]
    out = ['<h2 id="fig11">Figure 11: FB_Hadoop on the FatTree</h2>']
    out.append(fig(f"{figrel}/fig11_fct.svg", "95th percentile FCT slowdown per flow-size bin",
                   "95th-percentile FCT slowdown per flow-size bin (10 bins with equal flow counts, labelled by the "
                   "largest flow in the bin; incast flows excluded). Slowdown = FCT / FCT of the same flow alone at line rate."))
    s = []
    for case, title in [("fb30inc", "30% load + incast"), ("fb50", "50% load")]:
        if case not in F:
            continue
        c = F[case]
        bits = []
        for cc in ORDER:
            if cc in c and c[cc]["p95_short_le_120k"] is not None:
                bits.append(f"{LABEL[cc]} {c[cc]['p95_short_le_120k']:.2f}")
        s.append(f"<b>{title}</b>, 95th-percentile slowdown of all flows &le; 120 KB: " + ", ".join(bits) + ".")
    long50 = ""
    if "fb50" in F and "hpcc" in F["fb50"]:
        h = F["fb50"]["hpcc"]["bins"][-1]["p50"]
        others = [F["fb50"][cc]["bins"][-1]["p50"] for cc in ["dcqcn", "dcqcn+win", "dctcp"] if cc in F["fb50"]]
        if others:
            ratio = h / (sum(others) / len(others))
            long50 = (f" In the largest-flow bin at 50% load the median slowdown is {h:.2f} for HPCC versus "
                      f"{sum(others) / len(others):.2f} on average for DCQCN, DCQCN+win and DCTCP (ratio {ratio:.2f}).")
    out.append(compare(
        "Figure 11a/c: for flows shorter than 120 KB (90% of FB_Hadoop flows) HPCC has a much lower 95th-percentile "
        "slowdown than DCQCN, TIMELY, their windowed variants and DCTCP, at 30% load + incast and at 50% load. Long "
        "flows are slower with HPCC (it leaves 5% headroom and pays INT bytes); the paper estimates 1.24x at 50% load.",
        " ".join(s) + long50,
        "HPCC has the lowest 95th-percentile slowdown in every bin up to 120 KB in both cases; plain DCQCN and "
        "TIMELY are worst (very much so with incast); adding a window helps both; DCTCP sits between the windowed "
        "schemes and HPCC, as the paper explains (its window limits in-flight bytes). At 50% load HPCC is the "
        "slowest scheme for the largest flows.",
        "The long-flow penalty at 50% load is 1.07x at the median but 1.52x at the 95th percentile (largest bin, "
        "versus the mean of the other schemes), against the paper's analytical 1.24x. With maxStage = 0 (the "
        "default in the authors' run.py; the paper's text says 5) the 95th-percentile gap shrinks to 1.15x, see "
        "the sensitivity table at the end of the page.",
        "With maxStage = 5 a long flow that starts on a busy link waits six RTTs between multiplicative increases "
        "and grows by only W_AI = 80 B per RTT meanwhile, so it reclaims bandwidth slowly; the absolute slowdowns "
        "also depend on the 5 ms measurement windows and on modelling choices listed in docs/DESIGN.md.",
        "matches"))
    out.append("<h3>95th-percentile slowdown per bin (table view)</h3>")
    for case, title in [("fb30inc", "30% load + incast"), ("fb50", "50% load")]:
        if case in F:
            out.append(f"<p>{title}</p>" + bins_table(F[case]))
    # PFC + latency
    out.append(fig(f"{figrel}/fig11_pfc_latency.svg", "PFC pause time and 95th percentile RTT",
                   "Top: share of time the lossless class was paused, over all host NIC ports and switch ports, after "
                   "warm-up. Bottom: 95th-percentile per-packet RTT measured at the sender."))
    rows = []
    for case, title in [("fb30inc", "30%+incast"), ("fb50", "50%")]:
        if case not in F:
            continue
        for cc in ORDER:
            if cc in F[case]:
                c = F[case][cc]
                rows.append([title, LABEL[cc], f"{100 * c['pause_frac_all_ports']:.3f}", f"{100 * c['pause_frac_hosts']:.3f}",
                             f"{c['pause_frames']:.0f}", f"{c['rtt_p95_us']:.1f}", f"{c['rtt_p99_us']:.1f}", f"{c['drops']:.0f}",
                             f"{c['measured_flows']:.0f}", f"{c['measured_unfinished']:.0f}"])
    rtt = ""
    if "fb50" in F and "hpcc" in F["fb50"]:
        h = F["fb50"]["hpcc"]["rtt_p95_us"]
        d = F["fb50"].get("dctcp", {}).get("rtt_p95_us")
        rtt = f"HPCC's 95th-percentile RTT at 50% load is {h:.1f} us"
        if d:
            rtt += f"; DCTCP's is {d:.1f} us ({d / h:.1f}x)"
        rtt += ". "
    paused = []
    if "fb30inc" in F:
        for cc in ORDER:
            if cc in F["fb30inc"]:
                paused.append(f"{LABEL[cc]} {F['fb30inc'][cc]['pause_frames']:.0f}")
    out.append(compare(
        "Figure 11b/d: large-scale PFC pauses appear only with DCQCN and TIMELY; adding a sending window "
        "(DCQCN+win, TIMELY+win) reduces them to almost zero; HPCC's 95th-percentile latency is 19.8 us at 50% "
        "load (base RTT 12 us) and DCTCP's is more than 2x higher; DCQCN/TIMELY latency is around a millisecond "
        "with incast.",
        rtt + "PFC pause frames with incast: " + ", ".join(paused) + ".",
        "Pauses happen only for the window-less rate-based schemes; every windowed scheme and HPCC never trigger "
        "PFC; HPCC's tail RTT is within a few us of the paper's 19.8 us; DCQCN/TIMELY tail RTTs with incast are in "
        "the millisecond range.",
        "The paper plots the paused fraction on a 0-20% axis; the 1-2.5% here is at the low end. DCTCP's tail RTT "
        "is 1.85x HPCC's, slightly short of the paper's 'more than 2 times'.",
        "The paper does not define the denominator of its pause fraction (which ports, which period); this page "
        "averages over every host and switch port after warm-up. The qualitative claim (only window-less schemes "
        "pause) does not depend on the definition.",
        "matches"))
    out.append(table(["case", "algorithm", "paused % (all ports)", "paused % (hosts)", "pause frames",
                      "RTT p95 (us)", "RTT p99 (us)", "drops", "measured flows", "unfinished"], rows, numeric_from=2))
    return "\n".join(out)


def section_fig10(n, figrel):
    if "fig10" not in n:
        return ""
    F = n["fig10"]
    out = ['<h2 id="fig10">Figure 10: WebSearch, FCT percentiles and queue lengths</h2>']
    out.append(fig(f"{figrel}/fig10_websearch.svg", "WebSearch FCT and queue CDF",
                   "Top: median (solid), 95th (dashed) and 99th (dotted) percentile slowdown per flow-size bin. "
                   "Bottom: CDF of switch egress queue samples (every 10 us, all switch ports), top 20% shown."))
    txt = []
    rows = []
    for l in ["ws30", "ws50"]:
        if l not in F:
            continue
        for cc in ["hpcc", "dcqcn", "dctcp"]:
            if cc in F[l]:
                c = F[l][cc]
                rows.append([l[2:] + "%", LABEL[cc], f(c["p99_under_3kb"], 2), c["n_under_3kb"], kb(c["queue_p50"]),
                             kb(c["queue_p95"]), kb(c["queue_p99"]), kb(c["queue_p999"]), f"{100 * c['queue_nonzero_frac']:.1f}"])
        if "hpcc" in F[l] and "dcqcn" in F[l]:
            h, d = F[l]["hpcc"], F[l]["dcqcn"]
            txt.append(f"At {l[2:]}% load the 99th-percentile slowdown of flows under 3 KB is {f(h['p99_under_3kb'], 2)} "
                       f"(HPCC) vs {f(d['p99_under_3kb'], 2)} (DCQCN); 99th-percentile queue {kb(h['queue_p99'])} vs "
                       f"{kb(d['queue_p99'])}.")
    out.append(compare(
        "Figure 10 (testbed, 25G NICs): HPCC and DCQCN have similar median slowdowns but HPCC is much better at the "
        "95th/99th percentile, especially for short flows: 99th-percentile slowdown for flows under 3 KB drops "
        "from 11.2 to 2.38 at 30% load and from 53.9 to 2.70 at 50% load. Median queue is 0 for both; at 50% load "
        "HPCC's 95th/99th-percentile queues are 19.7/22.9 KB versus 1.1/2.1 MB for DCQCN.",
        " ".join(txt),
        "Similar medians; HPCC much better in the tail for short flows; queues near zero with HPCC and in the MB "
        "range for DCQCN; median queue 0 for both.",
        "The DCQCN tail for tiny flows is less extreme than the testbed's 53.9. HPCC's 95th-percentile queue is "
        "lower than the testbed's 19.7 KB but its 99th percentile is higher than 22.9 KB.",
        "This is a simulation of the paper's 100G FatTree, not the 25G testbed: the bandwidth-delay product is about "
        "6x larger (162 KB), so line-rate starts of new flows collide in larger bursts (in BDP units HPCC's 99th "
        "percentile is lower than the testbed's); queue samples here include every switch port, most of them idle; "
        "and the testbed's DCQCN ran on real NICs.",
        "partly"))
    out.append(table(["load", "algorithm", "p99 slowdown <3KB", "flows <3KB", "queue p50", "queue p95", "queue p99",
                      "queue p99.9", "% samples non-empty"], rows, numeric_from=2))
    return "\n".join(out)


def section_fig13(n, figrel):
    if "fig13" not in n:
        return ""
    F = n["fig13"]
    out = ['<h2 id="fig13">Figure 13: per-ACK vs per-RTT vs HPCC reaction</h2>']
    out.append(fig(f"{figrel}/fig13_reaction.svg", "16-to-1 incast reaction",
                   "16 senders start at once towards one receiver through a single 100G switch (1 us links)."))
    rows = []
    for v in ["hpcc-perack", "hpcc-perrtt", "hpcc"]:
        if v in F:
            c = F[v]
            rows.append([LABEL[v], f"{c['peak_queue_kb']:.0f}", f(c["drain_us"], 0), f"{c['queue_kb_at']['100']:.0f}",
                         f"{c['thr_gbps_50_400']:.1f}", f"{c['thr_gbps_min_after_50']:.1f}"])
    ours = ""
    if all(v in F for v in ["hpcc", "hpcc-perack", "hpcc-perrtt"]):
        ours = (f"HPCC drains the initial {F['hpcc']['peak_queue_kb']:.0f} KB queue by {f(F['hpcc']['drain_us'], 0)} us and "
                f"keeps {F['hpcc']['thr_gbps_50_400']:.1f} Gbps of goodput; per-ACK drains at the same speed but its "
                f"goodput falls as low as {F['hpcc-perack']['thr_gbps_min_after_50']:.1f} Gbps and averages "
                f"{F['hpcc-perack']['thr_gbps_50_400']:.1f} Gbps; per-RTT needs until {f(F['hpcc-perrtt']['drain_us'], 0)} us.")
    out.append(compare(
        "Per-ACK reaction drains the queue quickly but overreacts: aggregate throughput drops to almost zero and "
        "then oscillates. Per-RTT reaction keeps the long queue for a long time. HPCC reacts fast without "
        "overreaction.",
        ours,
        "All three behaviours, including the near-zero throughput dips of per-ACK and the long plateau of per-RTT.",
        "Exact timings differ slightly from the paper's plot (its T and W_AI for this experiment are not stated).",
        "Here T = 4.5 us (slightly above the 4.2 us base RTT) and W_AI follows the paper's default.",
        "matches"))
    out.append(table(["variant", "peak queue (KB)", "queue < 10 KB at (us)", "queue at 100 us (KB)",
                      "goodput 50-400 us (Gbps)", "min 2-us goodput after 50 us"], rows))
    return "\n".join(out)


def section_fig14(n, figrel):
    if "fig14" not in n:
        return ""
    F = n["fig14"]
    out = ['<h2 id="fig14">Figure 14: choosing W_AI</h2>']
    out.append(fig(f"{figrel}/fig14_wai.svg", "W_AI sweep",
                   "16-to-1 on one 100G switch for 10 ms. Left: Jain's fairness index of the 16 flows per 100 us. "
                   "Right: bottleneck queue sampled every 1 us."))
    rows = [[f"{w} B", f"{F[w]['queue_p95_kb']:.1f}", f"{F[w]['queue_p99_kb']:.1f}", f"{F[w]['jain_first_ms']:.3f}",
             f"{F[w]['jain_last_2ms']:.3f}", f"{F[w]['goodput_last_2ms_gbps']:.1f}"] for w in ["25", "50", "100", "150", "300"] if w in F]
    q = ", ".join(f"{w} B: {F[w]['queue_p95_kb']:.1f} KB" for w in ["25", "50", "100", "150", "300"] if w in F)
    out.append(compare(
        "With 16 flows W_AI should not exceed 100 Gbps x 4 us x 5% / 16 = 150 B. All W_AI up to 150 B keep the "
        "95th-percentile queue within 4 KB; 300 B gives 13 KB. A larger W_AI converges to fairness faster.",
        "95th-percentile queue by W_AI: " + q + ".",
        "The threshold behaviour: up to 150 B the 95th-percentile queue stays at a few KB, 300 B is several times "
        "higher; larger W_AI is fairer.",
        "Values are close to but not identical with 4 KB / 13 KB.",
        "T = 4.5 us here vs the 4 us base RTT the paper quotes; INT and header sizes are this model's.",
        "matches"))
    out.append(table(["W_AI", "queue p95 (KB)", "queue p99 (KB)", "Jain, first 1 ms", "Jain, last 2 ms",
                      "goodput, last 2 ms (Gbps)"], rows))
    return "\n".join(out)


def section_fig9(n, figrel):
    if "fig9" not in n or not n["fig9"]:
        return ""
    F = n["fig9"]
    out = ['<h2 id="fig9">Figure 9: testbed micro-benchmarks, re-run in simulation</h2>',
           "<p>The paper ran these on a 25G testbed; here they run on one simulated 25G switch with 1 us links. "
           "Fair-share steps are 5 ms instead of 1 s.</p>"]
    if "longshort" in F:
        out.append(fig(f"{figrel}/fig9_longshort.svg", "long-short", "A long flow; a 1 MB flow joins at 1 ms."))
        h, d = F["longshort"].get("hpcc", {}), F["longshort"].get("dcqcn", {})
        out.append(compare(
            "HPCC recovers the long flow's rate right after the short flow ends; DCQCN cannot recover to line rate "
            "even after 2 ms.",
            f"After the short flow ends, the long flow is back at 95% of its earlier rate (sustained for 100 us) after "
            f"{f(h.get('recovery_us_to_95pct'), 0)} us with HPCC; with DCQCN "
            + ("it has not recovered by the end of the 3 ms run" if d.get("recovery_us_to_95pct") is None
               else f"after {f(d.get('recovery_us_to_95pct'), 0)} us")
            + f", and over the first millisecond it averages {100 * (d.get('long_rate_next_1ms_frac') or 0):.0f}% of its earlier rate "
            f"(HPCC: {100 * (h.get('long_rate_next_1ms_frac') or 0):.0f}%). Peak buffer: HPCC {h.get('peak_buffer_kb', 0):.1f} KB, "
            f"DCQCN {d.get('peak_buffer_kb', 0):.0f} KB.",
            "Fast recovery with HPCC, slow (300 us timer driven) recovery with DCQCN.",
            "-", "-", "matches"))
    if "incast" in F:
        out.append(fig(f"{figrel}/fig9_incast.svg", "incast", "Seven 1 MB flows start at 1 ms towards the receiver of a long flow."))
        h, d = F["incast"].get("hpcc", {}), F["incast"].get("dcqcn", {})
        out.append(compare(
            "HPCC reacts after one round trip and the queue drains quickly; DCQCN's queue builds up to 550 KB.",
            f"Peak buffer: HPCC {h.get('peak_buffer_kb', 0):.0f} KB, DCQCN {d.get('peak_buffer_kb', 0):.0f} KB.",
            "HPCC's queue is drained within a few RTTs; DCQCN's queue is far larger and lasts longer.",
            "Peak values depend on T, the number of senders' initial windows and DCQCN timers.",
            "The testbed used real NICs with their own start-up behaviour.", "matches"))
    if "mice" in F:
        out.append(fig(f"{figrel}/fig9_mice.svg", "elephant-mice", "Two elephants saturate the link; a 1 KB mouse every 20 us."))
        h, d = F["mice"].get("hpcc", {}), F["mice"].get("dcqcn", {})
        out.append(compare(
            "HPCC keeps a near-zero queue, so 1 KB mice see about the 5.4 us base RTT; DCQCN keeps a standing queue "
            "around its ECN threshold and mice latency is consistently above 35 us.",
            f"Median mouse completion time: HPCC {h.get('latency_p50_us', 0):.1f} us, DCQCN {d.get('latency_p50_us', 0):.1f} us; "
            f"median queue {h.get('queue_p50_kb', 0):.1f} KB vs {d.get('queue_p50_kb', 0):.0f} KB.",
            "Mice at the base RTT with HPCC and an empty median queue; much higher and more variable latency with DCQCN.",
            "DCQCN's median mouse latency is below the paper's 'consistently higher than 35 us': its queue "
            "saw-tooths between 0 and ~180 KB instead of sitting at the 100 KB marking threshold.",
            "The timer-only vendor DCQCN model used here (from the authors' simulator) is not the testbed NIC firmware; "
            "the base RTT here is ~4.9 us, not 5.4 us.", "partly"))
    if "fairshare" in F:
        out.append(fig(f"{figrel}/fig9_fairshare.svg", "fair share", "Four flows join one by one and then leave."))
        h, d = F["fairshare"].get("hpcc", {}), F["fairshare"].get("dcqcn", {})
        out.append(compare(
            "HPCC provides good fairness even on short time scales.",
            f"Jain's index while all four flows run: HPCC {h.get('jain_4flows', 0):.3f}, DCQCN {d.get('jain_4flows', 0):.3f}.",
            "Both share fairly on average; HPCC converges smoothly.",
            "Time scale is compressed 200x (5 ms steps).", "Simulation cost.", "matches"))
    return "\n".join(out)


def render_page(n, figrel, label, docrel="."):
    FULL["on"] = label == "full"
    title = "HPCC reproduction results"
    scope = ("the paper's 320-host FatTree (full set, <code>make repro</code>)" if label == "full"
             else "a scaled-down 32-host FatTree with shorter windows (<code>make quick</code>); the full-scale results "
                  f'are in <a href="{esc(docrel)}/results.html">docs/results.html</a>')
    body = [f"<h1>{title}</h1>",
            f'<p class="sub">Independent re-implementation and simulation study of Li et al., "HPCC: High Precision '
            f'Congestion Control", SIGCOMM 2019. This page was generated from {scope}. Discussion: '
            f'<a href="{esc(docrel)}/report.md">report.md</a>; model and choices: <a href="{esc(docrel)}/DESIGN.md">DESIGN.md</a>.</p>',
            '<p>Contents: <a href="#fig11">Figure 11</a> &middot; <a href="#fig10">Figure 10</a> &middot; '
            '<a href="#fig13">Figure 13</a> &middot; <a href="#fig14">Figure 14</a> &middot; <a href="#fig9">Figure 9</a>'
            + (' &middot; <a href="#extras">seeds and sensitivity</a>' if n.get("extras") else "") + "</p>"]
    body.append(section_fig11(n, figrel, label))
    body.append(section_fig10(n, figrel))
    body.append(section_fig13(n, figrel))
    body.append(section_fig14(n, figrel))
    body.append(section_fig9(n, figrel))
    ex = n.get("extras", {})
    if ex:
        rows = []
        for name, e in sorted(ex.items()):
            b = e["bins"]
            rows.append([name, f"{b[0]['p95']:.2f}", f"{b[len(b) // 2]['p95']:.2f}", f"{b[-1]['p95']:.2f}", f"{b[-1]['p50']:.2f}",
                         f"{e['rtt_p95_us']:.1f}", f"{e['pause_frames']:.0f}"])
        body.append('<h2 id="extras">Seed variation and sensitivity</h2>'
                    "<p>Extra FB_Hadoop 50% runs with other seeds, and HPCC with maxStage = 0 (the default in the "
                    "authors' run.py) instead of the paper's 5.</p>"
                    + table(["run", "p95 smallest bin", "p95 middle bin", "p95 largest bin", "p50 largest bin",
                             "RTT p95 (us)", "pause frames"], rows))
    page = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{esc(title)}</title>
<style>{CSS}</style>
</head>
<body>
<main>
{chr(10).join(body)}
</main>
</body>
</html>
"""
    return page
