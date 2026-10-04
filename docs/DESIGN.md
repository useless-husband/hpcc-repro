# Design

This document explains how the simulator is built, which parts of the paper's setup it models and how, the
problems that were hard to get right, and the alternatives that were rejected. The reproduction results and their
discussion are in [report.md](report.md).

## Goal and scope

The goal is to reproduce the *evaluation* of "HPCC: High Precision Congestion Control" (Li et al., SIGCOMM 2019)
with an independently written simulator, so that agreement with the paper means something: if two separate
implementations of the same model agree, neither is likely to be wrong in the same way. The simulator therefore
models exactly what the paper's results depend on, and nothing else:

| Part | Modelled | Not modelled |
|---|---|---|
| Links | full duplex; serialization at line rate; propagation delay; store-and-forward | bit errors, link failures, FEC, inter-frame gap |
| Switches | output queues, shared buffer with dynamic PFC thresholds and headroom, RED-style ECN at dequeue, INT stamping at dequeue, ECMP | cut-through, multiple lossless classes, PFC pause quanta (explicit XOFF/XON instead), Fig. 12's IRN |
| NICs | per-flow pacing (rate), in-flight window, round-robin between flows, ACK per packet, PFC response, go-back-N when PFC is off | PCIe/DMA and NIC processing delay, ACK coalescing, hardware division tables (§4.3) |
| Senders | HPCC (and per-ACK / per-RTT variants), DCQCN, DCQCN+win, DCTCP, TIMELY, TIMELY+win | HPCC-PINT, IRN, Swift |
| Traffic | WebSearch and FB_Hadoop size CDFs, Poisson arrivals, 60-to-1 incast at 2% of capacity, hand-placed micro-benchmarks | real application traces |

## Why Rust

The full experiment set simulates a 320-host, 56-switch network at 100/400 Gbps for tens of milliseconds, which is
several billion events. The choice was between C++, Rust and Go:

* **Speed.** A single-threaded Rust build processes about 13 million events per second on this machine (Apple M5),
  so one full-scale run of the paper's FatTree takes 20 s to just over 2 min and the whole `make repro` set takes about
  6.5 minutes on 4 cores. C++ would be about as fast; Go was expected to be somewhat slower (garbage collector,
  less aggressive optimiser) and Python far slower. These alternatives were not benchmarked.
* **Correctness.** Packets move between queues, wires and the receiver many millions of times. In Rust a packet
  is a `u32` index into a slab that is owned by exactly one place at a time; the pool asserts on double free and on
  use-after-free in debug builds, and the byte-conservation test checks that nothing is ever leaked. There is no
  undefined behaviour to chase.
* **Determinism and portability.** No garbage collector, no hidden hash-map iteration order in the hot path, and
  IEEE-754 arithmetic that is identical on arm64 and x86-64 (Rust never fuses multiply-adds on its own).
* **No dependencies.** The crate has zero dependencies, builds offline in seconds and has nothing to audit.

