# Changelog

## 0.2.0 - 2026-10-05

- ACKs now wait in the data queue at switches by default, matching the effective behaviour of the authors' ns-3
  simulator (its `ACK_HIGH_PRIO` setting only reaches host NICs). This was the cause of the DCQCN gap in the first
  cross-check; HPCC, DCQCN, DCTCP and TIMELY now agree with the authors' simulator within about seed-to-seed
  variation. The previous model is available as `--acks-high-priority` and runs as a sensitivity case.
- Fixed two go-back-N liveness bugs that dropped ACKs exposed: duplicates are re-acknowledged, and a late ACK after
  a rewind no longer leaves `snd_nxt` behind `snd_una`.
- `hpcc-sim ns3` runs scenarios given in the authors' input format; cross-check configs for all four algorithms.
- Portability: a test forbids platform-dependent float operations, hash maps and unstable sorts in the model;
  golden digests verified identical on arm64 and x86-64.
- All results, figures and documents regenerated.

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
