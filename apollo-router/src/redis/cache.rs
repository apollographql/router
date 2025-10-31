use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use redis::FromRedisValue;
use redis::RedisError;
use redis::ToRedisArgs;

use super::Config;
use super::Key;
use super::pool::Pool;

// TODO: does this need a drop impl?
// TODO: better name than cache
#[derive(Clone)]
pub(crate) struct Cache {
    pool: Pool,
    caller: Arc<String>,
    namespace: Option<Arc<String>>,
    ttl: Option<Duration>,
    reset_ttl: bool,
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("{0}")]
    ConnPool(#[from] super::pool::Error),
    #[error("{0}")]
    Redis(#[from] RedisError),
}

impl Cache {
    // TODO: handle required to start and other config options
    fn new(config: Config, caller: &str) -> Result<Self, Error> {
        let pool = Pool::try_from(config.clone())?;
        Ok(Self {
            pool,
            caller: Arc::new(caller.to_string()),
            namespace: config.namespace.map(Arc::new),
            ttl: config.ttl,
            reset_ttl: config.reset_ttl,
        })
    }

    fn namespaced_key<S: ToString, K: Into<Key<S>>>(&self, key: K) -> String {
        let key = key.into();
        match (key, self.namespace.as_ref()) {
            (Key::Simple(key), Some(namespace)) => format!("{namespace}:{}", key.to_string()),
            (Key::Simple(key), _) | (Key::Namespaced(key), _) => key.to_string(),
        }
    }

    async fn get<S: ToString, K: Into<Key<S>>, V: FromRedisValue + std::marker::Send + 'static>(
        &self,
        key: K,
    ) -> Result<V, Error> {
        // TODO: timeout?
        // TODO: need an option to reset ttl...
        let key = self.namespaced_key(key);
        Ok(self.pool.get(key).await?)
    }

    async fn get_multiple<
        S: ToString,
        K: Into<Key<S>>,
        V: FromRedisValue + Send + Sync + 'static,
    >(
        &self,
        keys: Vec<K>,
    ) -> Result<Vec<V>, Error> {
        let keys = keys
            .into_iter()
            .map(|key| self.namespaced_key(key))
            .collect();

        Ok(self.pool.get_multiple(keys).await?)
    }

    async fn insert_multiple<
        S: ToString,
        K: Into<Key<S>>,
        V: FromRedisValue + ToRedisArgs + Send + Sync + 'static + Debug,
    >(
        &self,
        data: Vec<(K, V)>,
        ttl: Option<Duration>,
    ) -> Result<(), Error> {
        let data = data
            .into_iter()
            .map(|(key, value)| (self.namespaced_key(key), value))
            .collect();
        Ok(self.pool.insert_multiple(data, ttl.or(self.ttl)).await?)
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashMap;

    use itertools::Itertools;
    use rand::Rng;
    use rand::RngCore;
    use rand::distr::Alphanumeric;
    use serde_json::json;
    use tower::BoxError;

    use super::Cache;

    // Duplicated from redis-rs as it's not exposed (https://github.com/redis-rs/redis-rs/blob/ce1c5fd0f2f0c3793c87bd2f1ca80a9440cee2c1/redis/src/cluster_handling/routing.rs#L18)
    const SLOT_SIZE: u16 = 16384;
    fn slot(key: &[u8]) -> u16 {
        crc16::State::<crc16::XMODEM>::calculate(key) % SLOT_SIZE
    }
    fn get_hashtag(key: &[u8]) -> Option<&[u8]> {
        let open = key.iter().position(|v| *v == b'{')?;

        let close = key[open..].iter().position(|v| *v == b'}')?;

        let rv = &key[open + 1..open + close];
        (!rv.is_empty()).then_some(rv)
    }
    fn get_slot(key: &[u8]) -> u16 {
        let key = get_hashtag(key).unwrap_or(key);
        slot(key)
    }

    /// Tests that `insert_multiple` and `get_multiple` are successful when run against clustered Redis.
    ///
    /// Clustered Redis works by hashing each key to one of 16384 hash slots, and assigning each hash
    /// slot to a node. Operations which interact with multiple keys (`MGET`, `MSET`) *cannot* be
    /// used on keys which map to different hash slots, even if those hash slots are on the same node.
    ///
    /// This test inserts data that is guaranteed to hash to different slots to verify that
    /// `RedisCacheStorage` is well-behaved when operating against a cluster.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_redis_storage_avoids_common_cross_slot_errors() -> Result<(), BoxError> {
        let config_json = json!({
            "urls": ["redis-cluster://localhost:7000"],
            "namespace": "test_redis_storage_avoids_common_cross_slot_errors",
            "required_to_start": true,
            "ttl": "60s"
        });
        let config = serde_json::from_value(config_json).unwrap();
        let storage = Cache::new(config, "test_redis_cluster")?;
        storage.pool.connect_all().await?;

        // insert values which reflect different cluster slots
        let mut data = HashMap::default();
        let expected_value = rand::rng().next_u32() as usize;
        let unique_cluster_slot_count = |data: &HashMap<String, _>| {
            data.keys()
                .map(|key| get_slot(key.as_bytes()))
                .unique()
                .count()
        };

        while unique_cluster_slot_count(&data) < 50 {
            // NB: include {} around key so that this key is what determines the cluster hash slot - adding
            // the namespace will otherwise change the slot
            let key = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(10)
                .map(char::from)
                .collect::<String>();
            data.insert(format!("{{{}}}", key), expected_value);
        }

        // insert values
        let keys: Vec<_> = data.keys().cloned().collect();
        let data: Vec<_> = data.into_iter().collect();
        storage.insert_multiple(data, None).await?;

        // make a `get` call for each key and ensure that it has the expected value. this tests both
        // the `get` and `insert_multiple` functions
        for key in &keys {
            let value: usize = storage.get(key.clone()).await?;
            assert_eq!(value, expected_value);
        }

        // test the `mget` functionality
        let values: Vec<Option<usize>> = storage.get_multiple(keys).await?;
        for value in values {
            let value: usize = value.expect("missing value");
            assert_eq!(value, expected_value);
        }

        Ok(())
    }

    /// Test that `get_multiple` returns items in the correct order.
    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    #[tokio::test]
    async fn test_get_multiple_is_ordered() -> Result<(), BoxError> {
        let config_json = json!({
            "urls": ["redis://localhost:6379"],
            "namespace": "test_get_multiple_is_ordered",
            "required_to_start": true,
            "ttl": "60s"
        });
        let config = serde_json::from_value(config_json).unwrap();
        let storage = Cache::new(config, "test_get_multiple_is_ordered")?;
        storage.pool.connect_all().await?;

        let data = [("a", "1"), ("b", "2"), ("c", "3")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        storage.insert_multiple(data, None).await?;

        // check different orders of fetches to make everything is ordered correctly, including
        // when some values are none
        let test_cases = vec![
            (vec!["a", "b", "c"], vec![Some("1"), Some("2"), Some("3")]),
            (vec!["c", "b", "a"], vec![Some("3"), Some("2"), Some("1")]),
            (vec!["d", "b", "c"], vec![None, Some("2"), Some("3")]),
            (
                vec!["d", "3", "s", "b", "s", "1", "c", "Y"],
                vec![None, None, None, Some("2"), None, None, Some("3"), None],
            ),
        ];

        for (keys, expected_values) in test_cases {
            // let keys: Vec<_> = keys.into_iter().map(|key| key.to_string()).collect();
            let expected_values: Vec<Option<_>> = expected_values
                .into_iter()
                .map(|x| x.map(ToString::to_string))
                .collect();

            let values: Vec<Option<String>> = storage.get_multiple(keys).await?;
            assert_eq!(values, expected_values);
        }

        Ok(())
    }
}
