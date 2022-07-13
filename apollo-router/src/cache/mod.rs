use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::sync::Mutex;

use self::storage::CacheStorage;

pub(crate) mod storage;

type WaitMap<K, V> = Arc<Mutex<HashMap<K, broadcast::Sender<V>>>>;

/// Cache implementation with query deduplication
#[derive(Clone)]
pub(crate) struct DeduplicatingCache<K: Clone + Send + Eq + Hash, V: Clone> {
    wait_map: WaitMap<K, V>,
    storage: CacheStorage<K, V>,
}

impl<K, V> DeduplicatingCache<K, V>
where
    K: Clone + Send + Eq + Hash + 'static,
    V: Clone + Send + 'static,
{
    pub(crate) async fn new(capacity: usize) -> Self {
        Self {
            wait_map: Arc::new(Mutex::new(HashMap::new())),
            storage: CacheStorage::new(capacity).await,
        }
    }

    pub(crate) async fn get(&self, key: &K) -> Entry<K, V> {
        let mut locked_wait_map = self.wait_map.lock().await;
        match locked_wait_map.get(key) {
            Some(waiter) => {
                // Register interest in key
                let receiver = waiter.subscribe();
                Entry {
                    inner: EntryInner::Receiver { receiver },
                }
            }
            None => {
                let (sender, _receiver) = broadcast::channel(1);

                locked_wait_map.insert(key.clone(), sender.clone());

                drop(locked_wait_map);

                if let Some(value) = self.storage.get(key).await {
                    let mut locked_wait_map = self.wait_map.lock().await;
                    let _ = locked_wait_map.remove(key);
                    let _ = sender.send(value.clone());

                    return Entry {
                        inner: EntryInner::Value(value),
                    };
                }

                let k = key.clone();
                // when _drop_signal is dropped, either by getting out of the block, returning
                // the error from ready_oneshot or by cancellation, the drop_sentinel future will
                // return with Err(), then we remove the entry from the wait map
                let (_drop_signal, drop_sentinel) = oneshot::channel::<()>();
                let wait_map = self.wait_map.clone();
                tokio::task::spawn(async move {
                    let _ = drop_sentinel.await;
                    let mut locked_wait_map = wait_map.lock().await;
                    let _ = locked_wait_map.remove(&k);
                });

                Entry {
                    inner: EntryInner::First {
                        sender,
                        key: key.clone(),
                        cache: self.clone(),
                        _drop_signal,
                    },
                }
            }
        }
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.storage.insert(key, value.clone()).await;
    }

    pub(crate) async fn remove_wait(&self, key: &K) {
        let mut locked_wait_map = self.wait_map.lock().await;
        let _ = locked_wait_map.remove(key);
    }
}

pub(crate) struct Entry<K: Clone + Send + Eq + Hash, V: Clone + Send> {
    inner: EntryInner<K, V>,
}
enum EntryInner<K: Clone + Send + Eq + Hash, V: Clone + Send> {
    First {
        key: K,
        sender: broadcast::Sender<V>,
        cache: DeduplicatingCache<K, V>,
        _drop_signal: oneshot::Sender<()>,
    },
    Receiver {
        receiver: broadcast::Receiver<V>,
    },
    Value(V),
}

#[derive(Debug)]
pub(crate) enum EntryError {
    Closed,
    IsFirst,
}

impl<K, V> Entry<K, V>
where
    K: Clone + Send + Eq + Hash + 'static,
    V: Clone + Send + 'static,
{
    pub(crate) fn is_first(&self) -> bool {
        matches!(self.inner, EntryInner::First { .. })
    }

    pub(crate) async fn get(self) -> Result<V, EntryError> {
        match self.inner {
            // there was already a value in cache
            EntryInner::Value(v) => Ok(v),
            EntryInner::Receiver { mut receiver } => {
                receiver.recv().await.map_err(|_| EntryError::Closed)
            }
            _ => Err(EntryError::IsFirst),
        }
    }

    pub(crate) async fn insert(self, value: V) {
        if let EntryInner::First {
            key,
            sender,
            cache,
            _drop_signal,
        } = self.inner
        {
            cache.insert(key.clone(), value.clone()).await;
            cache.remove_wait(&key).await;
            let _ = sender.send(value);
        }
    }

    /// sends the value without storing it into the cache
    #[allow(unused)]
    pub(crate) async fn send(self, value: V) {
        if let EntryInner::First {
            sender, cache, key, ..
        } = self.inner
        {
            let _ = sender.send(value);
            cache.remove_wait(&key).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeduplicatingCache;

    #[tokio::test]
    async fn example_cache_usage() {
        let k = "key".to_string();
        let cache = DeduplicatingCache::new(1).await;

        let entry = cache.get(&k).await;

        if entry.is_first() {
            // potentially long and complex async task that can fail
            let value = "hello".to_string();
            entry.insert(value.clone()).await;
            value
        } else {
            entry.get().await.unwrap()
        };
    }
}
