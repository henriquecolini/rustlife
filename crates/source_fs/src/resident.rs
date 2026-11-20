//! The files a mount keeps in memory so they can be read without waiting.
//!
//! Content that arrives asynchronously (a browser) cannot be handed to code
//! that reads synchronously, such as building a level's physics from its
//! models. Such code asks the mount for a file that is *resident*; the
//! caller first makes the files resident (an `await`), and the synchronous
//! read then finds them here. Files are kept up to a byte budget, least
//! recently used first, except *pinned* ones, which stay.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

/// A byte-budgeted, least-recently-used cache of whole files by path.
///
/// One entry larger than the whole budget is still kept (a large map must be
/// readable), and is the first to go when anything else arrives.
#[derive(Debug)]
pub struct ResidentCache {
    budget: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<String, Entry>,
    /// Bytes held by all entries.
    bytes: usize,
    /// Advances on every touch; the entry with the lowest value is the least
    /// recently used.
    clock: u64,
}

#[derive(Debug)]
struct Entry {
    data: Arc<[u8]>,
    pinned: bool,
    last_used: u64,
}

impl ResidentCache {
    /// A cache holding up to `budget` bytes of unpinned files.
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        // The state is a map and two counters, valid after any panic.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The resident file at `path`, marking it recently used.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<Arc<[u8]>> {
        let mut inner = self.inner();
        inner.clock += 1;
        let clock = inner.clock;
        let entry = inner.entries.get_mut(path)?;
        entry.last_used = clock;
        Some(Arc::clone(&entry.data))
    }

    /// Whether `path` is resident.
    #[must_use]
    pub fn contains(&self, path: &str) -> bool {
        self.inner().entries.contains_key(path)
    }

    /// Makes `data` resident at `path`, replacing any earlier copy, then
    /// evicts least recently used unpinned files until the budget holds.
    /// `pinned` files are never evicted.
    pub fn insert(&self, path: &str, data: Arc<[u8]>, pinned: bool) {
        let mut inner = self.inner();
        inner.clock += 1;
        let last_used = inner.clock;
        let added = data.len();
        let previous = inner.entries.insert(
            path.to_owned(),
            Entry {
                data,
                pinned,
                last_used,
            },
        );
        inner.bytes += added;
        if let Some(previous) = previous {
            inner.bytes -= previous.data.len();
        }
        inner.evict(self.budget);
    }

    /// Pins `path` if it is resident, so it is never evicted.
    pub fn pin(&self, path: &str) {
        if let Some(entry) = self.inner().entries.get_mut(path) {
            entry.pinned = true;
        }
    }

    /// Bytes held, pinned or not.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.inner().bytes
    }

    /// Number of resident files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner().entries.len()
    }

    /// Whether nothing is resident.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Inner {
    /// Drops least recently used unpinned entries until the unpinned bytes
    /// fit `budget`.
    fn evict(&mut self, budget: usize) {
        let mut unpinned: usize = self
            .entries
            .values()
            .filter(|entry| !entry.pinned)
            .map(|entry| entry.data.len())
            .sum();
        while unpinned > budget {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(_, entry)| !entry.pinned)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            // A single entry over the budget is kept: dropping it would make
            // it unreadable the moment it arrived.
            if self.entries.values().filter(|entry| !entry.pinned).count() == 1 {
                break;
            }
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.data.len();
                unpinned -= entry.data.len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn bytes(len: usize) -> Arc<[u8]> {
        vec![7; len].into()
    }

    #[test]
    fn stores_and_returns_files() {
        let cache = ResidentCache::new(100);
        assert!(cache.is_empty());
        cache.insert("a", bytes(10), false);
        assert!(cache.contains("a"));
        assert_eq!(cache.get("a").unwrap().len(), 10);
        assert!(cache.get("b").is_none());
        assert_eq!(cache.bytes(), 10);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn replacing_a_file_does_not_double_count_it() {
        let cache = ResidentCache::new(100);
        cache.insert("a", bytes(10), false);
        cache.insert("a", bytes(30), false);
        assert_eq!(cache.bytes(), 30);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn evicts_the_least_recently_used_first() {
        let cache = ResidentCache::new(25);
        cache.insert("a", bytes(10), false);
        cache.insert("b", bytes(10), false);
        // Touch "a" so "b" is now the oldest.
        assert!(cache.get("a").is_some());
        cache.insert("c", bytes(10), false);
        assert!(cache.contains("a"));
        assert!(!cache.contains("b"));
        assert!(cache.contains("c"));
        assert_eq!(cache.bytes(), 20);
    }

    #[test]
    fn pinned_files_survive_pressure_and_do_not_count_against_the_budget() {
        let cache = ResidentCache::new(10);
        cache.insert("pinned", bytes(50), true);
        cache.insert("a", bytes(10), false);
        cache.insert("b", bytes(10), false);
        assert!(cache.contains("pinned"));
        assert!(!cache.contains("a"));
        assert!(cache.contains("b"));
    }

    #[test]
    fn pinning_later_protects_a_file() {
        let cache = ResidentCache::new(10);
        cache.insert("a", bytes(10), false);
        cache.pin("a");
        cache.insert("b", bytes(10), false);
        cache.insert("c", bytes(10), false);
        assert!(cache.contains("a"));
        assert!(!cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn one_file_over_the_budget_is_kept_until_something_else_arrives() {
        let cache = ResidentCache::new(10);
        cache.insert("big", bytes(100), false);
        assert!(cache.contains("big"));
        cache.insert("small", bytes(5), false);
        assert!(!cache.contains("big"));
        assert!(cache.contains("small"));
    }

    proptest! {
        /// After any sequence of inserts, the unpinned bytes fit the budget
        /// (unless a single file alone exceeds it), pinned files are all still
        /// there, and the byte count matches what is held.
        #[test]
        fn the_budget_holds(
            budget in 1_usize..200,
            files in proptest::collection::vec((0_u8..12, 1_usize..80, any::<bool>()), 0..40),
        ) {
            let cache = ResidentCache::new(budget);
            let mut pinned = std::collections::HashSet::new();
            for (name, len, pin) in files {
                let path = format!("f{name}");
                cache.insert(&path, bytes(len), pin);
                if pin { pinned.insert(path); } else { pinned.remove(&path); }
            }
            let inner = cache.inner();
            let unpinned: Vec<usize> = inner.entries.values()
                .filter(|e| !e.pinned).map(|e| e.data.len()).collect();
            let unpinned_total: usize = unpinned.iter().sum();
            prop_assert!(unpinned_total <= budget || unpinned.len() == 1);
            for path in &pinned {
                prop_assert!(inner.entries.contains_key(path));
            }
            let held: usize = inner.entries.values().map(|e| e.data.len()).sum();
            prop_assert_eq!(inner.bytes, held);
        }
    }
}
