use moka::future::Cache;
use std::hash::Hash;
// placeholder storage module
//
// this will be replaced by the multi level (in memory + redis/memcached) once we find
// a suitable implementation.

// these trait bounds should be revisited if we move away from moka
#[derive(Clone)]
pub(crate) struct CacheStorage<K: Hash + Eq + Send + Sync, V: Clone> {
    inner: Cache<K, V>,
}

impl<K, V> CacheStorage<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub(crate) async fn new(max_capacity: usize) -> Self {
        Self {
            inner: Cache::new(max_capacity as u64),
        }
    }

    pub(crate) async fn get(&self, key: &K) -> Option<V> {
        self.inner.get(key)
    }

    pub(crate) async fn insert(&mut self, key: K, value: V) {
        self.inner.insert(key, value).await
    }
}
