use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sqlx::Acquire;
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::chrono::DateTime;
use sqlx::types::chrono::Utc;

use super::cache_control::CacheControl;

#[derive(sqlx::FromRow, Debug, Clone)]
pub(crate) struct CacheEntryRow {
    pub(crate) id: i64,
    pub(crate) cache_key: String,
    pub(crate) data: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) control: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CacheEntry {
    #[allow(unused)] // Used in the database but not in rust code
    pub(crate) id: i64,
    pub(crate) cache_key: String,
    pub(crate) data: serde_json_bytes::Value,
    #[allow(unused)] // Used in the database but not in rust code
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) control: CacheControl,
}

#[derive(Debug, Clone)]
pub(crate) struct BatchDocument {
    pub(crate) cache_key: String,
    pub(crate) data: String,
    pub(crate) control: String,
    pub(crate) invalidation_keys: HashSet<String>,
    pub(crate) expire: Duration,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
/// Postgres cache configuration
pub(crate) struct PostgresCacheConfig {
    /// List of URL to Postgres
    pub(crate) url: url::Url,

    /// PostgreSQL username if not provided in the URLs. This field takes precedence over the username in the URL
    pub(crate) username: Option<String>,
    /// PostgreSQL password if not provided in the URLs. This field takes precedence over the password in the URL
    pub(crate) password: Option<String>,

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    /// PostgreSQL request timeout (default: 4mins)
    pub(crate) timeout: Option<Duration>,

    #[serde(default = "default_required_to_start")]
    /// Prevents the router from starting if it cannot connect to PostgreSQL
    pub(crate) required_to_start: bool,

    #[serde(default = "default_pool_size")]
    /// The size of the PostgreSQL connection pool
    pub(crate) pool_size: u32,
    #[serde(default = "default_batch_size")]
    /// The size of batch when inserting cache entries in PG (default: 100)
    pub(crate) batch_size: usize,
    /// Useful when running tests in parallel to avoid conflicts
    #[serde(default)]
    pub(crate) namespace: Option<String>,
}

pub(super) const fn default_required_to_start() -> bool {
    false
}

pub(super) const fn default_pool_size() -> u32 {
    5
}

pub(super) const fn default_batch_size() -> usize {
    100
}

impl TryFrom<CacheEntryRow> for CacheEntry {
    type Error = serde_json::Error;

    fn try_from(value: CacheEntryRow) -> Result<Self, Self::Error> {
        let data = serde_json::from_str(&value.data)?;
        let control = serde_json::from_str(&value.control)?;
        Ok(Self {
            id: value.id,
            cache_key: value.cache_key,
            data,
            expires_at: value.expires_at,
            control,
        })
    }
}

#[derive(Clone)]
pub(crate) struct PostgresCacheStorage {
    batch_size: usize,
    pg_pool: PgPool,
    namespace: Option<String>,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum PostgresCacheStorageError {
    #[error("invalid configuration: {0}")]
    BadConfiguration(String),
    #[error("postgres error: {0}")]
    PgError(#[from] sqlx::Error),
}

impl PostgresCacheStorage {
    pub(crate) async fn new(conf: &PostgresCacheConfig) -> Result<Self, PostgresCacheStorageError> {
        match (&conf.username, &conf.password) {
            (None, None) => {
                let pg_pool = PgPoolOptions::new()
                    .max_connections(conf.pool_size)
                    .idle_timeout(conf.timeout.or_else(|| Some(Duration::from_secs(60 * 4))))
                    .connect(conf.url.as_ref())
                    .await?;
                Ok(Self { pg_pool, batch_size: conf.batch_size, namespace: conf.namespace.clone() })
            }
            (None, Some(_)) | (Some(_), None) => Err(PostgresCacheStorageError::BadConfiguration(
                "You have to set both username and password for postgres configuration, not only one of them. If there's no password set an empty string".to_string(),
            )),
            (Some(user), Some(password)) => {
                let host = conf
                    .url
                    .host_str()
                    .ok_or_else(|| PostgresCacheStorageError::BadConfiguration("malformed postgres url, doesn't contain host".to_string()))?;
                let port = conf
                    .url
                    .port()
                    .ok_or_else(|| PostgresCacheStorageError::BadConfiguration("malformed postgres url, doesn't contain port".to_string()))?;
                let db_name = conf.url.path();
                let pg_pool = PgPoolOptions::new()
                    .max_connections(conf.pool_size)
                    .idle_timeout(conf.timeout.or_else(|| Some(Duration::from_secs(60 * 4))))
                    .connect_with(
                        PgConnectOptions::new()
                            .host(host)
                            .port(port)
                            .database(db_name)
                            .username(user)
                            .password(password),
                    )
                    .await?;
                Ok(Self { pg_pool, batch_size: conf.batch_size, namespace: conf.namespace.clone() })
            }
        }
    }

