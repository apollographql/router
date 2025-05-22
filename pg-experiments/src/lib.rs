use sqlx::types::chrono::DateTime;
use sqlx::types::chrono::Utc;
use sqlx::{Acquire, Pool, Postgres};
use std::borrow::Cow;
use std::time::Duration;

pub struct Cache {
    pub client: Pool<Postgres>,
    pub config: CacheConfig,
}

pub struct CacheConfig {
    pub namespace: Option<String>,
    pub temporary_seconds: Option<u64>,
    pub index_name: String,
    pub indexed_document_id_prefix: String,
    pub invalidation_keys_field_name: String,
}

#[derive(Debug, Clone, Copy)]
pub enum Expire {
    In { seconds: i64 },
    At { timestamp: i64 },
}

#[derive(Debug, Copy, Clone)]
pub struct AsciiWhitespaceSeparated<'a>(pub &'a str);

impl CacheConfig {
    pub fn random_namespace() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct CacheEntry {
    pub id: i64,
    pub cache_key: String,
    pub data: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CacheEntryJson {
    pub id: i64,
    pub cache_key: String,
    pub data: serde_json::Value,
    pub expires_at: DateTime<Utc>,
}

pub struct BatchDocument {
    pub cache_key: String,
    pub data: String,
    pub invalidation_keys: Vec<String>,
    pub expire: Expire,
}

impl TryFrom<CacheEntry> for CacheEntryJson {
    type Error = serde_json::Error;

    fn try_from(value: CacheEntry) -> Result<Self, Self::Error> {
        let data = serde_json::from_str(&value.data)?;
        Ok(Self {
            id: value.id,
            cache_key: value.cache_key,
            data,
            expires_at: value.expires_at,
        })
    }
}

impl Cache {
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!().run(&self.client).await?;
        Ok(())
    }

    pub async fn truncate(&self) -> anyhow::Result<()> {
        let mut conn = self.client.acquire().await?;
        let mut transaction = conn.begin().await?;
        let tx = &mut transaction;

        sqlx::query!("TRUNCATE TABLE cache CASCADE")
            .execute(&mut **tx)
            .await?;
        sqlx::query!("TRUNCATE TABLE invalidation_key")
            .execute(&mut **tx)
            .await?;

        transaction.commit().await?;

        Ok(())
    }

    pub async fn insert_hash_document(
        &self,
        document_id: &str,
        expire: Expire,
        invalidation_keys: Vec<String>,
        value: serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut conn = self.client.acquire().await?;
        let mut transaction = conn.begin().await?;
        let tx = &mut transaction;

        let expired_at = match expire {
            Expire::In { seconds } => Utc::now() + Duration::from_secs(seconds as u64),
            Expire::At { timestamp } => DateTime::from_timestamp(timestamp, 0).unwrap(),
        };
        let value_str = serde_json::to_string(&value)?;
        let rec = sqlx::query!(
            r#"
        INSERT INTO cache ( cache_key, data, expires_at )
        VALUES ( $1, $2, $3 )
        RETURNING id
                "#,
            &self.document_id(document_id),
            value_str,
            expired_at
        )
        .fetch_one(&mut **tx)
        .await?;

        for invalidation_key in invalidation_keys {
            sqlx::query!(
                r#"INSERT into invalidation_key (cache_key_id, invalidation_key, subgraph_name) VALUES ($1, $2, $3)"#,
                rec.id,
                &self.document_id(&invalidation_key),
                "xxx"
            )
            .execute(&mut **tx)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub async fn insert_hash_document_in_batch(
        &self,
        batch_docs: Vec<BatchDocument>,
    ) -> anyhow::Result<()> {
        let mut conn = self.client.acquire().await?;

        let batch_docs = batch_docs.chunks(100);
        for batch_docs in batch_docs {
            let mut transaction = conn.begin().await?;
            let tx = &mut transaction;
            let cache_keys = batch_docs
                .iter()
                .map(|b| b.cache_key.clone())
                .collect::<Vec<String>>();

            let data = batch_docs
                .iter()
                .map(|b| b.data.clone())
                .collect::<Vec<String>>();
            let expires = batch_docs
                .iter()
                .map(|b| match b.expire {
                    Expire::In { seconds } => Utc::now() + Duration::from_secs(seconds as u64),
                    Expire::At { timestamp } => DateTime::from_timestamp(timestamp, 0).unwrap(),
                })
                .collect::<Vec<DateTime<Utc>>>();
            let resp = sqlx::query!(
                r#"
                INSERT INTO cache
                ( cache_key, data, expires_at ) SELECT * FROM UNNEST(
                    $1::VARCHAR(1024)[],
                    $2::TEXT[],
                    $3::TIMESTAMP WITH TIME ZONE[]
                ) RETURNING id
                "#,
                &cache_keys,
                &data,
                &expires,
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

            let subgraph_names: Vec<String> = invalidation_keys
                .iter()
                .map(|_| "xxx".to_string())
                .collect();
            let invalidation_keys: Vec<String> = invalidation_keys
                .into_iter()
                .map(|(_, invalidation_key)| invalidation_key)
                .collect();
            sqlx::query!(
                r#"
                INSERT INTO invalidation_key (cache_key_id, invalidation_key, subgraph_name)
                SELECT * FROM UNNEST(
                    $1::BIGINT[],
                    $2::VARCHAR(255)[],
                    $3::VARCHAR(255)[]
                )
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

    pub async fn get_hash_document(&self, document_id: &str) -> anyhow::Result<CacheEntryJson> {
        let resp = sqlx::query_as!(
            CacheEntry,
            "SELECT * FROM cache WHERE cache.cache_key = $1 AND expires_at >= NOW()",
            &self.document_id(document_id)
        )
        .fetch_one(&self.client)
        .await?;

        let cache_entry_json = resp.try_into()?;

        Ok(cache_entry_json)
    }

    /// Deletes all documents that have one (or more) of the keys
    /// in `ascii_whitespace_separated_invalidation_keys`.
    ///
    /// Returns the number of deleted documents.
    pub async fn invalidate(&self, invalidation_keys: Vec<String>) -> anyhow::Result<u64> {
        let rec = sqlx::query!(
            r#"WITH deleted AS
            (DELETE
                FROM cache
                USING invalidation_key
                WHERE invalidation_key.invalidation_key = ANY($1::text[])
                    AND invalidation_key.cache_key_id = cache.id  AND invalidation_key.subgraph_name = $2 RETURNING cache.cache_key
            )
        SELECT COUNT(*) AS count FROM deleted"#,
            &invalidation_keys,
            "xxx"
        )
        .fetch_one(&self.client)
        .await?;

        Ok(rec.count.unwrap_or_default() as u64)
    }

    fn document_id<'a>(&self, id: &'a str) -> Cow<'a, str> {
        if let Some(ns) = &self.config.namespace {
            format!("{ns}:{id}").into()
        } else {
            id.into()
        }
    }
}
