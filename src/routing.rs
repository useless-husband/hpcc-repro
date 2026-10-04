//! Shortest-path routing with ECMP.
//!
//! Every switch keeps, for every destination host, the set of egress ports
//! that lie on a shortest path. A packet picks one of them by hashing its
//! flow 5-tuple with a per-switch seed (as the authors' ns-3 code does), so
//! all packets of a flow follow the same path and are never reordered.

use crate::rng::mix64;
use crate::topology::Topology;
use std::collections::VecDeque;

/// Port numbering: each link adds one port to both of its end nodes, in the
/// order the links are listed in the topology.
#[derive(Clone, Debug)]
pub struct PortMap {
    /// `ports[node][port] = (peer node, peer's port number, link index)`
    pub ports: Vec<Vec<(u32, u16, u32)>>,
}

impl PortMap {
    pub fn new(t: &Topology) -> Self {
        let mut ports: Vec<Vec<(u32, u16, u32)>> = vec![Vec::new(); t.n_nodes() as usize];
        for (i, l) in t.links.iter().enumerate() {
            let pa = ports[l.a as usize].len() as u16;
            let pb = ports[l.b as usize].len() as u16;
            ports[l.a as usize].push((l.b, pb, i as u32));
            ports[l.b as usize].push((l.a, pa, i as u32));
        }
        PortMap { ports }
    }
}

pub struct Routes {
    n_hosts: u32,
    /// For switch `s` (index `node - n_hosts`) and destination host `h`:
    /// `hops[(s * n_hosts + h)]` is a range into `ports`.
    ranges: Vec<(u32, u16)>,
    ports: Vec<u16>,
    /// Hop distance (in links) from every node to every host.
    dist: Vec<Vec<u32>>,
}

impl Routes {
    pub fn new(t: &Topology, pm: &PortMap) -> Self {
        let n = t.n_nodes() as usize;
        let nh = t.n_hosts as usize;
        // BFS from each destination host. Hosts never forward traffic, so
        // they are not expanded (except the source of the BFS itself).
        let mut dist = vec![vec![u32::MAX; n]; nh];
        for h in 0..nh {
            let d = &mut dist[h];
            d[h] = 0;
            let mut q = VecDeque::from([h as u32]);
            while let Some(u) = q.pop_front() {
                if t.is_host(u) && u as usize != h {
                    continue;
                }
                for &(v, _, _) in &pm.ports[u as usize] {
                    if d[v as usize] == u32::MAX {
                        d[v as usize] = d[u as usize] + 1;
                        q.push_back(v);
                    }
                }
            }
        }
        let mut ranges = Vec::with_capacity(t.n_switches as usize * nh);
        let mut ports = Vec::new();
        for s in t.n_hosts..t.n_nodes() {
            for (h, dh) in dist.iter().enumerate() {
                let start = ports.len() as u32;
                let ds = dh[s as usize];
                for (pi, &(v, _, _)) in pm.ports[s as usize].iter().enumerate() {
                    let dv = dh[v as usize];
                    let usable =
                        (!t.is_host(v) || v as usize == h) && dv != u32::MAX && dv + 1 == ds;
                    if usable {
                        ports.push(pi as u16);
                    }
                }
                ranges.push((start, (ports.len() as u32 - start) as u16));
            }
        }
        Routes {
            n_hosts: t.n_hosts,
            ranges,
            ports,
            dist,
        }
    }

    /// Equal-cost egress ports at switch `node` towards host `dst`.
    #[inline]
    pub fn next_hops(&self, node: u32, dst: u32) -> &[u16] {
        let (s, len) = self.ranges[((node - self.n_hosts) * self.n_hosts + dst) as usize];
        &self.ports[s as usize..s as usize + len as usize]
    }

    /// ECMP choice: hash of the packet's 5-tuple hash and the switch's seed.
    #[inline]
    pub fn pick(&self, node: u32, dst: u32, pkt_hash: u64, switch_seed: u64) -> u16 {
        let nh = self.next_hops(node, dst);
        assert!(!nh.is_empty(), "no route from node {node} to host {dst}");
        if nh.len() == 1 {
            return nh[0];
        }
        nh[(mix64(pkt_hash ^ switch_seed) % nh.len() as u64) as usize]
    }

