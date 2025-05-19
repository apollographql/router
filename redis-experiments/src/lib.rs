use std::borrow::Cow;
use std::collections::HashMap;

use fred::bytes::Bytes;
use fred::bytes_utils::string::StrInner;
use fred::prelude::*;
use fred::types::MultipleKeys;
use fred::types::RespVersion;
use fred::types::cluster::ClusterRouting;
use fred::types::redisearch::FtCreateOptions;
use fred::types::redisearch::FtSearchOptions;
use fred::types::redisearch::IndexKind;
use fred::types::redisearch::SearchField;
use fred::types::redisearch::SearchSchema;
use fred::types::redisearch::SearchSchemaKind;

const INVALIDATION_KEY_SEPARATOR: char = ' ';
const INVALIDATION_BATCH_SIZE: i64 = 1_000;

pub struct Cache {
    pub client: Client,
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

impl Cache {
    /// Wraps the `FT.CREATE` command, ignoring "Index already exists" errors
    pub async fn create_index_if_not_exists(&self) -> FredResult<()> {
        let result = self.create_index().await;
        if let Err(err) = &result {
            if err.details().contains("already exists") {
                return Ok(());
            }
        }
        result
    }

    /// Wraps the `FT.CREATE` command
    pub async fn create_index(&self) -> FredResult<()> {
        self.client
            .ft_create(
                self.index_name(),
                FtCreateOptions {
                    on: Some(IndexKind::Hash),
                    prefixes: vec![
                        self.document_id(&self.config.indexed_document_id_prefix)
                            .into(),
                    ],
                    // temporary: self.config.temporary_seconds,
                    ..Default::default()
                },
                vec![SearchSchema {
                    field_name: self.config.invalidation_keys_field_name.as_str().into(),
                    alias: None,
                    kind: SearchSchemaKind::Tag {
                        sortable: false,
                        unf: false,
                        separator: Some(INVALIDATION_KEY_SEPARATOR),
                        casesensitive: true,
                        withsuffixtrie: false,
                        noindex: false,
                    },
                }],
            )
            .await
    }

    /// Wraps the `FT.DROPINDEX` command
    pub async fn drop_index(&self, drop_indexed_documents: bool) -> FredResult<()> {
        self.client
            .ft_dropindex(self.index_name(), drop_indexed_documents)
            .await
    }

    /// Wraps the `HSET` command, adding an indexed field for invalidation keys
    ///
    /// If `document_id` already exists and is a hash document, new values are "merged" with it
    /// (existing fields not specified here are not removed).
    pub async fn insert_hash_document<MapKey, MapValue>(
        &self,
        document_id: &str,
        expire: Expire,
        invalidation_keys: AsciiWhitespaceSeparated<'_>,
        values: impl IntoIterator<Item = (MapKey, MapValue)>,
    ) -> FredResult<()>
    where
        MapKey: Into<Key>,
        MapValue: Into<Value>,
    {
        let mut invalidation_keys = invalidation_keys.0.split_ascii_whitespace();
        let invalidation_keys: Option<String> = invalidation_keys.next().map(|first| {
            let mut separated = first.to_owned();
            for next in invalidation_keys {
                separated.push(INVALIDATION_KEY_SEPARATOR);
                separated.push_str(next);
            }
            // Looks like "{key1}{INVALIDATION_KEY_SEPARATOR}{key2}"
            separated
        });
        let maybe_invalidation_keys_field = invalidation_keys.map(|v| {
            let k = Key::from(&self.config.invalidation_keys_field_name);
            (k, v.into())
        });
        let map = fred::types::Map::from_iter(
            values
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .chain(maybe_invalidation_keys_field),
        );
        let id = self.document_id(document_id);
        let _: () = self.client.hset(id.as_ref(), map).await?;
        match expire {
            Expire::In { seconds } => self.client.expire(id.as_ref(), seconds, None).await,
            Expire::At { timestamp } => self.client.expire_at(id.as_ref(), timestamp, None).await,
        }
    }

    /// Wraps the `HMGET` command
    pub async fn get_hash_document<R: FromValue>(
        &self,
        document_id: &str,
        fields: impl Into<MultipleKeys> + Send,
    ) -> FredResult<R> {
        self.client
            .hmget(self.document_id(document_id).as_ref(), fields)
            .await
    }

