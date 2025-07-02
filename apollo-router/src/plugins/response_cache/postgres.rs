use std::collections::HashMap;
use std::time::Duration;

use chrono::TimeDelta;
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
    pub(crate) invalidation_keys: Vec<String>,
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

    #[serde(deserialize_with = "humantime_serde::deserialize", default)]
    #[schemars(with = "Option<String>", default)]
    /// Specifies the interval between cache cleanup operations (e.g., "2 hours", "30mins"). Default: 30mins
    pub(crate) cleanup_interval: Option<Duration>,
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
    cleanup_interval: Option<TimeDelta>,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum PostgresCacheStorageError {
    #[error("invalid configuration: {0}")]
    BadConfiguration(String),
    #[error("postgres error: {0}")]
    PgError(#[from] sqlx::Error),
    #[error("cleanup_interval configuration is out of range: {0}")]
    OutOfRangeError(#[from] chrono::OutOfRangeError),
    #[error("cleanup_interval configuration is incorrect: {0}")]
    WrongCleanupInterval(String),
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
                Ok(Self { pg_pool, batch_size: conf.batch_size, namespace: conf.namespace.clone(), cleanup_interval: conf.cleanup_interval.map(TimeDelta::from_std).transpose()? })
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
                Ok(Self { pg_pool, batch_size: conf.batch_size, namespace: conf.namespace.clone(), cleanup_interval: conf.cleanup_interval.map(TimeDelta::from_std).transpose()? })
            }
        }
    }

    pub(crate) async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!().run(&self.pg_pool).await?;
        Ok(())
    }

    #[cfg(test)]
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
        invalidation_keys: Vec<String>,
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

    pub(crate) async fn update_cron(&self) -> anyhow::Result<()> {
        if let Some(cleanup_interval) = &self.cleanup_interval {
            let cron = Cron::try_from(cleanup_interval)
                .map_err(PostgresCacheStorageError::WrongCleanupInterval)?;
            sqlx::query!("SELECT cron.alter_job((SELECT jobid FROM cron.job WHERE jobname = 'delete-old-cache-entries'), $1)", &cron.0)
                .execute(&self.pg_pool)
                .await?;
        }

        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn get_cron(&self) -> anyhow::Result<Cron> {
        let rec = sqlx::query!(
            "SELECT schedule FROM cron.job WHERE jobname = 'delete-old-cache-entries'"
        )
        .fetch_one(&self.pg_pool)
        .await?;

        Ok(Cron(rec.schedule))
    }
}

#[derive(Debug, sqlx::Type)]
#[sqlx(transparent)]
pub(crate) struct Cron(pub(crate) String);

impl TryFrom<&TimeDelta> for Cron {
    type Error = String;
    fn try_from(value: &TimeDelta) -> Result<Self, Self::Error> {
        let num_days = value.num_days();
        let num_hours = value.num_hours();
        let num_mins = value.num_minutes();
        if num_days > 366 {
            Err(String::from("interval cannot exceed 1 year"))
        } else if num_days > 31 {
            // multiple months
            let months = (num_days / 30).min(12);
            Ok(Cron(format!("0 0 1 */{months} *")))
        } else if num_days > 28 {
            // treat as one month
            Ok(Cron(String::from("0 0 1 * *")))
        } else if num_days > 0 {
            Ok(Cron(format!("0 0 */{num_days} * *")))
        } else if num_hours > 0 {
            Ok(Cron(format!("0 */{num_hours} * * *")))
        } else if num_mins > 0 {
            Ok(Cron(format!("*/{num_mins} * * * *")))
        } else {
            Err(String::from(
                "interval lower than 1 minute is not supported",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minutes_conversion() {
        // Test with 5 minutes
        let delta = TimeDelta::minutes(5);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "*/5 * * * *");

        // Test with 30 minutes
        let delta = TimeDelta::minutes(30);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "*/30 * * * *");

        // Test with 59 minutes
        let delta = TimeDelta::minutes(59);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "*/59 * * * *");
    }

    #[test]
    fn test_hours_conversion() {
        // Test with 1 hour
        let delta = TimeDelta::hours(1);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 */1 * * *");

        // Test with 3 hours
        let delta = TimeDelta::hours(3);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 */3 * * *");

        // Test with 12 hours
        let delta = TimeDelta::hours(12);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 */12 * * *");

        // Test with 23 hours
        let delta = TimeDelta::hours(23);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 */23 * * *");
    }

    #[test]
    fn test_days_conversion() {
        // Test with 1 day
        let delta = TimeDelta::days(1);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */1 * *");

        // Test with 7 days
        let delta = TimeDelta::days(7);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */7 * *");

        // Test with 15 days
        let delta = TimeDelta::days(15);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */15 * *");

        // Test with 28 days (limit)
        let delta = TimeDelta::days(28);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */28 * *");
    }

    #[test]
    fn test_months_conversion() {
        // Test with 29 days (should be treated as monthly)
        let delta = TimeDelta::days(29);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 * *");

        // Test with 30 days (should be treated as monthly)
        let delta = TimeDelta::days(30);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 * *");

        // Test with 56 days (2 months)
        let delta = TimeDelta::days(56);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 */2 *");

        // Test with 84 days (3 months)
        let delta = TimeDelta::days(84);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 */3 *");

        // Test with 168 days (6 months)
        let delta = TimeDelta::days(168);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 */6 *");

        // Test with 336 days (12 months)
        let delta = TimeDelta::days(336);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 */12 *");
    }

    #[test]
    fn test_edge_cases() {
        // Test with exactly 1 minute
        let delta = TimeDelta::minutes(1);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "*/1 * * * *");

        // Test with exactly 1 hour (60 minutes)
        let delta = TimeDelta::minutes(60);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 */1 * * *");

        // Test with exactly 1 jour (24 hours)
        let delta = TimeDelta::hours(24);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */1 * *");
    }

    #[test]
    fn test_error_cases() {
        // Test with 0 minute (must fail)
        let delta = TimeDelta::minutes(0);
        let result = Cron::try_from(&delta);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "interval lower than 1 minute is not supported".to_string()
        );

        // Test with negative intervals
        let delta = TimeDelta::minutes(-5);
        let result = Cron::try_from(&delta);
        assert!(result.is_err());

        // Test with interval bigger than a year
        let delta = TimeDelta::days(400);
        let result = Cron::try_from(&delta);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            "interval bigger than 1 year is not supported".to_string()
        );
    }

    #[test]
    fn test_boundary_values() {
        // Test with 28 days
        let delta = TimeDelta::days(28);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */28 * *");

        // Test with more than 28 days
        let delta = TimeDelta::days(29);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 * *");

        // Test with 12 months
        let delta = TimeDelta::days(336);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 1 */12 *");

        // Test with more than a year
        let delta = TimeDelta::days(370);
        let result = Cron::try_from(&delta);
        assert!(result.is_err());
    }

    #[test]
    fn test_complex_intervals() {
        // It should only take care of hours
        let delta = TimeDelta::hours(1) + TimeDelta::minutes(30);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 */1 * * *");

        // It should only take care of days
        let delta = TimeDelta::days(1) + TimeDelta::hours(12);
        let cron = Cron::try_from(&delta).unwrap();
        assert_eq!(cron.0, "0 0 */1 * *");
    }
}
