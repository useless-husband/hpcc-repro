//! Topologies. Nodes `0..n_hosts` are hosts (one NIC each); nodes
//! `n_hosts..n_hosts+n_switches` are switches.

use crate::time::{gbps, Time, US};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub a: u32,
    pub b: u32,
    pub rate_bps: u64,
    pub delay: Time,
}

#[derive(Clone, Debug)]
pub struct Topology {
    pub name: String,
    pub n_hosts: u32,
    pub n_switches: u32,
    pub links: Vec<Link>,
}

impl Topology {
    pub fn n_nodes(&self) -> u32 {
        self.n_hosts + self.n_switches
    }

    pub fn is_host(&self, n: u32) -> bool {
        n < self.n_hosts
    }

    /// Checks structural assumptions the simulator relies on.
    pub fn validate(&self) -> Result<(), String> {
        let mut host_links = vec![0u32; self.n_hosts as usize];
        for l in &self.links {
            if l.a >= self.n_nodes() || l.b >= self.n_nodes() || l.a == l.b {
                return Err(format!("bad link {l:?}"));
            }
            if l.rate_bps == 0 {
                return Err(format!("zero-rate link {l:?}"));
            }
            for n in [l.a, l.b] {
                if self.is_host(n) {
                    host_links[n as usize] += 1;
                }
            }
            if self.is_host(l.a) && self.is_host(l.b) {
                return Err("host-to-host links are not supported".into());
            }
        }
        if let Some(h) = host_links.iter().position(|&c| c != 1) {
            return Err(format!("host {h} must have exactly one link"));
        }
        Ok(())
    }
}

/// Parameters of the paper's FatTree (§5.1): pods of `tors_per_pod` ToRs and
/// `aggs_per_pod` Agg switches, fully meshed inside the pod; Agg switch `k`
/// of every pod connects to `cores_per_agg` core switches
/// `k*cores_per_agg .. (k+1)*cores_per_agg`.
#[derive(Clone, Debug)]
pub struct FatTreeParams {
    pub pods: u32,
    pub tors_per_pod: u32,
    pub aggs_per_pod: u32,
    pub cores_per_agg: u32,
    pub hosts_per_tor: u32,
    pub host_rate_bps: u64,
    pub fabric_rate_bps: u64,
    pub delay: Time,
}

impl FatTreeParams {
    /// The paper's simulation topology: 16 core, 20 Agg, 20 ToR switches and
    /// 320 hosts; 100 Gbps host links, 400 Gbps fabric links, 1 us per link.
    pub fn paper() -> Self {
        FatTreeParams {
            pods: 5,
            tors_per_pod: 4,
            aggs_per_pod: 4,
            cores_per_agg: 4,
            hosts_per_tor: 16,
            host_rate_bps: gbps(100),
            fabric_rate_bps: gbps(400),
            delay: US,
        }
    }

    /// A scaled-down tree with the same link rates, delays, path lengths and
    /// (non-blocking) oversubscription ratio: 2 pods x (2 ToR + 2 Agg),
    /// 4 cores, 8 hosts per ToR = 32 hosts. Per ToR, 8 x 100G down and
    /// 2 x 400G up, exactly like the paper's 16 x 100G down / 4 x 400G up.
    pub fn small() -> Self {
        FatTreeParams {
            pods: 2,
            tors_per_pod: 2,
            aggs_per_pod: 2,
            cores_per_agg: 2,
            hosts_per_tor: 8,
            host_rate_bps: gbps(100),
            fabric_rate_bps: gbps(400),
            delay: US,
        }
    }

    pub fn n_hosts(&self) -> u32 {
        self.pods * self.tors_per_pod * self.hosts_per_tor
    }
}

