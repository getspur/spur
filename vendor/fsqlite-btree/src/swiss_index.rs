//! Swiss Table implementation for SIMD-accelerated index lookups.
//!
//! Wraps `hashbrown::HashMap` to provide a drop-in replacement for `std::collections::HashMap`
//! with integrated observability (tracing spans and metrics) as required by §7.7.
//!
//! Uses SSE2/AVX2 control byte probing for cache-line parallel lookup (via hashbrown).
//!
//! **Performance note:** Observability (atomic probe counters and tracing spans) is
//! deferred to a cold path gated on `tracing::enabled!(Level::TRACE)`. On the hot
//! path — cursor lookups inside the VDBE opcode dispatch — only the underlying
//! hashbrown operation runs, with zero overhead from instrumentation.

use crate::instrumentation::{record_swiss_probe, set_swiss_load_factor};
use foldhash::fast::FixedState;
use hashbrown::HashMap;
use std::borrow::Borrow;
use std::hash::Hash;
use tracing::Level;

/// A SIMD-accelerated hash map with integrated observability.
///
/// This structure is a thin wrapper around `hashbrown::HashMap` that automatically
/// emits `hash_probe` spans and updates `fsqlite_swiss_table_probes_total` and
/// `fsqlite_swiss_table_load_factor` metrics on operations — but only when
/// TRACE-level tracing is enabled, keeping the hot path zero-cost.
#[derive(Debug, Clone, Default)]
pub struct SwissIndex<K, V> {
    inner: HashMap<K, V, FixedState>,
}

