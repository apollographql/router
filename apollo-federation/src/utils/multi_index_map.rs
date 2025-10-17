use std::hash::Hash;
use std::ops::Deref;

use apollo_compiler::collections::IndexMap;

/// A simple MultiMap implementation using IndexMap with Vec<V> as its value type.
/// - Preserves the insertion order of keys and values.
pub(crate) struct MultiIndexMap<K, V>(IndexMap<K, Vec<V>>);

impl<K, V> Deref for MultiIndexMap<K, V> {
    type Target = IndexMap<K, Vec<V>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K, V> MultiIndexMap<K, V>
where
    K: Eq + Hash,
{
    pub(crate) fn new() -> Self {
        Self(IndexMap::default())
    }

    pub(crate) fn insert(&mut self, key: K, value: V) {
        self.0.entry(key).or_default().push(value);
    }

    pub(crate) fn extend<I: IntoIterator<Item = (K, V)>>(&mut self, iterable: I) {
        for (key, value) in iterable {
            self.insert(key, value);
        }
    }
}