pub fn fat_tree(p: &FatTreeParams) -> Topology {
    let n_hosts = p.n_hosts();
    let n_tor = p.pods * p.tors_per_pod;
    let n_agg = p.pods * p.aggs_per_pod;
    let n_core = p.aggs_per_pod * p.cores_per_agg;
    let tor0 = n_hosts;
    let agg0 = tor0 + n_tor;
    let core0 = agg0 + n_agg;
    let mut links = Vec::new();
    for h in 0..n_hosts {
        links.push(Link { a: h, b: tor0 + h / p.hosts_per_tor, rate_bps: p.host_rate_bps, delay: p.delay });
    }
    for pod in 0..p.pods {
        for t in 0..p.tors_per_pod {
            for a in 0..p.aggs_per_pod {
                links.push(Link {
                    a: tor0 + pod * p.tors_per_pod + t,
                    b: agg0 + pod * p.aggs_per_pod + a,
                    rate_bps: p.fabric_rate_bps,
                    delay: p.delay,
                });
            }
        }
    }
    for pod in 0..p.pods {
        for a in 0..p.aggs_per_pod {
            for c in 0..p.cores_per_agg {
                links.push(Link {
                    a: agg0 + pod * p.aggs_per_pod + a,
                    b: core0 + a * p.cores_per_agg + c,
                    rate_bps: p.fabric_rate_bps,
                    delay: p.delay,
                });
            }
        }
    }
    Topology {
        name: format!("fattree-{}h", n_hosts),
        n_hosts,
        n_switches: n_tor + n_agg + n_core,
        links,
    }
}

/// `n` hosts on one switch (the paper's §5.4 micro-benchmark topology).
pub fn star(n: u32, rate_bps: u64, delay: Time) -> Topology {
    let links = (0..n).map(|h| Link { a: h, b: n, rate_bps, delay }).collect();
    Topology { name: format!("star-{n}"), n_hosts: n, n_switches: 1, links }
}

/// A chain of `k` switches with `hosts_per_switch` hosts on each switch.
/// Used to test multi-hop timing and multi-bottleneck behaviour.
pub fn chain(k: u32, hosts_per_switch: u32, host_rate: u64, core_rate: u64, delay: Time) -> Topology {
    let n_hosts = k * hosts_per_switch;
    let mut links = Vec::new();
    for h in 0..n_hosts {
        links.push(Link { a: h, b: n_hosts + h / hosts_per_switch, rate_bps: host_rate, delay });
    }
    for s in 0..k.saturating_sub(1) {
        links.push(Link { a: n_hosts + s, b: n_hosts + s + 1, rate_bps: core_rate, delay });
    }
    Topology { name: format!("chain-{k}"), n_hosts, n_switches: k, links }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_fat_tree_shape() {
        let t = fat_tree(&FatTreeParams::paper());
        t.validate().unwrap();
        assert_eq!(t.n_hosts, 320);
        assert_eq!(t.n_switches, 16 + 20 + 20);
        let n100 = t.links.iter().filter(|l| l.rate_bps == gbps(100)).count();
        let n400 = t.links.iter().filter(|l| l.rate_bps == gbps(400)).count();
        assert_eq!((n100, n400), (320, 160));
        // Every core switch connects to one Agg in each of the 5 pods.
        let core0 = 320 + 40;
        for c in core0..core0 + 16 {
            assert_eq!(t.links.iter().filter(|l| l.b == c).count(), 5);
        }
        // Non-blocking: per ToR, 16 x 100G down and 4 x 400G up.
        let tor = 320;
        let up: u64 = t.links.iter().filter(|l| l.a == tor).map(|l| l.rate_bps).sum();
        let down: u64 = t.links.iter().filter(|l| l.b == tor).map(|l| l.rate_bps).sum();
        assert_eq!(up, down);
    }

    #[test]
    fn small_fat_tree_is_non_blocking() {
        let t = fat_tree(&FatTreeParams::small());
        t.validate().unwrap();
        assert_eq!(t.n_hosts, 32);
        let tor = 32;
        let up: u64 = t.links.iter().filter(|l| l.a == tor).map(|l| l.rate_bps).sum();
        let down: u64 = t.links.iter().filter(|l| l.b == tor).map(|l| l.rate_bps).sum();
        assert_eq!(up, down);
    }

    #[test]
    fn validate_rejects_bad_hosts() {
        let mut t = star(2, gbps(100), US);
        t.links.push(Link { a: 0, b: 2, rate_bps: 1, delay: 0 });
        assert!(t.validate().is_err());
    }
}