ns-3 (the authors' simulator) is C++ but a large framework; writing a small, purpose-built simulator was the point.

## Architecture

```
                    +--------------------------------------------+
                    |  EventQueue: min-heap on (time_ps, seq)    |
                    +--------------------------------------------+
                       | TxDone | Arrive | NicWake | Timer | FlowStart | Sample
          +------------+--------+---------------------------------------+
          v                                                             v
  Host NIC (sim.rs)                                              Switch (sim.rs)
  - flows: snd_nxt/snd_una, pacing slot,         data ->  - ECMP pick (routing.rs)
    window, CC state (cc/*)                               - Mmu: admit, PFC pause/resume (mmu.rs)
  - round robin over eligible flows                       - egress queue per port (control + data)
  - receiver: in-order check, ACK/NACK      <- ACK/PFC    - at dequeue: ECN mark, INT stamp, resume check
          |                                                             |
          +---------------- Port: serialize (size/rate) ----------------+
                            then propagate (delay)
```

* `time.rs` - picosecond integer time. Every serialization delay of the paper's rates is an exact integer
  (100 Gbps = 80 ps per byte).
* `event.rs` - binary heap ordered by `(time, sequence number)`. The sequence number makes simultaneous events
  run in the order they were scheduled, which is what makes runs reproducible.
* `packet.rs` - packets live in a slab and are passed around by index; a data packet is turned into its own ACK
  in place at the receiver, so the INT stack is never copied.
* `sim.rs` - ports, hosts, switches and the event handlers. Each hop costs two events (`TxDone` at the sender side,
  `Arrive` at the receiver side).
* `mmu.rs` - the shared-buffer switch memory with PFC, as a standalone, unit-tested state machine.
* `cc/` - one file per algorithm. Each is a pure state machine: ACKs and timers in, `{rate, window}` out. They can
  be (and are) unit-tested without a network.
* `routing.rs` - BFS shortest paths and per-flow ECMP hashing.
* `workload.rs` - size distributions, Poisson arrivals, incast.
* `experiments.rs` - the paper's scenarios and the CSV/JSON output; `bin/hpcc-sim.rs` is the CLI.
* `analysis/` - Python: the experiment matrix (standard library only) and the figures/page (matplotlib).

## The hard problems

### 1. Being faithful to the paper where it is precise, and honest where it is not

Algorithm 1 is implemented line by line in `src/cc/hpcc.rs`, with the line numbers in comments. Where the paper is
silent, the choice is taken from the authors' public ns-3 code and written down (table below). The paper's
evaluation parameters are used as stated: eta = 95%, maxStage = 5, W_AI = 80 B, T = 13 us, 32 MB buffers, DCQCN
ECN thresholds 100/400 KB x Bw/25G, DCTCP K = 30 KB x Bw/10G, PFC at about 11% of free buffer, INT overhead of
42 bytes.

| Detail | Paper | Authors' ns-3 code | This simulator |
|---|---|---|---|
| HPCC maxStage | 5 (§5.1) | `MI_THRESH` default 0 | 5; a sensitivity run uses 0 |
| HPCC first ACK of a flow | not stated | records INT, starts the first RTT | same |
| HPCC initial U | not stated | 1 | 1 |
| HPCC rate bounds | not stated | [1 Gbps, line rate] | same, applied to W (W = R T) |
| DCQCN parameters | "vendor" values; Ti = 300 us, Td = 4 us (§2.3) | alpha timer 1 us, g = 1/256, F = 1, AI 20 / HAI 200 Mbps at 100G, min rate 1 Gbps | same |
| CNP generation | DCQCN paper: NP rate-limits CNPs | ECN echoed on every ACK, RP decreases at most every Td | same |
| DCTCP | no slow start (§5.1) | rate-based with a window, +1 MSS per RTT | window (cwnd) paced at cwnd/T, +1 MSS per RTT |
| TIMELY | parameters of [31] | updates once per RTT | same |
| PFC threshold | 11% of free buffer | dynamic threshold, alpha = 1/8 at NIC speed, x2 per doubling of port speed | same |
| PFC headroom | not stated | 3 x rate x link delay per port | same |
| ACK queue in switches | not stated | data queue for every CC (`ACK_HIGH_PRIO` reaches only the host NICs, see §7) | data queue (`AckClass::SharedWithData`); a high-priority class is a sensitivity option |
| ACK at the host NIC | not stated | sent before data; paused by PFC only if `ACK_HIGH_PRIO` = 0 (HPCC, DCTCP) | same |
| Data packet | not stated | 1000 B payload, 48 B headers | same; +42 B INT for HPCC |
| ACK | not stated | 60 B (+INT) | 60 B (+42 B INT for HPCC) |
| Slowdown baseline | "FCT when the network only has this flow" | base RTT + bytes / bandwidth | exact store-and-forward FCT of a lone flow (below) |

### 2. Exact timing, and a baseline that is exactly right

FCT slowdown divides by the "ideal" FCT, so an error in the ideal skews every figure. The ideal here is computed by
an exact recurrence over every packet and every hop of the flow's real ECMP path (including the partial last packet
and the ACK of the last packet on the reverse path). A test runs lone line-rate flows of awkward sizes (1 B,
1001 B, 3 000 001 B, ...) on four topologies with mixed link rates and requires the simulated FCT to equal the
ideal **to the picosecond**. Another test checks a single packet's end-to-end latency against the hand-computed
value (4 us of propagation + 2 x 3.92 ns + 2 x 4.8 ns).

