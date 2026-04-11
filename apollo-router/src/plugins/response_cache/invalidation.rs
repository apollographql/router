use std::collections::HashSet;
use std::sync::Arc;

use futures::FutureExt;
use futures::StreamExt;
use futures::stream;
use itertools::Itertools;
use opentelemetry::StringValue;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tower::BoxError;
use tracing::Instrument;

use super::plugin::StorageInterface;
use crate::plugins::response_cache::ErrorCode;
use crate::plugins::response_cache::plugin::INTERNAL_CACHE_TAG_PREFIX;
use crate::plugins::response_cache::plugin::RESPONSE_CACHE_VERSION;
use crate::plugins::response_cache::storage;
use crate::plugins::response_cache::storage::CacheStorage;
use crate::plugins::response_cache::storage::redis::Storage;

#[derive(Clone)]
pub(crate) struct Invalidation {
    pub(crate) storage: Arc<StorageInterface>,
}

#[derive(Error, Debug)]
pub(super) enum InvalidationError {
    #[error("error")]
    Misc(#[from] anyhow::Error),
    #[error("caching database error")]
    Storage(#[from] storage::Error),
    #[error("several errors")]
    Errors(#[from] InvalidationErrors),
}

impl ErrorCode for InvalidationError {
    fn code(&self) -> &'static str {
        match &self {
            InvalidationError::Misc(_) => "MISC",
            InvalidationError::Storage(error) => error.code(),
            InvalidationError::Errors(_) => "INVALIDATION_ERRORS",
        }
    }
}

#[derive(Debug)]
pub(crate) struct InvalidationErrors(Vec<InvalidationError>);

impl std::fmt::Display for InvalidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalidation errors: [{}]",
            self.0.iter().map(|e| e.to_string()).join("; ")
        )
    }
}

impl std::error::Error for InvalidationErrors {}

impl Invalidation {
    pub(crate) async fn new(storage: Arc<StorageInterface>) -> Result<Self, BoxError> {
        Ok(Self { storage })
    }

