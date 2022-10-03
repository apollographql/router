use std::hash::Hash;
use std::sync::Arc;

use lru::LruCache;
use redis::cluster::ClusterClient;
use redis::Commands;
use tokio::sync::Mutex;

// placeholder storage module
//
// this will be replaced by the multi level (in memory + redis/memcached) once we find
// a suitable implementation.
#[derive(Clone)]
pub(crate) struct CacheStorage<K: Hash + Eq + Send, V: Clone> {
    inner: Arc<Mutex<LruCache<K, V>>>,
}

impl<K, V> CacheStorage<K, V>
where
    K: Hash + Eq + Send,
    V: Clone + Send,
{
    pub(crate) async fn new(max_capacity: usize) -> Self {
        let nodes = vec!["redis://redis-cluster-headless.redis.svc.cluster.local:6379"];
        let client = ClusterClient::open(nodes).expect("opening ClusterClient");
        let mut connection = client.get_connection().expect("got redis connection");
        let _: () = connection.set("test", "test_data").expect("setting data");
        let rv: String = connection.get("test").expect("getting data");
        tracing::info!("rv: {:?}", rv);

        Self {
            inner: Arc::new(Mutex::new(LruCache::new(max_capacity))),
        }
    }

    pub(crate) async fn get(&self, key: &K) -> Option<V> {
        self.inner.lock().await.get(key).cloned()
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.inner.lock().await.put(key, value);
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}
