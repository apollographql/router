use sqlx::types::Json;
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
    pub data: sqlx::types::JsonValue,
    pub expires_at: DateTime<Utc>,
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
        let rec = sqlx::query!(
            r#"
        INSERT INTO cache ( cache_key, data, expires_at )
        VALUES ( $1, $2, $3 )
        RETURNING id
                "#,
            &self.document_id(document_id),
            Json(value) as _,
            expired_at
        )
        .fetch_one(&mut **tx)
        .await?;

        for invalidation_key in invalidation_keys {
            sqlx::query!(
                r#"INSERT into invalidation_key (cache_key_id, invalidation_key) VALUES ($1, $2)"#,
                rec.id,
                &self.document_id(&invalidation_key)
            )
            .execute(&mut **tx)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    pub async fn get_hash_document(&self, document_id: &str) -> anyhow::Result<CacheEntry> {
        // let conn = self.client.acquire().await?;
        let resp = sqlx::query_as!(
            CacheEntry,
            "SELECT * FROM cache WHERE cache.cache_key = $1 AND expires_at >= NOW()",
            &self.document_id(document_id)
        )
        .fetch_one(&self.client)
        .await?;

        Ok(resp)
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
                    AND invalidation_key.cache_key_id = cache.id RETURNING cache.cache_key
            )
        SELECT COUNT(*) AS count FROM deleted"#,
            &invalidation_keys
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

/// Unfortunately docs are woefully misleading:
///
/// https://redis.io/docs/latest/develop/interact/search-and-query/advanced-concepts/query_syntax/#tag-filters
/// > The following characters in tags should be escaped with a backslash (\): $, {, }, \, and |.
///
/// In testing with Redis 8.0.0, all ASCII punctuation except `_`
/// cause either a syntax error or a search mismatch.
pub fn escape_redisearch_tag_filter(searched_tag: &str) -> std::borrow::Cow<'_, str> {
    // We use Rust raw string syntax to avoid one level of escaping there,
    // but the '\', '-', '[', and ']' are still significant in regex syntax and need to be escaped
    static TO_ESCAPE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r##"[!"#$%&'()*+,\-./:;<=>?@\[\\\]^`{|}~]"##).unwrap()
    });
    TO_ESCAPE.replace_all(searched_tag, r"\$0")
}
