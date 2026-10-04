# hpcc-repro

An independent, from-scratch packet-level simulator that reproduces the evaluation of
**"HPCC: High Precision Congestion Control"** (Yuliang Li et al., ACM SIGCOMM 2019), written to learn how a
systems-research result is checked. It is a reproduction study: the algorithm and the experiments are the paper's,
nothing here is new.

HPCC lets every switch on a packet's path stamp its queue length, transmitted bytes and link speed into the packet
(In-band Network Telemetry); the sender reads them back from the ACK and sets its window from the exact load of the
busiest link. The paper claims this gives near-zero queues, much better tail latency for short flows and no PFC
pause storms, compared with DCQCN, TIMELY and DCTCP. This repository rebuilds the paper's 320-host, 100/400 Gbps
FatTree and its workloads in a deterministic Rust simulator (no code shared with the authors' ns-3 simulator) and
checks those claims figure by figure.

| Paper claim | Paper | This reproduction (`make repro`) |
|---|---|---|
| 95th-pct RTT of HPCC, FB_Hadoop 50% load (Fig. 11d) | 19.8 us | **21.6 us** (3 seeds); 19.8 us if ACKs skip the data queues |
| HPCC vs DCTCP tail RTT | "more than 2x" lower | 1.93x lower (41.7 us vs 21.6 us) |
| PFC pauses, 30% load + incast (Fig. 11b) | only DCQCN and TIMELY; "almost zero" with a window | DCQCN 30 132 and TIMELY 38 280 pause frames; TIMELY+win 20; HPCC, DCTCP, DCQCN+win 0 |
| 95th-pct slowdown, flows <= 120 KB, 30% + incast (Fig. 11a) | HPCC lowest | HPCC 1.85, DCTCP 3.01, DCQCN+win 3.68, TIMELY+win 3.77, DCQCN 102, TIMELY 135 |
| 99th-pct slowdown, flows < 3 KB, WebSearch 50% (Fig. 10) | HPCC 2.70 vs DCQCN 53.9 (testbed) | HPCC 1.92 vs DCQCN 15.5 (simulated FatTree) |
| Long flows slower with HPCC at 50% load (§5.3) | ~1.24x (analytical) | 0.99x at the median, 1.44x at the 95th pct (1.08x with maxStage = 0) |
| W_AI <= 150 B keeps the 95th-pct queue within 4 KB; 300 B gives 13 KB (Fig. 14) | 4 KB / 13 KB | 4.4 KB / 12.0 KB |
| Per-ACK overreacts, per-RTT is slow, HPCC is both fast and stable (Fig. 13) | qualitative | per-ACK goodput falls to 0 Gbps; per-RTT queue lasts 190 us vs 108 us for HPCC |

Full discussion: **[docs/report.md](docs/report.md)** (claims, setup, figure-by-figure comparison, deviations,
threats to validity). All figures with the paper's description next to each: **[docs/results.html](docs/results.html)**
(open the file in a browser; no scripts, no external resources).

![Figure 11 reproduction](docs/figures/fig11_fct.svg)

## Demo

Two runs of the scaled-down (32-host) FatTree with FB_Hadoop at 30% load plus 60-to-1 incasts, real output:

```
$ ./target/release/hpcc-sim fct --cc hpcc --workload fbhadoop --load 0.3 --incast --small --out results/demo-hpcc
experiment=fct topology=fattree-small-32 workload=FB_Hadoop load=0.3 incast=true warmup_ps=1000000000 measure_ps=5000000000 cc=hpcc seed=1 pfc=true wai=default max_stage=default acks=data-class: 9137 flows done, sim 9.178 ms, 20433708 events in 1.0 s, drops 0, pause frames 0, host pause 0.0 us, switch pause 0.0 us, rtt p95 25.0 us
$ ./target/release/hpcc-sim fct --cc dcqcn --workload fbhadoop --load 0.3 --incast --small --out results/demo-dcqcn
experiment=fct topology=fattree-small-32 workload=FB_Hadoop load=0.3 incast=true warmup_ps=1000000000 measure_ps=5000000000 cc=dcqcn seed=1 pfc=true wai=default max_stage=default acks=data-class: 8049 flows done, sim 8.092 ms, 18201755 events in 1.0 s, drops 0, pause frames 16, host pause 112.7 us, switch pause 57.4 us, rtt p95 620.8 us
```

Each run writes `fct.csv` (every flow's FCT and exact ideal FCT), `summary.json`, queue and RTT histograms into
its `--out` directory. On macOS, double-click **`跑跑看.command`** to build, run `make quick` and open the page.

## How it works

```
 hosts (RDMA NIC model)                 switches (shared buffer, PFC, ECN, INT, ECMP)
 +--------------------------+  data   +-------------------------------------------------+
 | per-flow pacing + window |-------->| ingress accounting -> PFC PAUSE/RESUME upstream |
 | CC: HPCC / DCQCN / DCTCP |         | egress queue -> at dequeue: ECN mark, INT stamp |
 |     / TIMELY             |<--------|   {ts, txBytes, qlen, B} per hop                |
 | receiver: ACK + INT echo |  ACK    +-------------------------------------------------+
 +--------------------------+
            all driven by one event queue ordered by (picosecond time, sequence number)
```

* **Event core** (`src/event.rs`, `src/time.rs`): integer picoseconds, so 100 Gbps is exactly 80 ps per byte; ties
  run in scheduling order, which makes every run reproducible bit for bit.
* **Network** (`src/sim.rs`, `src/mmu.rs`, `src/routing.rs`): store-and-forward links, two strict-priority queues
  per port (control, data), dynamic-threshold shared buffer with PFC headroom, RED ECN at dequeue, INT at dequeue,
  per-flow ECMP.
* **Algorithms** (`src/cc/`): HPCC Algorithm 1 line by line (plus the per-ACK and per-RTT variants of Figure 13),
  DCQCN and DCQCN+win, DCTCP, TIMELY and TIMELY+win, as pure state machines with unit tests.
* **Experiments** (`src/experiments.rs`, `analysis/`): the paper's FatTree, WebSearch/FB_Hadoop traffic, incasts and
  micro-benchmarks; plots with matplotlib.

Parameters follow the paper (eta = 95%, maxStage = 5, W_AI = 80 B, T = 13 us, 32 MB buffers, DCQCN/DCTCP ECN
thresholds scaled with link speed, PFC at ~11% of free buffer, 42 B INT overhead). Where the paper is silent the
value comes from the authors' public scripts; [docs/DESIGN.md](docs/DESIGN.md) lists every such choice.

## Results

Figures 10 and 11 were reproduced at the paper's full simulation scale (320 hosts); Figures 9, 13 and 14 on the
paper's small micro-benchmark setups (Figures 9 and 10 were testbed experiments in the paper and are simulated
here). Summary of verdicts from [docs/report.md](docs/report.md):

* **Matches:** HPCC's short-flow advantage (Fig. 10, 11), PFC pauses only without a window and almost none with
  one (Fig. 11b), HPCC's low tail RTT, reaction variants (Fig. 13), W_AI threshold (Fig. 14), long-short recovery,
  incast reaction and fairness (Fig. 9).
* **Partly:** HPCC's 95th-percentile RTT is 21.6 us vs the paper's 19.8 us (exactly 19.8 us if ACKs skip the data
  queues; the paper does not define its latency metric); the long-flow penalty is larger at the 95th percentile
  than the paper's estimate unless maxStage = 0; DCQCN's tails in Fig. 10 and its mice latency in Fig. 9e are less
  extreme than on the paper's testbed; DCTCP's tail RTT is 1.93x HPCC's rather than more than 2x.
* **Not reproduced:** Figure 12 (IRN is not implemented), Figures 1-3 and 6.

### Cross-check against the authors' simulator

The authors' ns-3 simulator ([alibaba-edu/High-Precision-Congestion-Control](https://github.com/alibaba-edu/High-Precision-Congestion-Control))
uses waf 1.7, which needs Python 2. `tools/build_official_ns3.py` compiles the ten ns-3 modules it needs directly
with clang++ (20 s on 4 cores; their code is cloned to a scratch directory and never committed). Both simulators
were run on the *same* 32-host topology and the *same* 23 175 FB_Hadoop flows at 50% load
(`hpcc-sim fct ... --export-ns3 DIR`; `hpcc-sim ns3` also reads their input format directly), with the paper's
HPCC parameters and the authors' `run.py` values for the others. Slowdowns of the ~4 945 flows starting in
[1 ms, 4 ms), all scored against the same exact ideal FCT (median / 95th / 99th percentile):

| | authors' simulator | this simulator (seed 1) | per-bin deviation vs own seed spread |
|---|---|---|---|
| HPCC | 1.12 / 3.64 / 7.55 | 1.14 / 3.69 / 7.36 | 3.8% vs 3.2% |
| DCQCN | 1.66 / 10.74 / 20.14 | 1.73 / 10.73 / 19.85 | 4.9% vs 9.0% |
| DCTCP | 1.65 / 5.34 / 8.28 | 1.71 / 5.60 / 8.46 | 4.1% vs 2.5% |
| TIMELY | 1.66 / 18.93 / 43.33 | 1.64 / 18.29 / 45.68 | 2.5% vs 13.2% |

All four agree to within a few percent, about the size of this simulator's own seed-to-seed variation. They did
not at first: DCQCN's tails were 1.2-1.9x higher in the authors' simulator. Diffing the two DCQCN implementations
mechanism by mechanism found them equivalent, and the switch queues of both simulators were statistically the same,
so the delay had to be outside the forward queues. A packet trace of their simulator showed ACKs waiting in the
**data** queue at switches. Their `scratch/third.cc` applies `ACK_HIGH_PRIO` only to host NICs, and the switches'
`AckHighPrio` attribute keeps its default of 0. Their config documentation says "1: prioritize ACK", so this looks
unintended, but it is how their results were produced. This simulator originally gave ACKs a high-priority class.
It now defaults to the authors' behaviour and keeps the original as `--acks-high-priority`, which `make repro`
runs as a sensitivity case: DCQCN's short-flow 95th percentile at 50% load is 5.6 with priority ACKs vs 8.2 without,
and every qualitative conclusion holds under both. Details: [docs/DESIGN.md](docs/DESIGN.md) §7 and
[docs/report.md](docs/report.md) §6.

### Runtime

`make repro` runs 41 simulations (the paper's 320-host FatTree for Figures 10 and 11) and draws the figures in
**7.7 minutes** of wall time with 4 parallel jobs on an Apple M5 (10 cores, shared with other jobs while measured).
Results are bit-identical between runs and between arm64 and x86-64 builds (checked under Rosetta 2). One full-scale run takes
20-150 s; the simulator processes about 13 million events per second on one core (`make bench`), with about 130 MB
of memory per full-scale run. `make quick` (32-host tree, shorter windows) takes about 15 s.

## Validation

The simulator is tested separately from the algorithms it runs (`cargo test --release`, 75 tests):

* **Timing**: a single packet's latency equals the hand-computed value; lone flows of awkward sizes on four
  topologies finish exactly, to the picosecond, at the analytic store-and-forward FCT used as the slowdown baseline.
* **Buffers and PFC**: 600 000 random MMU operations with invariant checks after each; pause exactly at the
  threshold and resume exactly 3 KB below it; no drop ever with PFC on (including PFC spreading through three
  switches); with PFC off, drops occur (including dropped ACKs) and go-back-N recovers.
* **Byte conservation** (injected = delivered + dropped + in flight) at many points of 12 randomised runs.
* **ECMP**: deterministic, even spread, uses every tuple field. **Event queue**: FIFO ties, randomised model check.
* **Algorithms**: unit tests for each CC; on the network, lone long flows reach line rate (HPCC within a few percent of
  the rate predicted from eta and the INT overhead), four flows converge to a fair share for all six schemes, DCTCP holds its queue near K, a sending
  window stops DCQCN from triggering PFC, HPCC sees a bottleneck five switches away.
* **ACK model**: ACKs in the data class wait behind data, those in the control class do not.
* **Determinism**: same seed, same output; no libm, fused multiply-add, hash maps or unstable sorts in the model (a
  test enforces it); golden digests are identical on arm64 and x86-64 and are rechecked in CI on Linux.
* **Mutation check**: 19 deliberate one-line bugs (HPCC reading the queue length of the wrong hop, PFC off by one
  byte, ECN threshold not scaled, ...) must each break the suite: [docs/mutants.md](docs/mutants.md).

## Limitations

* No NIC/PCIe processing delay, ACK coalescing, PFC pause quanta (explicit XOFF/XON instead), multiple lossless
  classes, link failures, IRN, or HPCC-PINT.
* ACKs share the data queue in switches, as in the authors' simulator (see the cross-check); the other choice is a
  command-line option.
* Measurement windows are 5-10 ms of arrivals (50 000-83 000 flows for FB_Hadoop, 7 000-12 000 for WebSearch), so
  the 99th percentiles of WebSearch's size bins rest on 7-12 flows.
* The paper's pause-fraction and queue-length populations are not precisely defined; the definitions used here are
  stated on the results page.
* Only one configuration (32 hosts, FB_Hadoop 50%) was cross-checked against the authors' simulator; the
  320-host runs were not repeated there.

## Related work

* Li et al., *HPCC: High Precision Congestion Control*, SIGCOMM 2019, and the authors' ns-3 simulator
  [alibaba-edu/High-Precision-Congestion-Control](https://github.com/alibaba-edu/High-Precision-Congestion-Control)
  (ns-3.17 based; it also contains HPCC-PINT from Ben Basat et al., *PINT*, SIGCOMM 2020). The traffic CDFs used here
  are the ones tabulated in that repository's `traffic_gen/`.
* [ns-3](https://www.nsnam.org/), the simulator framework the authors used, and
  [ns3-rdma](https://github.com/bobzhuyb/ns3-rdma), the DCQCN authors' RDMA extension it descends from.
* [inet-tub/ns3-datacenter](https://github.com/inet-tub/ns3-datacenter) (PowerTCP, NSDI 2022, and later work), which
  extends the ns3-rdma and HPCC code bases.
* DCQCN: Zhu et al., *Congestion Control for Large-Scale RDMA Deployments*, SIGCOMM 2015. DCTCP: Alizadeh et al.,
  *Data Center TCP*, SIGCOMM 2010. TIMELY: Mittal et al., *TIMELY: RTT-based Congestion Control for the Datacenter*,
  SIGCOMM 2015. Workloads: the DCTCP paper (WebSearch) and Roy et al., *Inside the Social Network's (Datacenter)
  Network*, SIGCOMM 2015 (FB_Hadoop).

The other HPCC simulators I found are forks or extensions of the authors' ns-3 code. To my knowledge this is an
independent re-implementation rather than a derivative of it; it is a reproduction for study and makes no claim
beyond checking the paper's results.

## Build and test

Requirements: Rust stable (`cargo`), Python 3 (the experiment runner uses only the standard library; plots use
matplotlib, installed into `.venv` by the Makefile), `make`.

```
make build      # release build of target/release/hpcc-sim
make test       # 75 tests: unit, network-model validation, CC behaviour, portability, golden digests
make lint       # rustfmt, clippy -D warnings, Python syntax
make quick      # 32-host experiment set + figures + results/quick/index.html (~15 s)
make repro      # full experiment set at paper scale + docs/figures + docs/results.html (~8 min, 4 jobs)
make bench      # one fixed full-scale run, prints events/s
make mutants    # mutation check, rewrites docs/mutants.md (~7 min)
```

All Makefile paths are relative, so the repository can live in a directory whose name has spaces or non-ASCII
characters. `JOBS=n make repro` changes the parallelism.

## Layout

```
src/            simulator (event queue, packets, switch MMU, routing, hosts/switches, CC algorithms, workloads)
src/bin/        hpcc-sim command-line driver
tests/          network-model validation and CC behaviour tests
analysis/       experiment matrix (run_experiments.py), figures and results page (figures.py, page.py)
tools/          mutation check, ns-3 cross-check build and comparison
docs/           report, design notes, beginner guide (Traditional Chinese), figures, results page
```

## License

MIT, see [LICENSE](LICENSE).
