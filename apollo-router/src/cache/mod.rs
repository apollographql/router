use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio::sync::Mutex;

use self::storage::CacheStorage;

pub(crate) mod coalescing;
pub(crate) mod storage;

type WaitMap<K, V: Clone> = Arc<Mutex<HashMap<K, broadcast::Sender<Result<V, String>>>>>;

#[derive(Clone)]
pub(crate) struct DedupCache<K: Clone + Send + Sync + Eq + Hash, V: Clone> {
    wait_map: WaitMap<K, V>,
    storage: CacheStorage<K, V>,
}

impl<K, V> DedupCache<K, V>
where
    K: Clone + Send + Sync + Eq + Hash + 'static,
    V: Clone + Send + Sync + 'static,
{
    async fn get(&mut self, key: K) -> Entry<K, V> {
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
                    sender.send(Ok(value.clone()));

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

    async fn insert(&mut self, key: K, value: V) {
        self.storage.insert(key.clone(), value.clone()).await;
        let mut locked_wait_map = self.wait_map.lock().await;
        let opt_sender = locked_wait_map.remove(&key);
        /*drop(locked_wait_map);
        if let Some(sender) = opt_sender {
            sender.send(Ok(value));
        }*/
    }
}

pub struct Entry<K: Clone + Send + Sync + Eq + Hash, V: Clone + Send + Sync> {
    inner: EntryInner<K, V>,
}
enum EntryInner<K: Clone + Send + Sync + Eq + Hash, V: Clone + Send + Sync> {
    First {
        key: K,
        sender: broadcast::Sender<Result<V, String>>,
        cache: DedupCache<K, V>,
        _drop_signal: oneshot::Sender<()>,
    },
    Receiver {
        receiver: broadcast::Receiver<Result<V, String>>,
    },
    Value(V),
}

impl<K, V> Entry<K, V>
where
    K: Clone + Send + Sync + Eq + Hash + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub fn is_first(&self) -> bool {
        if let EntryInner::First { .. } = self.inner {
            true
        } else {
            false
        }
    }

    pub async fn get(self) -> Result<V, String> {
        match self.inner {
            // there was already a value in cache
            EntryInner::Value(v) => Ok(v),
            EntryInner::Receiver { mut receiver } => receiver.recv().await.unwrap(),
            _ => panic!("should not call get on the first call"),
        }
    }

    pub async fn insert(self, value: V) {
        match self.inner {
            EntryInner::First {
                key,
                sender,
                mut cache,
                _drop_signal,
            } => {
                cache.insert(key.clone(), value.clone()).await;
                sender.send(Ok(value));
            }
            _ => {}
        }
    }

    pub async fn error(self, error: String) {
        match self.inner {
            EntryInner::First { sender, .. } => {
                let _ = sender.send(Err(error));
            }
            _ => {}
        }
    }
}

async fn example_cache_usage(
    k: String,
    cache: &mut DedupCache<String, String>,
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
