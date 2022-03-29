use crate::{CacheResolver, CacheResolverError};
use derivative::Derivative;
use futures::lock::Mutex;
use lru::LruCache;
use std::cmp::Eq;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::sync::{Arc, Weak};
use tokio::sync::broadcast::{self, Sender};

/// A caching map optimised for slow value resolution.
///
/// The CachingMap hold values in an LruCache. Values are loaded into the cache on a cache miss and
/// the cache relies on the resolver to provide values. There is no way to manually remove, update
/// or otherwise invalidate a cache value at this time. Values will be evicted from the cache once
/// the cache_limit is reached.
#[derive(Derivative)]
#[derivative(Debug)]
pub struct CachingMap<K, V> {
    #[derivative(Debug = "ignore")]
    cached: Mutex<LruCache<K, Result<V, CacheResolverError>>>,
    #[allow(clippy::type_complexity)]
    #[derivative(Debug = "ignore")]
    wait_map: Mutex<HashMap<K, Weak<Sender<(K, Result<V, CacheResolverError>)>>>>,
    cache_limit: usize,
    #[derivative(Debug = "ignore")]
    resolver: Box<dyn CacheResolver<K, V> + Send + Sync>,
}

impl<K, V> CachingMap<K, V>
where
    K: Clone + fmt::Debug + Eq + Hash + Send + Sync + 'static,
    V: fmt::Debug + Send + Sync + 'static,
    Result<V, CacheResolverError>: Clone,
{
    /// Create a new CachingMap.
    ///
    /// resolver is used to resolve cache misses.
    /// cache_limit specifies the size (number of items) of the cache
    pub fn new(resolver: Box<(dyn CacheResolver<K, V> + Send + Sync)>, cache_limit: usize) -> Self {
        Self {
            cached: Mutex::new(LruCache::new(cache_limit)),
            wait_map: Mutex::new(HashMap::new()),
            cache_limit,
            resolver,
        }
    }

    /// Get a value from the cache.
    pub async fn get(&self, key: K) -> Result<V, CacheResolverError> {
        let mut locked_cache = self.cached.lock().await;
        if let Some(value) = locked_cache.get(&key).cloned() {
            return value;
        }

        // Holding a lock across the delegated get is a bad idea because
        // the delegate get() could take a long time during which all
        // other get() requests are blocked.
        // Alternatively, if we don't hold the lock, there is a risk
        // that we will do the work multiple times. This is also
        // sub-optimal.

        // To work around this, we keep a list of keys we are currently
        // processing in the delegate. If we try to get a key on this
        // list, we block and wait for it to complete and then retry.
        //
        // This is more complex than either of the two simple
        // alternatives but succeeds in providing a mechanism where each
        // client only waits for uncached values that they are going to
        // use AND avoids generating the value multiple times.

        let mut locked_wait_map = self.wait_map.lock().await;

        // We must only drop the locked cache after we have locked the
        // wait map. Otherwise,we might get a race that causes us to
        // miss a broadcast.
        drop(locked_cache);

        loop {
            match locked_wait_map.get_mut(&key) {
                Some(weak_waiter) => {
                    // Try to upgrade our weak Arc. If we can't, the sender must have
                    // been cancelled, so remove the entry from the map and try again.
                    let waiter = match Weak::upgrade(weak_waiter) {
                        Some(waiter) => waiter,
                        None => {
                            locked_wait_map.remove(&key);
                            continue;
                        }
                    };
                    // Register interest in key
                    let mut receiver = waiter.subscribe();
                    drop(locked_wait_map);
                    match receiver.recv().await {
                        Ok((recv_key, recv_value)) => {
                            debug_assert_eq!(recv_key, key);
                            return recv_value;
                        }
                        // there was an issue with the broadcast channel, retry fetching
                        Err(_) => {
                            locked_wait_map = self.wait_map.lock().await;
                            continue;
                        }
                    }
                }
                None => {
                    let (tx, _rx) = broadcast::channel(1);
                    let tx = Arc::new(tx);
                    locked_wait_map.insert(key.clone(), Arc::downgrade(&tx));
                    drop(locked_wait_map);
                    // This is the potentially high duration operation where we ask our resolver to
                    // resolve the key (retrieve a value) for us
                    // No cache locks are held here
                    let value = self.resolver.retrieve(key.clone()).await;

                    // this is a separate block used to release the locks after editing the cache and wait map,
                    // but before broadcasting the value
                    {
                        // Update our cache
                        let mut locked_cache = self.cached.lock().await;
                        locked_cache.put(key.clone(), value.clone());
                        // Update our wait list
                        let mut locked_wait_map = self.wait_map.lock().await;
                        locked_wait_map.remove(&key);
                    }

                    // Let our waiters know
                    let broadcast_value = value.clone();
                    // We may get errors here, for instance if a task is cancelled,
                    // so just ignore the result of send
                    let _ = tokio::task::spawn_blocking(move || {
                        tx.send((key, broadcast_value))
                    })
                    .await
                    .expect("can only fail if the task is aborted or if the internal code panics, neither is possible here; qed");
                    return value;
                }
            }
        }
    }

    /// Get the top 20% of most recently (LRU) used keys
    pub async fn get_hot_keys(&self) -> Vec<K> {
        let locked_cache = self.cached.lock().await;
        locked_cache
            .iter()
            .take(self.cache_limit / 5)
            .map(|(key, _value)| key.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CacheResolverError;
    use async_trait::async_trait;
    use futures::stream::{FuturesUnordered, StreamExt};
    use mockall::mock;
    use test_log::test;

    struct HasACache {
        cm: CachingMap<usize, usize>,
    }

    struct HasACacheResolver {}

    impl HasACache {
        // fn new(resolver: limit: usize) -> Self {
        fn new(
            resolver: Box<(dyn CacheResolver<usize, usize> + Send + Sync)>,
            cache_limit: usize,
        ) -> Self {
            // let resolver = Box::new(HasACacheResolver {});
            let cm = CachingMap::new(resolver, cache_limit);
            Self { cm }
        }

        async fn get(&self, key: usize) -> Result<usize, CacheResolverError> {
            self.cm.get(key).await
        }
    }

    #[async_trait]
    impl CacheResolver<usize, usize> for HasACacheResolver {
        async fn retrieve(&self, key: usize) -> Result<usize, CacheResolverError> {
            Ok(key)
        }
    }

    mock! {
        HasACacheResolver {}

        #[async_trait]
        impl CacheResolver<usize, usize> for HasACacheResolver {
            async fn retrieve(&self, key: usize) -> Result<usize, CacheResolverError>;
        }
    }

    #[test(tokio::test)]
    async fn it_should_enforce_cache_limits() {
        let cache = HasACache::new(Box::new(HasACacheResolver {}), 13);

        for i in 0..14 {
            cache.get(i).await.expect("gets the value");
        }
        let guard = cache.cm.cached.lock().await;
        assert_eq!(guard.len(), 13);
    }

    #[test(tokio::test)]
    async fn it_should_only_delegate_once_per_key() {
        let mut mock = MockHasACacheResolver::new();

        mock.expect_retrieve().times(1).return_const(Ok(1));

        let cache = HasACache::new(Box::new(mock), 10);

        // Let's trigger 100 concurrent gets of the same value and ensure only
        // one delegated retrieve is made
        let mut computations: FuturesUnordered<_> = (0..100).map(|_| cache.get(1)).collect();

        while let Some(result) = computations.next().await {
            result.expect("result retrieved");
        }

        // To be really sure, check there is only one value in the cache
        let guard = cache.cm.cached.lock().await;
        assert_eq!(guard.len(), 1);
    }
}
