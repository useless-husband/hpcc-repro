//! Packets and the packet pool.
//!
//! Packets live in a slab (`PacketPool`) and are referred to by a `u32`
//! index everywhere else (queues, events), so moving a packet from a queue to
//! a wire never copies its INT stack.

use crate::time::Time;

/// The HPCC paper's INT header holds at most 5 hops (Fig. 7: "42 bytes for 5
/// hops"); the longest path in the FatTree crosses exactly 5 switches.
pub const MAX_HOPS: usize = 5;

/// One switch's INT record, pushed when the packet leaves the egress port
/// (HPCC paper, §3.1 and Fig. 7): timestamp, cumulative bytes transmitted on
/// the port, current queue length and the port's line rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntHop {
    pub ts: Time,
    pub tx_bytes: u64,
    pub qlen: u64,
    pub rate_bps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Data,
    Ack,
    /// Go-back-N negative acknowledgement (only used when PFC is disabled).
    Nack,
    /// PFC XOFF for the data priority.
    Pause,
    /// PFC XON for the data priority.
    Resume,
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub kind: Kind,
    /// Data: congestion experienced (CE) mark. Ack: ECN echo.
    pub ecn: bool,
    pub n_hops: u8,
    /// Ingress port at the switch currently holding the packet (for the
    /// buffer accounting of the PFC ingress queue).
    pub in_port: u16,
    pub flow: u32,
    pub src: u32,
    pub dst: u32,
    /// Bytes on the wire.
    pub size: u32,
    /// Application payload carried (data packets only).
    pub payload: u32,
    /// Data: offset of the first payload byte. Ack/Nack: next expected byte.
    pub seq: u64,
    /// Data: time the sender put the packet on the wire. Ack: echoed.
    pub ts: Time,
    /// ECMP hash of the packet's 5-tuple (precomputed at the sender).
    pub hash: u64,
    pub hops: [IntHop; MAX_HOPS],
}

impl Packet {
    pub fn control(kind: Kind, size: u32) -> Packet {
        Packet {
            kind,
            ecn: false,
            n_hops: 0,
            in_port: 0,
            flow: u32::MAX,
            src: 0,
            dst: 0,
            size,
            payload: 0,
            seq: 0,
            ts: 0,
            hash: 0,
            hops: [IntHop::default(); MAX_HOPS],
        }
    }

    pub fn int_hops(&self) -> &[IntHop] {
        &self.hops[..self.n_hops as usize]
    }

    pub fn is_control_priority(&self) -> bool {
        !matches!(self.kind, Kind::Data)
    }
}

/// Slab allocator for packets.
#[derive(Default)]
pub struct PacketPool {
    slots: Vec<Packet>,
    live: Vec<bool>,
    free: Vec<u32>,
    n_live: usize,
}

impl PacketPool {
    pub fn alloc(&mut self, p: Packet) -> u32 {
        self.n_live += 1;
        if let Some(i) = self.free.pop() {
            self.slots[i as usize] = p;
            self.live[i as usize] = true;
            i
        } else {
            self.slots.push(p);
            self.live.push(true);
            (self.slots.len() - 1) as u32
        }
    }

    #[inline]
    pub fn get(&self, i: u32) -> &Packet {
        debug_assert!(self.live[i as usize], "use of freed packet {i}");
        &self.slots[i as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, i: u32) -> &mut Packet {
        debug_assert!(self.live[i as usize], "use of freed packet {i}");
        &mut self.slots[i as usize]
    }

    pub fn free(&mut self, i: u32) {
        assert!(self.live[i as usize], "double free of packet {i}");
        self.live[i as usize] = false;
        self.free.push(i);
        self.n_live -= 1;
    }

    pub fn live_count(&self) -> usize {
        self.n_live
    }

    /// Iterates over packets currently alive (in queues or on wires).
    pub fn live_packets(&self) -> impl Iterator<Item = &Packet> {
        self.slots.iter().zip(&self.live).filter(|(_, l)| **l).map(|(p, _)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_reuses_slots_and_counts() {
        let mut pool = PacketPool::default();
        let a = pool.alloc(Packet::control(Kind::Ack, 60));
        let b = pool.alloc(Packet::control(Kind::Pause, 64));
        assert_eq!(pool.live_count(), 2);
        pool.free(a);
        assert_eq!(pool.live_count(), 1);
        let c = pool.alloc(Packet::control(Kind::Resume, 64));
        assert_eq!(c, a, "freed slot is reused");
        assert_eq!(pool.get(c).kind, Kind::Resume);
        assert_eq!(pool.live_packets().count(), 2);
        pool.free(b);
        pool.free(c);
        assert_eq!(pool.live_count(), 0);
    }

    #[test]
    #[should_panic(expected = "double free")]
    fn double_free_panics() {
        let mut pool = PacketPool::default();
        let a = pool.alloc(Packet::control(Kind::Ack, 60));
        pool.free(a);
        pool.free(a);
    }
}