### 3. PFC that never drops and never leaks

The switch MMU keeps per-port ingress, headroom and egress counters and a shared-pool counter. The accounting rules
follow the authors' `SwitchMmu`, but here they are a separate module with an explicit invariant check (ingress +
headroom = egress = bytes in queues; shared usage = sum of per-port usage above the reserve; headroom never above
its limit). A randomised test drives the MMU with 600 000 random enqueue/dequeue operations over 30 seeds and checks
the invariants after every step. Integration tests check that line-rate incasts with PFC on never drop a packet
(including PFC propagating hop by hop through three switches), that every PAUSE is eventually resumed, and that the
same incast with PFC off does drop and is recovered by go-back-N.

### 4. Measuring FCT without end effects

If traffic stops arriving when measurement stops, the last flows finish in an emptying network and look too fast.
Here flows keep arriving for the whole run; only flows that *start* in the measurement window are counted, and the
run stops when all of them have finished (or at a hard limit). Flows still unfinished at the hard limit are not
dropped from the statistics: they are counted with their elapsed time as a lower bound on their FCT, and their number
is reported on the results page. Queue, RTT and pause statistics are collected only after the warm-up.

### 5. Determinism, on every platform

Identical seeds must give identical output, and the golden digests pinned on macOS/arm64 must also hold on the
Linux/x86-64 CI runner. The argument, by construction:

* **Only exactly-rounded arithmetic.** IEEE-754 `+ - * /`, comparisons, `ceil`, `min`/`max` and integer<->float
  conversions are specified bit-exactly and give the same result on every conforming CPU. x86-64 uses SSE2 (no x87
  extended precision), and Rust never contracts `a*b+c` into a fused multiply-add unless `mul_add` is called.
* **No libm.** Transcendental functions (`ln`, `exp`, `pow`, ...) are not correctly rounded and differ between
  macOS's libm and glibc in the last bit. The only one the model needs, the logarithm for exponential inter-arrival
  times, is `rng::det_ln`, built from `+ - * /` and bit manipulation.
* **No unordered iteration.** No hash maps or hash sets; all sorts are stable; simultaneous events run in
  scheduling order (FIFO tie-break), never in heap-layout order.