    pub(crate) async fn invalidate(
        &self,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, BoxError> {
        u64_counter_with_unit!(
            "apollo.router.operations.response_cache.invalidation.event",
            "Response cache received a batch of invalidation requests",
            "{request}",
            1u64
        );

        Ok(self
            .handle_request_batch(requests)
            .instrument(tracing::info_span!("cache.invalidation.batch"))
            .await?)
    }

    async fn handle_request(
        &self,
        storage: &Storage,
        request: &InvalidationRequest,
    ) -> Result<u64, InvalidationError> {
        let invalidation_key = request.invalidation_key();
        tracing::debug!(
            "got invalidation request: {request:?}, will invalidate: {}",
            invalidation_key
        );
        let (count, subgraphs) = match request {
            InvalidationRequest::Subgraph { subgraph } => {
                let count = storage
                    .invalidate_by_subgraph(subgraph, request.kind())
                    .await
                    .inspect_err(|err| {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "subgraph",
                            "subgraph.name" = subgraph.clone()
                        );
                    })?;
                u64_counter_with_unit!(
                    "apollo.router.operations.response_cache.invalidation.entry",
                    "Response cache counter for invalidated entries",
                    "{entry}",
                    count,
                    "kind" = "subgraph",
                    "subgraph.name" = subgraph.clone()
                );
                (count, vec![subgraph.clone()])
            }
            InvalidationRequest::Type {
                subgraph,
                r#type: graphql_type,
            } => {
                let subgraph_counts = storage
                    .invalidate(
                        vec![invalidation_key],
                        vec![subgraph.clone()],
                        request.kind(),
                    )
                    .await
                    .inspect_err(|err| {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "type",
                            "subgraph.name" = subgraph.clone(),
                            "graphql.type" = graphql_type.clone()
                        );
                    })?;
                let mut total_count = 0;
                for (subgraph_name, count) in subgraph_counts {
                    total_count += count;
                    u64_counter_with_unit!(
                        "apollo.router.operations.response_cache.invalidation.entry",
                        "Response cache counter for invalidated entries",
                        "{entry}",
                        count,
                        "kind" = "type",
                        "subgraph.name" = subgraph_name,
                        "graphql.type" = graphql_type.clone()
                    );
                }

                (total_count, vec![subgraph.clone()])
            }
            InvalidationRequest::CacheTag {
                subgraphs,
                cache_tag,
            } => {
                let subgraph_counts = storage
                    .invalidate(
                        vec![cache_tag.clone()],
                        subgraphs.clone().into_iter().collect(),
                        request.kind(),
                    )
                    .await
                    .inspect_err(|err| {
                        let subgraphs: opentelemetry::Array = subgraphs
                            .clone()
                            .into_iter()
                            .map(StringValue::from)
                            .collect::<Vec<StringValue>>()
                            .into();
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "cache_tag",
                            "subgraph.names" = opentelemetry::Value::Array(subgraphs),
                            "cache.tag" = cache_tag.clone()
                        );
                    })?;
                let mut total_count = 0;
                for (subgraph_name, count) in subgraph_counts {
                    total_count += count;
                    u64_counter_with_unit!(
                        "apollo.router.operations.response_cache.invalidation.entry",
                        "Response cache counter for invalidated entries",
                        "{entry}",
                        count,
                        "kind" = "cache_tag",
                        "subgraph.name" = subgraph_name,
                        "cache.tag" = cache_tag.clone()
                    );
                }

                (
                    total_count,
                    subgraphs.clone().into_iter().collect::<Vec<String>>(),
                )
            }
            InvalidationRequest::ConnectorSource { source } => {
                let count = storage
                    .invalidate_by_subgraph(source, request.kind())
                    .await
                    .inspect_err(|err| {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "connector",
                            "connector.source" = source.clone()
                        );
                    })?;
                u64_counter_with_unit!(
                    "apollo.router.operations.response_cache.invalidation.entry",
                    "Response cache counter for invalidated entries",
                    "{entry}",
                    count,
                    "kind" = "connector",
                    "connector.source" = source.clone()
                );
                (count, vec![source.clone()])
            }
            InvalidationRequest::ConnectorType {
                source,
                r#type: graphql_type,
            } => {
                let source_counts = storage
                    .invalidate(vec![invalidation_key], vec![source.clone()], request.kind())
                    .await
                    .inspect_err(|err| {
                        u64_counter_with_unit!(
                            "apollo.router.operations.response_cache.invalidation.error",
                            "Errors when invalidating data in cache",
                            "{error}",
                            1,
                            "code" = err.code(),
                            "kind" = "type",
                            "connector.source" = source.clone(),
                            "graphql.type" = graphql_type.clone()
                        );
                    })?;
                let mut total_count = 0;
                for (source_name, count) in source_counts {
                    total_count += count;
                    u64_counter_with_unit!(
                        "apollo.router.operations.response_cache.invalidation.entry",
                        "Response cache counter for invalidated entries",
                        "{entry}",
                        count,
                        "kind" = "type",
                        "connector.source" = source_name,
                        "graphql.type" = graphql_type.clone()
                    );
                }

                (total_count, vec![source.clone()])
            }
        };

        for subgraph in subgraphs {
            u64_histogram_with_unit!(
                "apollo.router.operations.response_cache.invalidation.request.entry",
                "Number of invalidated entries per invalidation request.",
                "{entry}",
                count,
                "subgraph.name" = subgraph
            );
        }

        Ok(count)
    }

    async fn handle_request_batch(
        &self,
        requests: Vec<InvalidationRequest>,
    ) -> Result<u64, InvalidationError> {
        let mut count = 0;
        let mut errors = Vec::new();
        let mut futures = Vec::new();
        for request in requests {
            let storages = match &request {
                InvalidationRequest::Subgraph { subgraph }
                | InvalidationRequest::Type { subgraph, .. } => match self.storage.get(subgraph) {
                    Some(s) => vec![s],
                    None => continue,
                },
                InvalidationRequest::CacheTag { subgraphs, .. } => subgraphs
                    .iter()
                    .filter_map(|subgraph| {
                        self.storage
                            .get(subgraph)
                            .or_else(|| self.storage.get_connector(subgraph))
                    })
                    .collect(),
                InvalidationRequest::ConnectorSource { source }
                | InvalidationRequest::ConnectorType { source, .. } => {
                    match self.storage.get_connector(source) {
                        Some(s) => vec![s],
                        None => continue,
                    }
                }
            };

            for storage in storages {
                let request = request.clone();
                let f = async move {
                    self.handle_request(storage, &request)
                        .instrument(tracing::info_span!("cache.invalidation.request"))
                        .await
                };
                futures.push(f.boxed());
            }
        }
        let mut stream: stream::FuturesUnordered<_> = futures.into_iter().collect();
        while let Some(res) = stream.next().await {
            match res {
                Ok(c) => count += c,
                Err(err) => {
                    errors.push(err);
                }
            }
        }

        if !errors.is_empty() {
            Err(InvalidationErrors(errors).into())
        } else {
            Ok(count)
        }
    }
}