impl<K, V> SwissIndex<K, V>
where
    K: Eq + Hash,
{
    /// Creates an empty `SwissIndex`.
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: HashMap::with_hasher(FixedState::default()),
        }
    }

    /// Creates an empty `SwissIndex` with the specified capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: HashMap::with_capacity_and_hasher(capacity, FixedState::default()),
        }
    }

    /// Returns the number of elements in the map.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true if the map contains no elements.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Inserts a key-value pair into the map.
    #[inline]
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let result = self.inner.insert(key, value);
        self.maybe_record_probe_and_load_factor();
        result
    }

    /// Returns a reference to the value corresponding to the key.
    #[inline]
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let result = self.inner.get(key);
        self.maybe_record_probe();
        result
    }

    /// Returns a mutable reference to the value corresponding to the key.
    #[inline]
    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.maybe_record_probe();
        self.inner.get_mut(key)
    }

    /// Gets the given key's corresponding entry for in-place manipulation,
    /// inserting a default value if the key is absent.
    #[inline]
    pub fn entry_or_insert_with(&mut self, key: K, default: impl FnOnce() -> V) -> &mut V {
        self.maybe_record_probe();
        self.inner.entry(key).or_insert_with(default)
    }

    /// Returns an iterator visiting all values in arbitrary order.
    #[inline]
    pub fn values(&self) -> hashbrown::hash_map::Values<'_, K, V> {
        self.inner.values()
    }

    /// Returns true if the map contains a value for the specified key.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let result = self.inner.contains_key(key);
        self.maybe_record_probe();
        result
    }

    /// Removes a key from the map, returning the value at the key if the key
    /// was previously in the map.
    #[inline]
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let result = self.inner.remove(key);
        self.maybe_record_probe_and_load_factor();
        result
    }

    /// Clears the map, removing all key-value pairs.
    #[inline]
    pub fn clear(&mut self) {
        self.inner.clear();
        if tracing::enabled!(Level::TRACE) {
            self.update_load_factor();
        }
    }

    /// An iterator visiting all key-value pairs in arbitrary order.
    #[inline]
    pub fn iter(&self) -> hashbrown::hash_map::Iter<'_, K, V> {
        self.inner.iter()
    }

    /// An iterator visiting all key-value pairs in arbitrary order, with mutable references to the values.
    #[inline]
    pub fn iter_mut(&mut self) -> hashbrown::hash_map::IterMut<'_, K, V> {
        self.inner.iter_mut()
    }

    // --- Observability Helpers (cold path only) ---

    /// Record a probe event only when TRACE-level tracing is active.
    /// On the hot path this compiles to a single branch on the tracing
    /// subscriber's interest flag — no atomic increment, no span creation.
    #[inline]
    fn maybe_record_probe(&self) {
        if tracing::enabled!(Level::TRACE) {
            self.record_probe_cold();
        }
    }

    /// Combined probe + load-factor update, gated on TRACE.
    #[inline]
    fn maybe_record_probe_and_load_factor(&self) {
        if tracing::enabled!(Level::TRACE) {
            self.record_probe_cold();
            self.update_load_factor();
        }
    }

    #[cold]
    #[inline(never)]
    fn record_probe_cold(&self) {
        record_swiss_probe();
        let span = tracing::span!(
            Level::TRACE,
            "hash_probe",
            probes = 1,
            items = self.len(),
            load_factor = self.load_factor_milli() as f64 / 1000.0
        );
        span.in_scope(|| {});
    }

    #[cold]
    #[inline(never)]
    fn update_load_factor(&self) {
        set_swiss_load_factor(self.load_factor_milli());
    }

    #[inline]
    fn load_factor_milli(&self) -> u64 {
        let capacity = self.inner.capacity();
        if capacity == 0 {
            0
        } else {
            (self.inner.len() as u64 * 1000) / capacity as u64
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}

impl<K, V, Q> std::ops::Index<&Q> for SwissIndex<K, V>
where
    K: Eq + Hash + Borrow<Q>,
    Q: Hash + Eq + ?Sized,
{
    type Output = V;

    fn index(&self, key: &Q) -> &V {
        &self.inner[key]
    }
}

impl<K, V> IntoIterator for SwissIndex<K, V>
where
    K: Eq + Hash,
{
    type Item = (K, V);
    type IntoIter = hashbrown::hash_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.inner.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a SwissIndex<K, V>
where
    K: Eq + Hash,
{
    type Item = (&'a K, &'a V);
    type IntoIter = hashbrown::hash_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, K, V> IntoIterator for &'a mut SwissIndex<K, V>
where
    K: Eq + Hash,
{
    type Item = (&'a K, &'a mut V);
    type IntoIter = hashbrown::hash_map::IterMut<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<K, V> FromIterator<(K, V)> for SwissIndex<K, V>
where
    K: Eq + Hash,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let inner = HashMap::from_iter(iter);
        let index = Self { inner };
        index.update_load_factor();
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swiss_index_basic_ops() {
        let mut map = SwissIndex::new();
        assert!(map.is_empty());

        map.insert("key1", 1);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("key1"), Some(&1));
        assert!(map.contains_key("key1"));

        map.insert("key2", 2);
        assert_eq!(map.len(), 2);

        assert_eq!(map.remove("key1"), Some(1));
        assert_eq!(map.len(), 1);
        assert!(!map.contains_key("key1"));
    }

    #[test]
    fn test_swiss_index_capacity_and_load_factor() {
        let mut map = SwissIndex::with_capacity(100);
        assert_eq!(map.load_factor_milli(), 0);

        for i in 0..50 {
            map.insert(i, i * 10);
        }

        // Load factor should be roughly 50% (capacity might be > 100 due to power of 2 sizing)
        let lf = map.load_factor_milli();
        assert!(lf > 0);
        assert!(lf < 1000);
    }

    #[test]
    fn test_swiss_index_entry_or_insert_with() {
        let mut map = SwissIndex::new();
        let val = map.entry_or_insert_with(42, || 100);
        assert_eq!(*val, 100);
        // Second call should return existing value.
        let val = map.entry_or_insert_with(42, || 999);
        assert_eq!(*val, 100);
    }

    #[test]
    fn test_swiss_index_from_iter() {
        let map: SwissIndex<i32, i32> = [(1, 10), (2, 20), (3, 30)].into_iter().collect();
        assert_eq!(map.len(), 3);
        assert_eq!(map.get(&2), Some(&20));
    }
}
