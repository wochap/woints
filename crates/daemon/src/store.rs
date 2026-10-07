//! Ring of recent snapshots so later requests (`act`) can resolve ids.

use std::collections::VecDeque;
use std::time::Instant;

use woints_core::model::{Element, Target};

pub const CAPACITY: usize = 8;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: u64,
    pub taken: Instant,
    pub targets: Vec<Target>,
    pub elements: Vec<Element>,
}

#[derive(Debug, Default)]
pub struct SnapshotStore {
    next_id: u64,
    ring: VecDeque<Snapshot>,
}

impl SnapshotStore {
    /// Store a snapshot and return its id (ids start at 1).
    pub fn insert(&mut self, targets: Vec<Target>, elements: Vec<Element>) -> u64 {
        self.next_id += 1;
        if self.ring.len() == CAPACITY {
            self.ring.pop_front();
        }
        self.ring.push_back(Snapshot {
            id: self.next_id,
            taken: Instant::now(),
            targets,
            elements,
        });
        self.next_id
    }

    pub fn get(&self, id: u64) -> Option<&Snapshot> {
        self.ring.iter().find(|s| s.id == id)
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_last_eight() {
        let mut s = SnapshotStore::default();
        let ids: Vec<u64> = (0..10).map(|_| s.insert(vec![], vec![])).collect();
        assert_eq!(ids, (1..=10).collect::<Vec<_>>());
        assert_eq!(s.len(), CAPACITY);
        assert!(s.get(2).is_none());
        assert!(s.get(3).is_some());
        assert!(s.get(10).is_some());
    }
}