pub(super) type InvalidationKind = &'static str;

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum InvalidationRequest {
    Subgraph {
        subgraph: String,
    },
    Type {
        subgraph: String,
        r#type: String,
    },
    CacheTag {
        subgraphs: HashSet<String>,
        cache_tag: String,
    },
    /// Invalidate all cached entries for a connector source
    #[serde(rename = "connector")]
    ConnectorSource {
        /// Connector source identifier in "subgraph_name.source_name" format
        source: String,
    },
    /// Invalidate all cached entries of a specific type for a connector source
    ConnectorType {
        /// Connector source identifier in "subgraph_name.source_name" format
        source: String,
        r#type: String,
    },
}

/// Intermediate struct for custom deserialization of `InvalidationRequest`.
/// Allows `"kind": "type"` to dispatch to either `Type` or `ConnectorType`
/// based on whether `"subgraph"` or `"source"` is present.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInvalidationRequest {
    kind: String,
    #[serde(default)]
    subgraph: Option<String>,
    #[serde(default)]
    subgraphs: Option<HashSet<String>>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    cache_tag: Option<String>,
    #[serde(default)]
    sources: Option<HashSet<String>>,
}

impl<'de> Deserialize<'de> for InvalidationRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let raw = RawInvalidationRequest::deserialize(deserializer)?;

        // Helper to reject unexpected fields
        fn reject_field<E: serde::de::Error>(
            field: &str,
            present: bool,
            kind: &str,
        ) -> Result<(), E> {
            if present {
                Err(E::custom(format!(
                    "unexpected field `{field}` for kind `{kind}`"
                )))
            } else {
                Ok(())
            }
        }

        let kind = raw.kind.as_str();

        match kind {
            "subgraph" => {
                let subgraph = raw
                    .subgraph
                    .ok_or_else(|| D::Error::missing_field("subgraph"))?;
                reject_field::<D::Error>("source", raw.source.is_some(), kind)?;
                reject_field::<D::Error>("type", raw.r#type.is_some(), kind)?;
                reject_field::<D::Error>("subgraphs", raw.subgraphs.is_some(), kind)?;
                reject_field::<D::Error>("cache_tag", raw.cache_tag.is_some(), kind)?;
                reject_field::<D::Error>("sources", raw.sources.is_some(), kind)?;
                Ok(InvalidationRequest::Subgraph { subgraph })
            }
            "type" => {
                let r#type = raw.r#type.ok_or_else(|| D::Error::missing_field("type"))?;
                reject_field::<D::Error>("subgraphs", raw.subgraphs.is_some(), kind)?;
                reject_field::<D::Error>("cache_tag", raw.cache_tag.is_some(), kind)?;
                reject_field::<D::Error>("sources", raw.sources.is_some(), kind)?;
                match (raw.subgraph, raw.source) {
                    (Some(subgraph), None) => Ok(InvalidationRequest::Type { subgraph, r#type }),
                    (None, Some(source)) => {
                        Ok(InvalidationRequest::ConnectorType { source, r#type })
                    }
                    (Some(_), Some(_)) => Err(D::Error::custom(
                        "cannot specify both `subgraph` and `source` for kind `type`",
                    )),
                    (None, None) => Err(D::Error::custom(
                        "kind `type` requires either `subgraph` or `source` field",
                    )),
                }
            }
            "connector" => {
                let source = raw
                    .source
                    .ok_or_else(|| D::Error::missing_field("source"))?;
                reject_field::<D::Error>("subgraph", raw.subgraph.is_some(), kind)?;
                reject_field::<D::Error>("type", raw.r#type.is_some(), kind)?;
                reject_field::<D::Error>("subgraphs", raw.subgraphs.is_some(), kind)?;
                reject_field::<D::Error>("cache_tag", raw.cache_tag.is_some(), kind)?;
                reject_field::<D::Error>("sources", raw.sources.is_some(), kind)?;
                Ok(InvalidationRequest::ConnectorSource { source })
            }
            "cache_tag" => {
                let subgraphs = match (raw.subgraphs, raw.sources) {
                    (Some(subgraphs), None) => subgraphs,
                    (None, Some(sources)) => sources,
                    (Some(_), Some(_)) => {
                        return Err(D::Error::custom(
                            "cannot specify both `subgraphs` and `sources` for kind `cache_tag`",
                        ));
                    }
                    (None, None) => {
                        return Err(D::Error::custom(
                            "kind `cache_tag` requires either `subgraphs` or `sources` field",
                        ));
                    }
                };
                let cache_tag = raw
                    .cache_tag
                    .ok_or_else(|| D::Error::missing_field("cache_tag"))?;
                reject_field::<D::Error>("subgraph", raw.subgraph.is_some(), kind)?;
                reject_field::<D::Error>("source", raw.source.is_some(), kind)?;
                reject_field::<D::Error>("type", raw.r#type.is_some(), kind)?;
                Ok(InvalidationRequest::CacheTag {
                    subgraphs,
                    cache_tag,
                })
            }
            other => Err(D::Error::unknown_variant(
                other,
                &["subgraph", "type", "connector", "cache_tag"],
            )),
        }
    }
}

impl InvalidationRequest {
    pub(crate) fn subgraph_names(&self) -> Vec<String> {
        match self {
            InvalidationRequest::Subgraph { subgraph }
            | InvalidationRequest::Type { subgraph, .. } => vec![subgraph.clone()],
            InvalidationRequest::CacheTag { subgraphs, .. } => {
                subgraphs.clone().into_iter().collect()
            }
            InvalidationRequest::ConnectorSource { source }
            | InvalidationRequest::ConnectorType { source, .. } => vec![source.clone()],
        }
    }

    fn invalidation_key(&self) -> String {
        match self {
            InvalidationRequest::Subgraph { subgraph } => {
                format!("version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph}",)
            }
            InvalidationRequest::Type { subgraph, r#type } => {
                format!(
                    "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:subgraph:{subgraph}:type:{type}",
                )
            }
            InvalidationRequest::CacheTag { cache_tag, .. } => cache_tag.clone(),
            InvalidationRequest::ConnectorSource { source } => {
                format!("version:{RESPONSE_CACHE_VERSION}:connector:{source}")
            }
            InvalidationRequest::ConnectorType { source, r#type } => {
                format!(
                    "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:connector:{source}:type:{type}",
                )
            }
        }
    }

    /// Returns whether this request targets connector storage (vs subgraph storage)
    pub(super) fn is_connector(&self) -> bool {
        matches!(
            self,
            InvalidationRequest::ConnectorSource { .. } | InvalidationRequest::ConnectorType { .. }
        )
    }

    pub(super) fn kind(&self) -> InvalidationKind {
        match self {
            InvalidationRequest::Subgraph { .. } => "subgraph",
            InvalidationRequest::Type { .. } => "type",
            InvalidationRequest::CacheTag { .. } => "cache_tag",
            InvalidationRequest::ConnectorSource { .. } => "connector",
            InvalidationRequest::ConnectorType { .. } => "type",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_source_invalidation_key_format() {
        let req = InvalidationRequest::ConnectorSource {
            source: "mysubgraph.my_api".to_string(),
        };
        let key = req.invalidation_key();
        assert_eq!(
            key,
            format!("version:{RESPONSE_CACHE_VERSION}:connector:mysubgraph.my_api")
        );
    }

    #[test]
    fn connector_type_invalidation_key_format() {
        let req = InvalidationRequest::ConnectorType {
            source: "mysubgraph.my_api".to_string(),
            r#type: "User".to_string(),
        };
        let key = req.invalidation_key();
        assert_eq!(
            key,
            format!(
                "{INTERNAL_CACHE_TAG_PREFIX}version:{RESPONSE_CACHE_VERSION}:connector:mysubgraph.my_api:type:User"
            )
        );
    }

    #[test]
    fn connector_source_is_connector() {
        assert!(
            InvalidationRequest::ConnectorSource {
                source: "s".to_string()
            }
            .is_connector()
        );
        assert!(
            InvalidationRequest::ConnectorType {
                source: "s".to_string(),
                r#type: "T".to_string()
            }
            .is_connector()
        );
    }

    #[test]
    fn subgraph_requests_are_not_connector() {
        assert!(
            !InvalidationRequest::Subgraph {
                subgraph: "s".to_string()
            }
            .is_connector()
        );
        assert!(
            !InvalidationRequest::Type {
                subgraph: "s".to_string(),
                r#type: "T".to_string()
            }
            .is_connector()
        );
        assert!(
            !InvalidationRequest::CacheTag {
                subgraphs: HashSet::new(),
                cache_tag: "tag".to_string()
            }
            .is_connector()
        );
    }

    #[test]
    fn connector_source_kind() {
        assert_eq!(
            InvalidationRequest::ConnectorSource {
                source: "s".to_string()
            }
            .kind(),
            "connector"
        );
    }

    #[test]
    fn connector_type_kind() {
        assert_eq!(
            InvalidationRequest::ConnectorType {
                source: "s".to_string(),
                r#type: "T".to_string()
            }
            .kind(),
            "type"
        );
    }

    #[test]
    fn connector_source_subgraph_names() {
        let req = InvalidationRequest::ConnectorSource {
            source: "mysubgraph.my_api".to_string(),
        };
        assert_eq!(req.subgraph_names(), vec!["mysubgraph.my_api"]);
    }

    #[test]
    fn connector_type_subgraph_names() {
        let req = InvalidationRequest::ConnectorType {
            source: "mysubgraph.my_api".to_string(),
            r#type: "User".to_string(),
        };
        assert_eq!(req.subgraph_names(), vec!["mysubgraph.my_api"]);
    }

    #[test]
    fn deserialize_type_with_source_gives_connector_type() {
        let json = r#"{"kind":"type","source":"graph.api","type":"User"}"#;
        let req: InvalidationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            InvalidationRequest::ConnectorType {
                source: "graph.api".to_string(),
                r#type: "User".to_string()
            }
        );
    }

    #[test]
    fn deserialize_type_with_subgraph_gives_type() {
        let json = r#"{"kind":"type","subgraph":"products","type":"Product"}"#;
        let req: InvalidationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            InvalidationRequest::Type {
                subgraph: "products".to_string(),
                r#type: "Product".to_string()
            }
        );
    }

    #[test]
    fn deserialize_connector_with_source_gives_connector_source() {
        let json = r#"{"kind":"connector","source":"graph.api"}"#;
        let req: InvalidationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            InvalidationRequest::ConnectorSource {
                source: "graph.api".to_string()
            }
        );
    }

    #[test]
    fn deserialize_subgraph_gives_subgraph() {
        let json = r#"{"kind":"subgraph","subgraph":"products"}"#;
        let req: InvalidationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            InvalidationRequest::Subgraph {
                subgraph: "products".to_string()
            }
        );
    }

    #[test]
    fn deserialize_type_with_both_subgraph_and_source_errors() {
        let json = r#"{"kind":"type","subgraph":"x","source":"y","type":"T"}"#;
        let result: Result<InvalidationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot specify both")
        );
    }

    #[test]
    fn deserialize_type_without_subgraph_or_source_errors() {
        let json = r#"{"kind":"type","type":"T"}"#;
        let result: Result<InvalidationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires either"));
    }

    #[test]
    fn deserialize_unknown_field_rejected() {
        let json = r#"{"kind":"type","subgraph":"x","type":"T","extra":"bad"}"#;
        let result: Result<InvalidationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn deserialize_unknown_kind_rejected() {
        let json = r#"{"kind":"bogus","subgraph":"x"}"#;
        let result: Result<InvalidationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown variant"));
    }

    #[test]
    fn deserialize_cache_tag_with_sources_field() {
        let json = r#"{"kind":"cache_tag","sources":["connector-graph.random_person_api"],"cache_tag":"test-1"}"#;
        let req: InvalidationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            InvalidationRequest::CacheTag {
                subgraphs: HashSet::from(["connector-graph.random_person_api".to_string()]),
                cache_tag: "test-1".to_string(),
            }
        );
    }

    #[test]
    fn deserialize_cache_tag_with_subgraphs_field() {
        let json = r#"{"kind":"cache_tag","subgraphs":["my-subgraph"],"cache_tag":"test-1"}"#;
        let req: InvalidationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req,
            InvalidationRequest::CacheTag {
                subgraphs: HashSet::from(["my-subgraph".to_string()]),
                cache_tag: "test-1".to_string(),
            }
        );
    }

    #[test]
    fn deserialize_cache_tag_with_both_subgraphs_and_sources_rejected() {
        let json =
            r#"{"kind":"cache_tag","subgraphs":["foo"],"sources":["bar"],"cache_tag":"test-1"}"#;
        let result: Result<InvalidationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot specify both")
        );
    }

    #[test]
    fn deserialize_cache_tag_with_neither_subgraphs_nor_sources_rejected() {
        let json = r#"{"kind":"cache_tag","cache_tag":"test-1"}"#;
        let result: Result<InvalidationRequest, _> = serde_json::from_str(json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("requires either"));
    }
}
