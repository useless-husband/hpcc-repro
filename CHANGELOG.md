# Changelog

## 0.1.0 - 2026-10-04

First complete version.

- Deterministic discrete-event simulator in Rust: picosecond integer time,
  FIFO tie-breaking, seeded streams, platform-independent arithmetic.
- Network model: RDMA-style NICs with per-flow pacing and windows,
  shared-buffer switches with dynamic PFC thresholds and headroom,
  RED-style ECN, INT stamping, ECMP, go-back-N when PFC is off.
- Senders: HPCC (Algorithm 1, plus the per-ACK and per-RTT variants of
  Figure 13), DCQCN and DCQCN+win, DCTCP, TIMELY and TIMELY+win.
- Workloads: WebSearch and FB_Hadoop size distributions, Poisson arrivals,
  60-to-1 incast at 2% of capacity; the paper's 320-host FatTree and a
  32-host scaled-down tree.
- Reproductions of Figures 9, 10, 11, 13 and 14; validation and mutation
  tests; results page and reproduction report.
