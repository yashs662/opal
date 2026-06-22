//! `BoundedMap` — a small FIFO-capped map for insert-mostly caches.
//!
//! The in-memory caches (resolved track details, per-cover accents, the
//! playlist-detail cache) are otherwise insert-only: a long listening
//! session grows them one entry per unique track / cover / playlist seen.
//! Each entry is tiny, but "unbounded" is still wrong for a process meant
//! to run for hours.
//!
//! This caps them. **UX comes first**: eviction is FIFO, so the entry
//! dropped on overflow is always the *oldest-inserted* one — never what's
//! on screen (the current cover/track is the most-recent insert). A
//! re-visit to an evicted entry just re-resolves it, and every one of
//! these caches is backed by the on-disk cache, so that re-resolve is a
//! local read, not a network round-trip. With the caps set generously
//! (see the call sites) a normal session never evicts at all — the bound
//! is a backstop against pathological growth, not a working constraint.
//!
//! Re-inserting an existing key overwrites in place and does **not** renew
//! its age; that's intentional and harmless for these pure caches.

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

pub struct BoundedMap<K, V> {
    map: HashMap<K, V>,
    /// Insertion order of live keys, oldest at the front. May briefly hold
    /// keys already removed via [`Self::remove`]; eviction skips those.
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V> BoundedMap<K, V> {
    /// A map that holds at most `cap` entries (must be ≥ 1).
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Insert (or overwrite) `key`. On overflow, evict oldest-inserted
    /// entries until back within `cap`.
    pub fn insert(&mut self, key: K, value: V) {
        let is_new = !self.map.contains_key(&key);
        self.map.insert(key.clone(), value);
        if is_new {
            self.order.push_back(key);
            // Drive eviction off the live map length, not `order.len()`,
            // so stale `order` entries (post-`remove`) can't over-evict.
            while self.map.len() > self.cap {
                match self.order.pop_front() {
                    // A live key → this is the eviction.
                    Some(old) => {
                        self.map.remove(&old);
                    }
                    None => break,
                }
            }
        }
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.get(key)
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.contains_key(key)
    }

    /// Remove `key`. Its slot in the order queue is left stale and skipped
    /// during a later eviction — cheap, and these maps remove rarely.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.map.remove(key)
    }

    /// Live entry count (excludes stale order-queue slots).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_oldest_first() {
        let mut m = BoundedMap::new(2);
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("c", 3); // evicts "a"
        assert_eq!(m.get(&"a"), None);
        assert_eq!(m.get(&"b"), Some(&2));
        assert_eq!(m.get(&"c"), Some(&3));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn overwrite_does_not_grow_or_renew() {
        let mut m = BoundedMap::new(2);
        m.insert("a", 1);
        m.insert("b", 2);
        m.insert("a", 10); // overwrite, age unchanged
        m.insert("c", 3); // evicts "a" (still oldest), not "b"
        assert_eq!(m.get(&"a"), None);
        assert_eq!(m.get(&"b"), Some(&2));
        assert_eq!(m.get(&"c"), Some(&3));
    }

    #[test]
    fn remove_then_insert_stays_bounded() {
        let mut m = BoundedMap::new(2);
        m.insert("a", 1);
        m.insert("b", 2);
        m.remove(&"a"); // leaves a stale order entry
        m.insert("c", 3);
        m.insert("d", 4); // stale "a" skipped, evicts "b"
        assert!(m.len() <= 2);
        assert_eq!(m.get(&"c"), Some(&3));
        assert_eq!(m.get(&"d"), Some(&4));
    }
}
