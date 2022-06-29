use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::sync::Mutex;

use self::storage::CacheStorage;

pub(crate) mod coalescing;
pub(crate) mod storage;

type WaitMap<K, V, E> = Arc<Mutex<HashMap<K, broadcast::Sender<Result<V, E>>>>>;

#[derive(Clone)]
pub(crate) struct DedupCache<K: Clone + Send + Eq + Hash, V: Clone, E> {
    wait_map: WaitMap<K, V, E>,
    storage: CacheStorage<K, V>,
}

impl<K, V, E> DedupCache<K, V, E>
where
    K: Clone + Send + Eq + Hash + 'static,
    V: Clone + Send + 'static,
    E: Clone + Send + 'static,
{
    pub(crate) async fn new(capacity: usize) -> Self {
        Self {
            wait_map: Arc::new(Mutex::new(HashMap::new())),
            storage: CacheStorage::new(capacity).await,
        }
    }

    pub(crate) async fn get(&self, key: K) -> Entry<K, V, E> {
        //loop {
        let mut locked_wait_map = self.wait_map.lock().await;
        match locked_wait_map.get(&key) {
            Some(waiter) => {
                // Register interest in key
                let receiver = waiter.subscribe();
                return Entry {
                    inner: EntryInner::Receiver { receiver },
                };
            }
            None => {
                let (sender, _receiver) = broadcast::channel(1);

                locked_wait_map.insert(key.clone(), sender.clone());

                drop(locked_wait_map);

                if let Some(value) = self.storage.get(&key).await {
                    let mut locked_wait_map = self.wait_map.lock().await;
                    let _ = locked_wait_map.remove(&key);
                    let _ = sender.send(Ok(value.clone()));

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

                let res = Entry {
                    inner: EntryInner::First {
                        sender,
                        key: key.clone(),
                        cache: self.clone(),
                        _drop_signal,
                    },
                };

                res
            }
        }
    }

    pub(crate) async fn insert(&self, key: K, value: V) {
        self.storage.insert(key, value.clone()).await;
    }

    pub(crate) async fn remove_wait(&self, key: &K) {
        let mut locked_wait_map = self.wait_map.lock().await;
        let _ = locked_wait_map.remove(&key);
    }
}

pub(crate) struct Entry<K: Clone + Send + Eq + Hash, V: Clone + Send, E: Clone + Send> {
    inner: EntryInner<K, V, E>,
}
enum EntryInner<K: Clone + Send + Eq + Hash, V: Clone + Send, E: Clone + Send> {
    First {
        key: K,
        sender: broadcast::Sender<Result<V, E>>,
        cache: DedupCache<K, V, E>,
        _drop_signal: oneshot::Sender<()>,
    },
    Receiver {
        receiver: broadcast::Receiver<Result<V, E>>,
    },
    Value(V),
}

impl<K, V, E> Entry<K, V, E>
where
    K: Clone + Send + Eq + Hash + 'static,
    V: Clone + Send + 'static,
    E: Clone + Send + 'static,
{
    pub(crate) fn is_first(&self) -> bool {
        if let EntryInner::First { .. } = self.inner {
            true
        } else {
            false
        }
    }

    pub(crate) async fn get(self) -> Result<V, E> {
        match self.inner {
            // there was already a value in cache
            EntryInner::Value(v) => Ok(v),
            EntryInner::Receiver { mut receiver } => {
                println!("will receive value");
                receiver.recv().await.unwrap()
            }
            _ => panic!("should not call get on the first call"),
        }
    }

    pub(crate) async fn insert(self, value: V) {
        match self.inner {
            EntryInner::First {
                key,
                sender,
                cache,
                _drop_signal,
            } => {
                cache.insert(key.clone(), value.clone()).await;
                println!("will send value");
                cache.remove_wait(&key).await;
                let _ = sender.send(Ok(value));
            }
            _ => {}
        }
    }

    pub(crate) async fn error(self, error: E) {
        match self.inner {
            EntryInner::First {
                sender, cache, key, ..
            } => {
                let _ = sender.send(Err(error));
                println!("sent error");
                cache.remove_wait(&key).await;
            }
            _ => {}
        }
    }
}

async fn example_cache_usage(
    k: String,
    cache: &mut DedupCache<String, String, String>,
) -> Result<String, String> {
    let entry = cache.get(k).await;

    if entry.is_first() {
        // potentially long and complex async task that can fail
        let value = "hello".to_string();
        entry.insert(value.clone()).await;
        Ok(value)
    } else {
        entry.get().await
    }
}