* **Own random numbers.** xoshiro256** (checked against the reference implementation's published outputs), with
  separate streams per purpose (workload, incast, each switch's ECN coin flips, ECMP seeds).
* **Wall-clock time** is only used to report how long a run took, never inside the model.

Enforcement and evidence:

* `tests/portability.rs` scans the simulator sources and fails if a libm transcendental call, `mul_add`, a hash map
  or an unstable sort appears in non-test code, and pins the exact bit patterns of `det_ln` for five inputs.
* `golden_digest` hashes the FCTs and counters of four small runs (HPCC, DCQCN, DCTCP, TIMELY) and compares them with
  constants recorded on arm64.
* The same tests were built for `x86_64-apple-darwin` and run under Rosetta 2 on the development machine: identical
  digests and identical `det_ln` bits. A complete FatTree run (`fct.csv`, queue and RTT histograms, summary) was also
  byte-identical between the arm64 and x86-64 binaries. Linux itself was not available locally; since the model
  calls no C library math, the only platform-dependent part (instruction selection and float codegen) is what the
  x86-64 run exercises, and CI recomputes the digests on Linux.

### 6. Speed

* Two events per packet per hop; no per-byte or per-tick events.
* Flows are created lazily: one pending `FlowStart` event, and CC state is boxed and allocated only when a flow
  starts (most of the hundreds of thousands of generated flows of a run are small).
* Timer cancellation uses generation counters instead of searching the heap.
* Serialization uses a precomputed picoseconds-per-byte factor when the rate allows it.

The result is about 13 M events/s single-threaded (`make bench`), with peak memory of about 130 MB for a full-scale
run.

### 7. Matching the authors' simulator, mechanism by mechanism

The first cross-check against the authors' ns-3 simulator (identical topology and flows, see the README) agreed
for HPCC but not for DCQCN: their 95th-percentile slowdowns of short flows were 1.2-1.9x higher. The DCQCN
implementations were then compared item by item:

| Mechanism | Authors' simulator | This simulator |
|---|---|---|
| Rate increase | 300 us timer only (no byte counter), F = 1, AI 20 / HAI 200 Mbps | same |
| Alpha | 1 us timer, g = 1/256, alpha = 1 at the first CNP | same |
| CNP | ECN echo on every ACK of a marked packet, per QP; at most one decrease per Td = 4 us | same |
| Rates | start at line rate; min 1 Gbps; rate on first CNP x1.0; target not clamped in fast recovery | same |
| ECN | RED at dequeue on the port's data-queue bytes; 400/1600 KB at 100G, 1600/6400 KB at 400G, Pmax 0.2 | same |
| PFC and buffer | 32 MB; alpha 1/8 (100G), 1/2 (400G); headroom 3 x rate x delay; 4 KB reserve; 3 KB resume offset | same |
| NIC | round robin over QPs; next send = now + size / rate | same |
| Pacing after a rate change | DCQCN timers assign the rate without re-timing the pending send | the pending send is re-timed (as their HPCC path does) |
| Time resolution | integer nanoseconds; a lone 1 MB flow at 100G finishes in 87.09 us instead of the exact 87.93 us (about 83 ns per 1048-byte packet instead of 83.84 ns), so long transfers run ~1% fast | integer picoseconds, exact |
| **ACKs in switches** | **data queue, for every CC** | **was: high-priority class** |

Every item but the last three rows is equivalent, and simple scenarios matched closely: two- and eight-sender
incasts (FCTs within 6%), a single elephant with single-packet probes on the sender, receiver or ACK path (probe
latencies within 2%), and two elephants with mice (mice latency tails within 3%). On a 16-host star with FB_Hadoop traffic the gap
reappeared even though the switches' time-averaged queue lengths were the same in both simulators (within 1.2% in
total, per port within run-to-run variation). So the extra delay was not in the forward queues. A packet trace of
their simulator (`ENABLE_TRACE 1`, their `trace_reader`) then showed ACKs enqueued in the *data* queue (index 3) at
switches, behind 1.1 MB of data, even with `ACK_HIGH_PRIO 1`. The cause is in `scratch/third.cc`: the config value
only sets `RdmaEgressQueue::ack_q_idx` on hosts, while `SwitchNode`'s `AckHighPrio` attribute (default 0, checked in
`SwitchNode::SendToDev`) is never set. Their `config_doc.txt` describes the knob as "1: prioritize ACK", so the
switch side looks like an unwired setting rather than a deliberate model, but it is the behaviour that produced their
results. With ACKs queued behind data the FCT of a short flow includes the reverse path's queue, and DCQCN's
CNP-carrying ACKs return later, which is exactly a DCQCN-sized effect (big queues) and an HPCC-sized non-effect
(near-zero queues).

The ACK class is now a model option (`sim::AckClass`) that defaults to the authors' effective behaviour, because
the goal is to reproduce the paper's evaluation; the old model is kept as `--acks-high-priority` and run as a
sensitivity case in `make repro`. Making ACKs droppable (when PFC is off) also exposed two liveness bugs in the
go-back-N path - duplicates were not re-acknowledged, and a late ACK after a rewind could leave `snd_nxt` behind
`snd_una` - which are fixed and covered by a regression test and two mutants.

## Trade-offs that were rejected

* **A calendar queue** instead of a binary heap: faster in theory, but the heap is not the bottleneck (cache misses on
  packet and port state are), and a heap is trivially correct.
* **Flow-level or fluid simulation**: orders of magnitude faster, but HPCC's behaviour depends on per-packet INT
  feedback and on per-ACK reaction, which a fluid model would have to assume rather than reproduce.
* **Copying the authors' code structure**: it would have made a numeric match more likely and the match less
  meaningful. Only the configuration values and the documented bookkeeping rules were taken from it.
* **Multithreading one simulation**: conservative parallel DES on a 56-switch network with 1 us link delays gives
  little lookahead; running 4 independent simulations in parallel is simpler and fully deterministic.
* **Using `f64` seconds for time**: rounding would make "lone flow FCT equals ideal exactly" untestable.