    /// Deletes all documents that have one (or more) of the keys
    /// in `ascii_whitespace_separated_invalidation_keys`.
    ///
    /// Returns the number of deleted documents.
    pub async fn invalidate(
        &self,
        invalidation_keys: AsciiWhitespaceSeparated<'_>,
    ) -> FredResult<u64> {
        let mut count = 0;
        let mut invalidation_keys = invalidation_keys.0.split_ascii_whitespace();
        let Some(first) = invalidation_keys.next() else {
            return Ok(count);
        };
        let mut query = String::new();
        query.push('@');
        query.push_str(&self.config.invalidation_keys_field_name);
        query.push_str(":{");
        query.push_str(&escape_redisearch_tag_filter(first));
        for key in invalidation_keys {
            query.push('|');
            query.push_str(&escape_redisearch_tag_filter(key));
        }
        query.push('}');

        // We want `NOCONTENT` but it’s apparently not supported on AWS:
        // TODO: test this, they also don’t document `DIALECT 2` but give an example with it.
        // https://docs.aws.amazon.com/memorydb/latest/devguide/vector-search-commands-ft.search.html
        //
        // A work-around is `RETURN 0` (a empty list of zero fields) but it’s not supported by Fred:
        // https://github.com/aembke/fred.rs/issues/345
        //
        // As a work-around for a the work-around, we request a single field that we know exists
        let mut options = FtSearchOptions {
            // nocontent: true,
            // r#return: vec![],
            r#return: vec![SearchField {
                identifier: self.config.invalidation_keys_field_name.as_str().into(),
                property: None,
            }],

            limit: Some((0, INVALIDATION_BATCH_SIZE)),
            ..Default::default()
        };

        // https://redis.io/docs/latest/develop/reference/protocol-spec/#resp-versions
        // > Future versions of Redis may change the default protocol version
        //
        // The result of FT.SEARCH is a map in RESP3 v.s. an array in RESP2.
        assert_eq!(self.client.protocol_version(), RespVersion::RESP2);
        loop {
            let search_result = self
                .client
                .ft_search(self.index_name(), &query, options.clone())
                .await?;
            let Value::Array(array) = search_result else {
                return Err(Error::new(
                    ErrorKind::Parse,
                    "Expected an array from FT.SEARCH",
                ));
            };
            let mut iter = array.into_iter();
            let _count = iter.next();
            if iter.len() == 0 {
                return Ok(count);
            }
            let mut keys_to_delete = Vec::with_capacity(iter.len() / 2);
            while let Some(id_value) = iter.next() {
                let Value::String(id_string) = id_value else {
                    return Err(Error::new(
                        ErrorKind::Parse,
                        "Expected a string for document ID from FT.SEARCH",
                    ));
                };
                keys_to_delete.push(id_string);
                let _values = iter.next();
            }

            let deleted: u64 = self.delete_cluster(keys_to_delete).await?;
            count += deleted;
        }
    }

    fn document_id<'a>(&self, id: &'a str) -> Cow<'a, str> {
        if let Some(ns) = &self.config.namespace {
            format!("{ns}:{id}").into()
        } else {
            id.into()
        }
    }

    fn index_name(&self) -> Cow<'_, str> {
        if let Some(ns) = &self.config.namespace {
            format!("{ns}:{}", self.config.index_name).into()
        } else {
            self.config.index_name.as_str().into()
        }
    }

    async fn delete_cluster(&self, keys: Vec<StrInner<Bytes>>) -> FredResult<u64> {
        let mut h: HashMap<u16, Vec<StrInner<Bytes>>> = HashMap::new();
        for key in keys.into_iter() {
            let hash = ClusterRouting::hash_key(key.as_bytes());
            let entry = h.entry(hash).or_default();
            entry.push(key);
        }

        // then we query all the key groups at the same time
        let results: Vec<Result<u64, fred::error::Error>> =
            futures::future::join_all(h.into_values().map(|keys| self.client.del(keys))).await;
        let mut total = 0u64;

        for res in results {
            total += res?;
        }

        Ok(total)
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
