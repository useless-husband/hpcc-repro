//! A deterministic packet-level datacenter network simulator built to
//! reproduce the evaluation of "HPCC: High Precision Congestion Control"
//! (Li et al., SIGCOMM 2019). See README.md and docs/DESIGN.md.

pub mod cc;
pub mod event;
pub mod experiments;
pub mod mmu;
pub mod packet;
pub mod rng;
pub mod routing;
pub mod sim;
pub mod stats;
pub mod time;
pub mod topology;
pub mod workload;