    pub fn distance(&self, from: u32, to_host: u32) -> u32 {
        self.dist[to_host as usize][from as usize]
    }
}

/// Hash of a flow's 5-tuple. Data packets use (src, dst, sport, dport);
/// acknowledgements travel the reverse direction, so they hash the reversed
/// tuple, exactly like a real switch would.
pub fn flow_hash(src: u32, dst: u32, sport: u32, dport: u32) -> u64 {
    let a = ((src as u64) << 32) | dst as u64;
    let b = ((sport as u64) << 32) | dport as u64;
    mix64(mix64(a) ^ b.rotate_left(17))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{gbps, US};
    use crate::topology::{chain, fat_tree, star, FatTreeParams};

    #[test]
    fn fat_tree_distances_and_path_diversity() {
        let t = fat_tree(&FatTreeParams::paper());
        let pm = PortMap::new(&t);
        let r = Routes::new(&t, &pm);
        // Same rack: 2 links, same pod: 4, cross pod: 6.
        assert_eq!(r.distance(0, 1), 2);
        assert_eq!(r.distance(0, 16), 4);
        assert_eq!(r.distance(0, 64), 6);
        let tor0 = 320;
        // A ToR has 4 equal-cost uplinks towards another pod and 1 downlink to its own host.
        assert_eq!(r.next_hops(tor0, 64).len(), 4);
        assert_eq!(r.next_hops(tor0, 3).len(), 1);
        // An Agg switch towards another pod: 4 core uplinks.
        assert_eq!(r.next_hops(340, 64).len(), 4);
        // A core switch towards a host: exactly one Agg in the host's pod.
        assert_eq!(r.next_hops(360, 64).len(), 1);
    }

    #[test]
    fn ecmp_is_deterministic_and_spreads() {
        let t = fat_tree(&FatTreeParams::paper());
        let pm = PortMap::new(&t);
        let r = Routes::new(&t, &pm);
        let mut counts = [0u32; 4];
        for f in 0..40_000u32 {
            let h = flow_hash(0, 64, f, 100);
            let p1 = r.pick(320, 64, h, 12345);
            let p2 = r.pick(320, 64, h, 12345);
            assert_eq!(p1, p2, "same flow, same switch, same port");
            let idx = r.next_hops(320, 64).iter().position(|&p| p == p1).unwrap();
            counts[idx] += 1;
        }
        for c in counts {
            assert!(
                (9_400..10_600).contains(&c),
                "uneven ECMP spread {counts:?}"
            );
        }
    }

    #[test]
    fn ecmp_hash_uses_all_tuple_fields() {
        let base = flow_hash(1, 2, 3, 4);
        assert_ne!(base, flow_hash(2, 2, 3, 4));
        assert_ne!(base, flow_hash(1, 3, 3, 4));
        assert_ne!(base, flow_hash(1, 2, 4, 4));
        assert_ne!(base, flow_hash(1, 2, 3, 5));
        assert_ne!(base, flow_hash(2, 1, 4, 3), "direction matters");
    }

    #[test]
    fn star_and_chain_routes() {
        let t = star(4, gbps(100), US);
        let pm = PortMap::new(&t);
        let r = Routes::new(&t, &pm);
        for h in 0..4 {
            assert_eq!(r.next_hops(4, h), &[h as u16]);
        }
        let t = chain(3, 2, gbps(100), gbps(100), US);
        let pm = PortMap::new(&t);
        let r = Routes::new(&t, &pm);
        assert_eq!(r.distance(0, 5), 4);
        // Hosts are never used as transit.
        for s in 6..9 {
            for h in 0..6 {
                for &p in r.next_hops(s, h) {
                    let (peer, _, _) = pm.ports[s as usize][p as usize];
                    assert!(peer >= 6 || peer == h);
                }
            }
        }
    }
}
