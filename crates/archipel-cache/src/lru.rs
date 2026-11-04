//! Byte-budgeted LRU caches with named budgets (PLAN §5.1).
//!
//! Every in-memory cache of the application is a [`ByteLru`] registered in a
//! [`CacheRegistry`], so total memory can be reported per cache.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

const NIL: usize = usize::MAX;

struct Node<K, V> {
    key: K,
    value: V,
    bytes: usize,
    prev: usize,
    next: usize,
}

/// Hit/miss/eviction counters of a cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Successful lookups.
    pub hits: u64,
    /// Failed lookups.
    pub misses: u64,
    /// Entries evicted to respect the budget.
    pub evictions: u64,
}

/// A least-recently-used cache bounded by a byte budget.
///
/// `get` and `insert` are O(1): entries live in a slab and are linked in a
/// doubly linked recency list by index. An entry larger than the whole budget
/// is not stored.
pub struct ByteLru<K, V> {
    map: HashMap<K, usize>,
    slab: Vec<Option<Node<K, V>>>,
    free: Vec<usize>,
    head: usize,
    tail: usize,
    bytes: usize,
    budget: usize,
    stats: CacheStats,
}

impl<K: Hash + Eq + Clone, V> ByteLru<K, V> {
    /// An empty cache holding at most `budget` bytes.
    #[must_use]
    pub fn new(budget: usize) -> Self {
        Self {
            map: HashMap::new(),
            slab: Vec::new(),
            free: Vec::new(),
            head: NIL,
            tail: NIL,
            bytes: 0,
            budget,
            stats: CacheStats::default(),
        }
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Bytes currently accounted.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The byte budget.
    #[must_use]
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Counters.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Looks up a value and marks it most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let Some(&idx) = self.map.get(key) else {
            self.stats.misses += 1;
            return None;
        };
        self.stats.hits += 1;
        self.unlink(idx);
        self.push_front(idx);
        self.slab[idx].as_ref().map(|n| &n.value)
    }

    /// Whether `key` is cached (does not change recency).
    #[must_use]
    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Inserts a value weighing `bytes`, evicting least recently used entries
    /// as needed. Returns `false` (and stores nothing) when `bytes` exceeds the budget.
    pub fn insert(&mut self, key: K, value: V, bytes: usize) -> bool {
        self.remove(&key);
        if bytes > self.budget {
            return false;
        }
        while self.bytes + bytes > self.budget && self.tail != NIL {
            let tail = self.tail;
            self.evict(tail);
            self.stats.evictions += 1;
        }
        let node = Node {
            key: key.clone(),
            value,
            bytes,
            prev: NIL,
            next: NIL,
        };
        let idx = if let Some(i) = self.free.pop() {
            self.slab[i] = Some(node);
            i
        } else {
            self.slab.push(Some(node));
            self.slab.len() - 1
        };
        self.push_front(idx);
        self.map.insert(key, idx);
        self.bytes += bytes;
        true
    }

