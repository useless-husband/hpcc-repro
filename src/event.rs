//! The discrete-event queue.
//!
//! Events are ordered by time; events scheduled for the same picosecond run
//! in the order they were scheduled (FIFO). The tie-break is what makes runs
//! reproducible: a binary heap alone does not preserve insertion order for
//! equal keys, so every entry carries a monotonically increasing sequence
//! number and the heap orders on `(time, seq)`.

use crate::time::Time;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

struct Entry<E> {
    time: Time,
    seq: u64,
    ev: E,
}

impl<E> PartialEq for Entry<E> {
    fn eq(&self, o: &Self) -> bool {
        self.time == o.time && self.seq == o.seq
    }
}
impl<E> Eq for Entry<E> {}
impl<E> PartialOrd for Entry<E> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl<E> Ord for Entry<E> {
    // BinaryHeap is a max-heap; reverse so the earliest (time, seq) is on top.
    fn cmp(&self, o: &Self) -> Ordering {
        o.time.cmp(&self.time).then_with(|| o.seq.cmp(&self.seq))
    }
}

pub struct EventQueue<E> {
    heap: BinaryHeap<Entry<E>>,
    seq: u64,
    now: Time,
    popped: u64,
}

impl<E> Default for EventQueue<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> EventQueue<E> {
    pub fn new() -> Self {
        EventQueue {
            heap: BinaryHeap::with_capacity(1 << 16),
            seq: 0,
            now: 0,
            popped: 0,
        }
    }

    /// Current simulated time (time of the last popped event).
    #[inline]
    pub fn now(&self) -> Time {
        self.now
    }

    /// Schedules `ev` at absolute time `t`. Scheduling into the past is a
    /// simulator bug, so it panics.
    #[inline]
    pub fn schedule(&mut self, t: Time, ev: E) {
        assert!(
            t >= self.now,
            "event scheduled in the past: {t} < {}",
            self.now
        );
        self.heap.push(Entry {
            time: t,
            seq: self.seq,
            ev,
        });
        self.seq += 1;
    }

    /// Removes the earliest event and advances the clock to its time.
    #[inline]
    pub fn pop(&mut self) -> Option<(Time, E)> {
        let e = self.heap.pop()?;
        debug_assert!(e.time >= self.now);
        self.now = e.time;
        self.popped += 1;
        Some((e.time, e.ev))
    }

    pub fn peek_time(&self) -> Option<Time> {
        self.heap.peek().map(|e| e.time)
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Number of events processed so far.
    pub fn processed(&self) -> u64 {
        self.popped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    #[test]
    fn pops_in_time_order() {
        let mut q = EventQueue::new();
        q.schedule(30, 'c');
        q.schedule(10, 'a');
        q.schedule(20, 'b');
        assert_eq!(q.pop(), Some((10, 'a')));
        assert_eq!(q.now(), 10);
        assert_eq!(q.pop(), Some((20, 'b')));
        assert_eq!(q.pop(), Some((30, 'c')));
        assert_eq!(q.pop(), None);
    }

    #[test]
    fn equal_times_are_fifo() {
        let mut q = EventQueue::new();
        for i in 0..1000u32 {
            q.schedule(5, i);
        }
        for i in 0..1000u32 {
            assert_eq!(q.pop(), Some((5, i)));
        }
    }

    #[test]
    #[should_panic(expected = "in the past")]
    fn scheduling_in_the_past_panics() {
        let mut q = EventQueue::new();
        q.schedule(10, ());
        q.pop();
        q.schedule(9, ());
    }

    #[test]
    fn randomised_against_stable_sort() {
        // Interleave pushes and pops; compare with a reference model that
        // keeps a list and picks the minimum (time, insertion index).
        for seed in 0..20u64 {
            let mut rng = Rng::new(seed);
            let mut q = EventQueue::new();
            let mut model: Vec<(Time, u64)> = Vec::new();
            let mut next_id = 0u64;
            for _ in 0..5000 {
                if rng.below(3) > 0 || model.is_empty() {
                    let t = q.now() + rng.below(50);
                    q.schedule(t, next_id);
                    model.push((t, next_id));
                    next_id += 1;
                } else {
                    let (i, _) = model.iter().enumerate().min_by_key(|(_, e)| **e).unwrap();
                    let want = model.remove(i);
                    assert_eq!(q.pop(), Some(want), "seed {seed}");
                }
            }
        }
    }
}