    pub(crate) async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!().run(&self.pg_pool).await?;
        Ok(())
    }

    #[cfg(all(
        test,
        any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
    ))]
    pub(crate) async fn truncate_namespace(&self) -> anyhow::Result<()> {
        if let Some(ns) = &self.namespace {
            sqlx::query!("DELETE FROM cache WHERE starts_with(cache_key, $1)", ns)
                .execute(&self.pg_pool)
                .await?;
        }

        Ok(())
    }

    fn namespaced(&self, key: &str) -> String {
        if let Some(ns) = &self.namespace {
            format!("{ns}-{key}")
        } else {
            key.into()
        }
    }

    pub(crate) async fn insert(
        &self,
        cache_key: &str,
        expire: Duration,
        invalidation_keys: HashSet<String>,
        value: serde_json_bytes::Value,
        control: CacheControl,
        subgraph_name: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.pg_pool.acquire().await?;
        let mut transaction = conn.begin().await?;
        let tx = &mut transaction;

        let expired_at = Utc::now() + expire;
        let value_str = serde_json::to_string(&value)?;
        let control_str = serde_json::to_string(&control)?;
        let cache_key = self.namespaced(cache_key);
        let rec = sqlx::query!(
            r#"
        INSERT INTO cache ( cache_key, data, control, expires_at )
        VALUES ( $1, $2, $3, $4 )
        ON CONFLICT (cache_key) DO UPDATE SET data = $2, control = $3, expires_at = $4
        RETURNING id
                "#,
            &cache_key,
            value_str,
            control_str,
            expired_at
        )
        .fetch_one(&mut **tx)
        .await?;

        for invalidation_key in invalidation_keys {
            let invalidation_key = self.namespaced(&invalidation_key);
            sqlx::query!(
                r#"INSERT into invalidation_key (cache_key_id, invalidation_key, subgraph_name) VALUES ($1, $2, $3) ON CONFLICT (cache_key_id, invalidation_key, subgraph_name) DO NOTHING"#,
                rec.id,
                &invalidation_key,
                subgraph_name
            )
            .execute(&mut **tx)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub(crate) async fn insert_in_batch(
        &self,
        batch_docs: Vec<BatchDocument>,
        subgraph_name: &str,
    ) -> anyhow::Result<()> {
        let mut conn = self.pg_pool.acquire().await?;

        let batch_docs = batch_docs.chunks(self.batch_size);
        for batch_docs in batch_docs {
            let mut transaction = conn.begin().await?;
            let tx = &mut transaction;
            let cache_keys = batch_docs
                .iter()
                .map(|b| self.namespaced(&b.cache_key))
                .collect::<Vec<_>>();

            let data = batch_docs
                .iter()
                .map(|b| b.data.clone())
                .collect::<Vec<String>>();
            let controls = batch_docs
                .iter()
                .map(|b| b.control.clone())
                .collect::<Vec<String>>();
            let expires = batch_docs
                .iter()
                .map(|b| Utc::now() + b.expire)
                .collect::<Vec<DateTime<Utc>>>();

            let resp = sqlx::query!(
                r#"
                INSERT INTO cache
                ( cache_key, data, expires_at, control ) SELECT * FROM UNNEST(
                    $1::VARCHAR(1024)[],
                    $2::TEXT[],
                    $3::TIMESTAMP WITH TIME ZONE[],
                    $4::TEXT[]
                ) ON CONFLICT (cache_key) DO UPDATE SET data = excluded.data, control = excluded.control, expires_at = excluded.expires_at
                RETURNING id
                "#,
                &cache_keys,
                &data,
                &expires,
                &controls
            )
            .fetch_all(&mut **tx)
            .await?;

            let invalidation_keys: Vec<(i64, String)> = resp
                .iter()
                .enumerate()
                .flat_map(|(idx, resp)| {
                    let cache_key_id = resp.id;
                    batch_docs
                        .get(idx)
                        .unwrap()
                        .invalidation_keys
                        .iter()
                        .map(move |k| (cache_key_id, k.clone()))
                })
                .collect();

            let cache_key_ids: Vec<i64> = invalidation_keys.iter().map(|(idx, _)| *idx).collect();

            let subgraph_names: Vec<String> = (0..invalidation_keys.len())
                .map(|_| subgraph_name.to_string())
                .collect();
            let invalidation_keys: Vec<String> = invalidation_keys
                .iter()
                .map(|(_, invalidation_key)| self.namespaced(invalidation_key))
                .collect();
            sqlx::query!(
                r#"
                INSERT INTO invalidation_key (cache_key_id, invalidation_key, subgraph_name)
                SELECT * FROM UNNEST(
                    $1::BIGINT[],
                    $2::VARCHAR(255)[],
                    $3::VARCHAR(255)[]
                ) ON CONFLICT (cache_key_id, invalidation_key, subgraph_name) DO NOTHING
                "#,
                &cache_key_ids,
                &invalidation_keys,
                &subgraph_names,
            )
            .execute(&mut **tx)
            .await?;

            transaction.commit().await?;
        }

        Ok(())
    }

    pub(crate) async fn get(&self, cache_key: &str) -> anyhow::Result<CacheEntry> {
        let cache_key = self.namespaced(cache_key);
        let resp = sqlx::query_as!(
            CacheEntryRow,
            "SELECT * FROM cache WHERE cache.cache_key = $1 AND expires_at >= NOW()",
            &cache_key
        )
        .fetch_one(&self.pg_pool)
        .await?;

        let cache_entry_json = resp.try_into()?;

        Ok(cache_entry_json)
    }

    pub(crate) async fn get_multiple(
        &self,
        cache_keys: &[&str],
    ) -> anyhow::Result<Vec<Option<CacheEntry>>> {
        let cache_keys: Vec<_> = cache_keys.iter().map(|ck| self.namespaced(ck)).collect();
        let resp = sqlx::query_as!(
            CacheEntryRow,
            "SELECT * FROM cache WHERE cache.cache_key = ANY($1::VARCHAR(1024)[]) AND expires_at >= NOW()",
            &cache_keys
        )
        .fetch_all(&self.pg_pool)
        .await?;

        let cache_key_entries: Result<HashMap<String, CacheEntry>, serde_json::Error> = resp
            .into_iter()
            .map(|e| {
                let entry: CacheEntry = e.try_into()?;

                Ok((entry.cache_key.clone(), entry))
            })
            .collect();
        let mut cache_key_entries = cache_key_entries?;

        Ok(cache_keys
            .iter()
            .map(|ck| cache_key_entries.remove(ck))
            .collect())
    }

    /// Deletes all documents that have one (or more) of the keys
    /// Returns the number of deleted documents.
    pub(crate) async fn invalidate_by_subgraphs(
        &self,
        subgraph_names: Vec<String>,
    ) -> anyhow::Result<u64> {
        let rec = sqlx::query!(
            r#"WITH deleted AS
            (DELETE
                FROM cache
                USING invalidation_key
                WHERE invalidation_key.cache_key_id = cache.id  AND invalidation_key.subgraph_name = ANY($1::text[]) RETURNING cache.cache_key, cache.expires_at
            )
        SELECT COUNT(*) AS count FROM deleted WHERE deleted.expires_at >= NOW()"#,
            &subgraph_names
        )
        .fetch_one(&self.pg_pool)
        .await?;

        Ok(rec.count.unwrap_or_default() as u64)
    }

    /// Deletes all documents that have one (or more) of the keys
    /// Returns the number of deleted documents.
    pub(crate) async fn invalidate(
        &self,
        invalidation_keys: Vec<String>,
        subgraph_names: Vec<String>,
    ) -> anyhow::Result<u64> {
        let invalidation_keys: Vec<String> = invalidation_keys
            .iter()
            .map(|ck| self.namespaced(ck))
            .collect();
        let rec = sqlx::query!(
            r#"WITH deleted AS
            (DELETE
                FROM cache
                USING invalidation_key
                WHERE invalidation_key.invalidation_key = ANY($1::text[])
                    AND invalidation_key.cache_key_id = cache.id  AND invalidation_key.subgraph_name = ANY($2::text[]) RETURNING cache.cache_key
            )
        SELECT COUNT(*) AS count FROM deleted"#,
            &invalidation_keys,
            &subgraph_names
        )
        .fetch_one(&self.pg_pool)
        .await?;

        Ok(rec.count.unwrap_or_default() as u64)
    }
}