    /// Removes an entry and returns its value.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let idx = self.map.remove(key)?;
        self.unlink(idx);
        let node = self.slab[idx].take()?;
        self.free.push(idx);
        self.bytes -= node.bytes;
        Some(node.value)
    }

    /// Removes every entry.
    pub fn clear(&mut self) {
        self.map.clear();
        self.slab.clear();
        self.free.clear();
        self.head = NIL;
        self.tail = NIL;
        self.bytes = 0;
    }

    /// Changes the budget, evicting entries if it shrank.
    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        while self.bytes > self.budget && self.tail != NIL {
            let tail = self.tail;
            self.evict(tail);
            self.stats.evictions += 1;
        }
    }

    fn evict(&mut self, idx: usize) {
        let key = self.slab[idx].as_ref().map(|n| n.key.clone());
        if let Some(key) = key {
            self.remove(&key);
        }
    }

    fn unlink(&mut self, idx: usize) {
        let (prev, next) = match &self.slab[idx] {
            Some(n) => (n.prev, n.next),
            None => return,
        };
        if prev == NIL {
            self.head = next;
        } else if let Some(p) = self.slab[prev].as_mut() {
            p.next = next;
        }
        if next == NIL {
            self.tail = prev;
        } else if let Some(n) = self.slab[next].as_mut() {
            n.prev = prev;
        }
    }

    fn push_front(&mut self, idx: usize) {
        let old_head = self.head;
        if let Some(n) = self.slab[idx].as_mut() {
            n.prev = NIL;
            n.next = old_head;
        }
        if old_head == NIL {
            self.tail = idx;
        } else if let Some(h) = self.slab[old_head].as_mut() {
            h.prev = idx;
        }
        self.head = idx;
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Memory report of one named cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheReport {
    /// Cache name.
    pub name: String,
    /// Entries.
    pub entries: usize,
    /// Bytes used.
    pub bytes: usize,
    /// Byte budget.
    pub budget: usize,
    /// Counters.
    pub stats: CacheStats,
}

trait Reportable: Send + Sync {
    fn report(&self) -> CacheReport;
}

/// A thread-safe [`ByteLru`] with a name, created through a [`CacheRegistry`].
pub struct SharedLru<K, V> {
    name: String,
    inner: Mutex<ByteLru<K, V>>,
}

impl<K: Hash + Eq + Clone, V: Clone> SharedLru<K, V> {
    /// A standalone cache (not registered anywhere).
    #[must_use]
    pub fn new(name: impl Into<String>, budget: usize) -> Self {
        Self {
            name: name.into(),
            inner: Mutex::new(ByteLru::new(budget)),
        }
    }

    /// Clones the cached value out, marking it most recently used.
    pub fn get(&self, key: &K) -> Option<V> {
        lock(&self.inner).get(key).cloned()
    }

    /// Inserts a value weighing `bytes`.
    pub fn insert(&self, key: K, value: V, bytes: usize) -> bool {
        lock(&self.inner).insert(key, value, bytes)
    }

    /// Removes an entry.
    pub fn remove(&self, key: &K) -> Option<V> {
        lock(&self.inner).remove(key)
    }

    /// Removes every entry.
    pub fn clear(&self) {
        lock(&self.inner).clear();
    }

    /// Bytes currently used.
    #[must_use]
    pub fn bytes(&self) -> usize {
        lock(&self.inner).bytes()
    }
}

impl<K: Hash + Eq + Clone + Send, V: Clone + Send> Reportable for SharedLru<K, V> {
    fn report(&self) -> CacheReport {
        let inner = lock(&self.inner);
        CacheReport {
            name: self.name.clone(),
            entries: inner.len(),
            bytes: inner.bytes(),
            budget: inner.budget(),
            stats: inner.stats(),
        }
    }
}

/// Creates named caches and reports their memory use.
///
/// The registry only keeps weak references: dropping a cache (e.g. when a
/// project is unloaded) removes it from reports.
#[derive(Default)]
pub struct CacheRegistry {
    caches: Mutex<Vec<Weak<dyn Reportable>>>,
}

impl CacheRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates and registers a cache.
    pub fn create<K, V>(&self, name: impl Into<String>, budget: usize) -> Arc<SharedLru<K, V>>
    where
        K: Hash + Eq + Clone + Send + 'static,
        V: Clone + Send + 'static,
    {
        let cache = Arc::new(SharedLru::new(name, budget));
        let weak: Weak<dyn Reportable> = Arc::downgrade(&(cache.clone() as Arc<dyn Reportable>));
        lock(&self.caches).push(weak);
        cache
    }

    /// Reports every live cache.
    #[must_use]
    pub fn report(&self) -> Vec<CacheReport> {
        let mut caches = lock(&self.caches);
        caches.retain(|w| w.strong_count() > 0);
        caches
            .iter()
            .filter_map(Weak::upgrade)
            .map(|c| c.report())
            .collect()
    }

    /// Total bytes used by every live cache.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.report().iter().map(|r| r.bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_within_budget() {
        let mut c = ByteLru::new(30);
        c.insert("a", 1, 10);
        c.insert("b", 2, 10);
        c.insert("c", 3, 10);
        assert_eq!(c.get(&"a"), Some(&1)); // a is now most recent
        c.insert("d", 4, 10); // evicts b
        assert!(!c.contains(&"b"));
        assert!(c.contains(&"a") && c.contains(&"c") && c.contains(&"d"));
        assert_eq!(c.bytes(), 30);
        assert_eq!(c.stats().evictions, 1);
    }

    #[test]
    fn oversized_entries_are_rejected() {
        let mut c = ByteLru::new(5);
        assert!(!c.insert(1, (), 6));
        assert!(c.is_empty());
    }

    #[test]
    fn replace_and_remove_keep_accounting() {
        let mut c = ByteLru::new(100);
        c.insert(1, "x", 40);
        c.insert(1, "y", 20);
        assert_eq!(c.bytes(), 20);
        assert_eq!(c.remove(&1), Some("y"));
        assert_eq!(c.bytes(), 0);
        c.insert(2, "z", 10);
        c.set_budget(5);
        assert!(c.is_empty());
    }

    #[test]
    fn stays_bounded_under_churn() {
        let mut c = ByteLru::new(1000);
        for i in 0..100_000u32 {
            c.insert(i, i, 7);
            if i % 3 == 0 {
                c.get(&(i / 2));
            }
            assert!(c.bytes() <= 1000);
        }
        assert!(c.slab.len() <= 1000 / 7 + 1);
    }

    #[test]
    fn registry_reports_live_caches() {
        let reg = CacheRegistry::new();
        let a = reg.create::<u32, String>("commits", 100);
        {
            let b = reg.create::<u32, String>("diffs", 100);
            b.insert(1, "x".into(), 10);
            assert_eq!(reg.report().len(), 2);
        }
        a.insert(1, "y".into(), 25);
        let report = reg.report();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].name, "commits");
        assert_eq!(reg.total_bytes(), 25);
        assert_eq!(a.get(&1).as_deref(), Some("y"));
    }
}
