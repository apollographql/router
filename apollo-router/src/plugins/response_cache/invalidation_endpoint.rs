use std::sync::Arc;
use std::task::Poll;

use bytes::Buf;
use futures::future::BoxFuture;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use http::header::AUTHORIZATION;
use http::header::CONTENT_TYPE;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json_bytes::json;
use tower::BoxError;
use tower::Service;
use tracing::Span;
use tracing_futures::Instrument;

use super::invalidation::Invalidation;
use super::plugin::Subgraph;
use crate::ListenAddr;
use crate::configuration::subgraph::SubgraphConfiguration;
use crate::graphql;
use crate::plugins::response_cache::invalidation::InvalidationRequest;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_ERROR;
use crate::plugins::telemetry::consts::OTEL_STATUS_CODE_OK;
use crate::services::router;

pub(crate) const INVALIDATION_ENDPOINT_SPAN_NAME: &str = "invalidation_endpoint";

/// Identifies one of the three invalidation index modes maintained by `response_cache`. Used
/// internally to gate which Redis ZSET writes the plugin performs and to map an incoming
/// `/invalidation` request kind to the index that backs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IndexMode {
    /// The `subgraph-{name}` ZSET. Backs `By subgraph` invalidation requests.
    Subgraph,
    /// The by-type ZSET keyed by `subgraph:{name}:type:{type}`. Backs `By type` invalidation requests.
    Type,
    /// The ZSETs for user-supplied cache tags from `apolloCacheTags`, `apolloEntityCacheTags`,
    /// and resolved `@cacheTag` directive values. Backs `By cache tag` invalidation requests.
    CacheTag,
}

/// Which invalidation indexes a subgraph maintains for its cached entries. Each field controls
/// whether the corresponding ZSET is written on cache inserts and whether the corresponding
/// invalidation request kind is honored at the `/invalidation` endpoint.
///
/// All three indexes are enabled by default. Operators with workloads that only ever invalidate
/// by a subset of modes can opt out of the unused indexes to reduce per-insert Redis work.
///
/// Note: `indexes` is **additive only**. Enabling a previously-disabled index does NOT
/// retroactively index existing cache entries. If a deployment changes from
/// `{cache_tag: true}` to `{cache_tag: true, subgraph: true}`, the `subgraph-{name}` ZSET will
/// be populated only for entries written after the config change. Pre-change entries are
/// invisible to `By subgraph` invalidation requests until they age out via TTL. To bring the
/// new index online over the full cache set, flush Redis before enabling the additional index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct InvalidationIndexes {
    /// Maintain the `subgraph-{name}` index. Required for `By subgraph` invalidation requests.
    pub(crate) subgraph: bool,
    /// Maintain the by-type index. Required for `By type` invalidation requests.
    #[serde(rename = "type")]
    pub(crate) r#type: bool,
    /// Maintain the per-tag indexes for `apolloCacheTags`, `apolloEntityCacheTags`, and resolved
    /// `@cacheTag` directive values. Required for `By cache tag` invalidation requests.
    pub(crate) cache_tag: bool,
}

impl Default for InvalidationIndexes {
    fn default() -> Self {
        // Defaults to all three indexes enabled for backward compatibility with deployments
        // predating the `indexes` setting. Operators opt out by setting individual fields to
        // `false`.
        Self {
            subgraph: true,
            r#type: true,
            cache_tag: true,
        }
    }
}

impl InvalidationIndexes {
    /// True if the index corresponding to `mode` is enabled.
    pub(crate) fn is_enabled(&self, mode: IndexMode) -> bool {
        match mode {
            IndexMode::Subgraph => self.subgraph,
            IndexMode::Type => self.r#type,
            IndexMode::CacheTag => self.cache_tag,
        }
    }

}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct SubgraphInvalidationConfig {
    /// Enable the invalidation
    pub(crate) enabled: bool,
    /// Shared key needed to request the invalidation endpoint
    pub(crate) shared_key: String,
    /// Which invalidation indexes to maintain for this subgraph's cached entries. Defaults to
    /// all three (`subgraph`, `type`, `cache_tag`) enabled, matching the original
    /// `response_cache` behavior. Operators with workloads that only invalidate by a subset of
    /// modes can opt out of the unused indexes to reduce per-insert Redis work.
    ///
    /// Setting all three to `false` disables invalidation indexing entirely; the `/invalidation`
    /// endpoint then returns HTTP 400 for any request against this subgraph. This shape is
    /// useful for pure TTL-based caching without an invalidation API.
    #[serde(default)]
    pub(crate) indexes: InvalidationIndexes,
}

impl Default for SubgraphInvalidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            shared_key: String::default(),
            indexes: InvalidationIndexes::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct InvalidationEndpointConfig {
    /// Specify on which path you want to listen for invalidation endpoint.
    pub(crate) path: String,
    /// Listen address on which the invalidation endpoint must listen.
    pub(crate) listen: ListenAddr,
}

#[derive(Clone)]
pub(crate) struct InvalidationService {
    config: Arc<SubgraphConfiguration<Subgraph>>,
    invalidation: Invalidation,
}

impl InvalidationService {
    pub(crate) fn new(
        config: Arc<SubgraphConfiguration<Subgraph>>,
        invalidation: Invalidation,
    ) -> Self {
        Self {
            config,
            invalidation,
        }
    }
}

impl Service<router::Request> for InvalidationService {
    type Response = router::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut std::task::Context<'_>) -> Poll<Result<(), Self::Error>> {
        Ok(()).into()
    }

    fn call(&mut self, req: router::Request) -> Self::Future {
        const APPLICATION_JSON_HEADER_VALUE: HeaderValue =
            HeaderValue::from_static("application/json");
        let invalidation = self.invalidation.clone();
        let config = self.config.clone();
        Box::pin(
            async move {
                let (parts, body) = req.router_request.into_parts();
                if !parts.headers.contains_key(AUTHORIZATION) {
                    Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                    return router::Response::error_builder()
                        .status_code(StatusCode::UNAUTHORIZED)
                        .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                        .error(
                            graphql::Error::builder()
                                .message(String::from("Missing authorization header"))
                                .extension_code(StatusCode::UNAUTHORIZED.to_string())
                                .build(),
                        )
                        .context(req.context)
                        .build();
                }
                match parts.method {
                    Method::POST => {
                        let body = router::body::into_bytes(body)
                            .instrument(tracing::info_span!("into_bytes"))
                            .await
                            .map_err(|e| format!("failed to get the request body: {e}"))
                            .and_then(|bytes| {
                                serde_json::from_reader::<_, Vec<InvalidationRequest>>(
                                    bytes.reader(),
                                )
                                .map_err(|err| {
                                    format!(
                                        "failed to deserialize the request body into JSON: {err}"
                                    )
                                })
                            });
                        let shared_key = parts
                            .headers
                            .get(AUTHORIZATION)
                            .ok_or("cannot find authorization header")?
                            .to_str()
                            .inspect_err(|_err| {
                                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                            })?;
                        match body {
                            Ok(body) => {
                                Span::current().record(
                                    "invalidation.request.kinds",
                                    body.iter()
                                        .map(|i| i.kind())
                                        .collect::<Vec<&'static str>>()
                                        .join(", "),
                                );
                                let shared_key_is_valid = body
                                    .iter()
                                    .flat_map(|b| b.subgraph_names())
                                    .all(|subgraph_name| {
                                        validate_shared_key(&config, shared_key, &subgraph_name)
                                    });
                                if !shared_key_is_valid {
                                    Span::current()
                                        .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                    return router::Response::error_builder()
                                        .status_code(StatusCode::UNAUTHORIZED)
                                        .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                        .error(
                                            graphql::Error::builder()
                                                .message(String::from(
                                                    "Invalid authorization header",
                                                ))
                                                .extension_code(
                                                    StatusCode::UNAUTHORIZED.to_string(),
                                                )
                                                .build(),
                                        )
                                        .context(req.context)
                                        .build();
                                }
                                // Reject any request whose kind is not in the active subgraph's
                                // index_modes. Allows customers to opt out of maintaining
                                // by-subgraph and by-type indexes when they only invalidate by
                                // cache tag, and surfaces misconfiguration to callers fast.
                                if let Some(rejection) =
                                    find_disabled_mode_rejection(&config, &body)
                                {
                                    let (subgraph, kind) = rejection;
                                    Span::current()
                                        .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                    tracing::warn!(
                                        subgraph = %subgraph,
                                        kind = %kind,
                                        "rejected invalidation request: kind not enabled in index_modes for subgraph",
                                    );
                                    let message = format!(
                                        "invalidation kind '{kind}' is not enabled for subgraph \
                                         '{subgraph}'; index_modes does not include this kind",
                                    );
                                    return router::Response::error_builder()
                                        .status_code(StatusCode::BAD_REQUEST)
                                        .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                        .error(
                                            graphql::Error::builder()
                                                .message(message)
                                                .extension_code(
                                                    StatusCode::BAD_REQUEST.to_string(),
                                                )
                                                .build(),
                                        )
                                        .context(req.context)
                                        .build();
                                }
                                match invalidation
                                    .invalidate(body)
                                    .instrument(tracing::info_span!("invalidate"))
                                    .await
                                {
                                    Ok(count) => router::Response::http_response_builder()
                                        .response(
                                            http::Response::builder()
                                                .status(StatusCode::ACCEPTED)
                                                .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                                .body(router::body::from_bytes(
                                                    serde_json::to_string(&json!({
                                                        "count": count
                                                    }))?,
                                                ))
                                                .map_err(BoxError::from)?,
                                        )
                                        .context(req.context)
                                        .build(),
                                    Err(err) => {
                                        Span::current()
                                            .record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                        router::Response::error_builder()
                                            .status_code(StatusCode::BAD_REQUEST)
                                            .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                            .error(
                                                graphql::Error::builder()
                                                    .message(err.to_string())
                                                    .extension_code(
                                                        StatusCode::BAD_REQUEST.to_string(),
                                                    )
                                                    .build(),
                                            )
                                            .context(req.context)
                                            .build()
                                    }
                                }
                            }
                            Err(err) => {
                                Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                                router::Response::error_builder()
                                    .status_code(StatusCode::BAD_REQUEST)
                                    .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                                    .error(
                                        graphql::Error::builder()
                                            .message(err)
                                            .extension_code(StatusCode::BAD_REQUEST.to_string())
                                            .build(),
                                    )
                                    .context(req.context)
                                    .build()
                            }
                        }
                    }
                    _ => {
                        Span::current().record(OTEL_STATUS_CODE, OTEL_STATUS_CODE_ERROR);
                        router::Response::error_builder()
                            .status_code(StatusCode::METHOD_NOT_ALLOWED)
                            .header(CONTENT_TYPE, APPLICATION_JSON_HEADER_VALUE)
                            .error(
                                graphql::Error::builder()
                                    .message("".to_string())
                                    .extension_code(StatusCode::METHOD_NOT_ALLOWED.to_string())
                                    .build(),
                            )
                            .context(req.context)
                            .build()
                    }
                }
            }
            .instrument(tracing::info_span!(
                INVALIDATION_ENDPOINT_SPAN_NAME,
                "invalidation.request.kinds" = ::tracing::field::Empty,
                "otel.status_code" = OTEL_STATUS_CODE_OK,
            )),
        )
    }
}

fn validate_shared_key(
    config: &SubgraphConfiguration<Subgraph>,
    shared_key: &str,
    subgraph_name: &str,
) -> bool {
    config
        .all
        .invalidation
        .as_ref()
        .map(|i| i.shared_key == shared_key)
        .unwrap_or_default()
        || config
            .subgraphs
            .get(subgraph_name)
            .and_then(|s| s.invalidation.as_ref())
            .map(|i| i.shared_key == shared_key)
            .unwrap_or_default()
}

/// Map an `InvalidationRequest` kind string to the `IndexMode` that gates whether the
/// corresponding index is maintained on cache inserts.
fn invalidation_kind_to_index_mode(kind: &str) -> Option<IndexMode> {
    match kind {
        "subgraph" => Some(IndexMode::Subgraph),
        "type" => Some(IndexMode::Type),
        "cache_tag" => Some(IndexMode::CacheTag),
        _ => None,
    }
}

/// Resolve the effective `InvalidationIndexes` for `subgraph_name`. Prefers the per-subgraph
/// `invalidation` block, falls back to the `all` block, and finally to `InvalidationIndexes::default()`
/// when neither is configured. This is the single source of truth for invalidation-index resolution
/// across both the cache write path and the `/invalidation` endpoint handler, so the two stay in lockstep.
pub(crate) fn effective_invalidation_indexes(
    config: &SubgraphConfiguration<Subgraph>,
    subgraph_name: &str,
) -> InvalidationIndexes {
    if let Some(subgraph_invalidation) = config
        .subgraphs
        .get(subgraph_name)
        .and_then(|s| s.invalidation.as_ref())
    {
        return subgraph_invalidation.indexes;
    }
    if let Some(all_invalidation) = config.all.invalidation.as_ref() {
        return all_invalidation.indexes;
    }
    InvalidationIndexes::default()
}

/// Scan the parsed invalidation request batch for any item whose `kind` is not enabled for its
/// target subgraph. Returns `Some((subgraph_name, kind))` for the first offending pair so the
/// caller can render a precise 400 response, or `None` when every request is permitted.
///
/// Subgraph names are visited in sorted order so the error message is deterministic across
/// repeated calls, which matters for `CacheTag` requests whose `subgraphs` field is an unordered
/// `HashSet<String>`.
fn find_disabled_mode_rejection(
    config: &SubgraphConfiguration<Subgraph>,
    body: &[InvalidationRequest],
) -> Option<(String, &'static str)> {
    for request in body {
        let kind_str = request.kind();
        let Some(mode) = invalidation_kind_to_index_mode(kind_str) else {
            continue;
        };
        let mut subgraphs = request.subgraph_names();
        subgraphs.sort();
        for subgraph in subgraphs {
            let indexes = effective_invalidation_indexes(config, &subgraph);
            if !indexes.is_enabled(mode) {
                return Some((subgraph, kind_str));
            }
        }
    }
    None
}

#[cfg(test)]
mod indexes_tests {
    use std::collections::HashMap;

    use super::*;
    use crate::plugins::response_cache::plugin::Subgraph;

    /// Test helper: build an `InvalidationIndexes` from a list of modes that should be enabled.
    /// Any mode not in the list is set to `false`.
    fn indexes_with(enabled: &[IndexMode]) -> InvalidationIndexes {
        InvalidationIndexes {
            subgraph: enabled.contains(&IndexMode::Subgraph),
            r#type: enabled.contains(&IndexMode::Type),
            cache_tag: enabled.contains(&IndexMode::CacheTag),
        }
    }

    fn subgraph_config(
        all_indexes: Option<InvalidationIndexes>,
        per_subgraph: Option<(&str, InvalidationIndexes)>,
    ) -> SubgraphConfiguration<Subgraph> {
        let all = Subgraph {
            ttl: None,
            enabled: Some(true),
            redis: None,
            private_id: None,
            invalidation: all_indexes.map(|indexes| SubgraphInvalidationConfig {
                enabled: true,
                shared_key: String::from("k"),
                indexes,
            }),
        };
        let subgraphs = if let Some((name, indexes)) = per_subgraph {
            let mut map = HashMap::new();
            map.insert(
                name.to_string(),
                Subgraph {
                    ttl: None,
                    enabled: Some(true),
                    redis: None,
                    private_id: None,
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: String::from("k"),
                        indexes,
                    }),
                },
            );
            map
        } else {
            HashMap::new()
        };
        SubgraphConfiguration { all, subgraphs }
    }

    #[test]
    fn invalidation_indexes_default_enables_all_three() {
        let indexes = InvalidationIndexes::default();
        assert!(indexes.subgraph);
        assert!(indexes.r#type);
        assert!(indexes.cache_tag);
    }

    #[test]
    fn invalidation_indexes_is_enabled_dispatches_per_mode() {
        let indexes = InvalidationIndexes {
            subgraph: true,
            r#type: false,
            cache_tag: true,
        };
        assert!(indexes.is_enabled(IndexMode::Subgraph));
        assert!(!indexes.is_enabled(IndexMode::Type));
        assert!(indexes.is_enabled(IndexMode::CacheTag));
    }


    #[test]
    fn subgraph_invalidation_config_default_has_all_three_indexes_enabled() {
        let cfg = SubgraphInvalidationConfig::default();
        assert!(cfg.indexes.subgraph);
        assert!(cfg.indexes.r#type);
        assert!(cfg.indexes.cache_tag);
    }

    #[test]
    fn subgraph_invalidation_config_yaml_default_round_trip() {
        // Omitting the `indexes` block from YAML should default to all three indexes enabled.
        let yaml = "enabled: true\nshared_key: secret\n";
        let cfg: SubgraphInvalidationConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.indexes.subgraph);
        assert!(cfg.indexes.r#type);
        assert!(cfg.indexes.cache_tag);
    }

    #[test]
    fn subgraph_invalidation_config_yaml_partial_indexes_field_inherits_defaults() {
        // Operators only have to write what they're disabling; omitted sub-fields stay `true`.
        let yaml = "\
enabled: true
shared_key: secret
indexes:
  subgraph: false
  type: false
";
        let cfg: SubgraphInvalidationConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.indexes.subgraph);
        assert!(!cfg.indexes.r#type);
        assert!(cfg.indexes.cache_tag, "cache_tag should default to true");
    }

    #[test]
    fn subgraph_invalidation_config_yaml_all_indexes_off() {
        let yaml = "\
enabled: true
shared_key: secret
indexes:
  subgraph: false
  type: false
  cache_tag: false
";
        let cfg: SubgraphInvalidationConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(!cfg.indexes.subgraph);
        assert!(!cfg.indexes.r#type);
        assert!(!cfg.indexes.cache_tag);
    }

    #[test]
    fn invalidation_kind_to_index_mode_maps_correctly() {
        assert_eq!(
            invalidation_kind_to_index_mode("subgraph"),
            Some(IndexMode::Subgraph)
        );
        assert_eq!(
            invalidation_kind_to_index_mode("type"),
            Some(IndexMode::Type)
        );
        assert_eq!(
            invalidation_kind_to_index_mode("cache_tag"),
            Some(IndexMode::CacheTag)
        );
        assert_eq!(invalidation_kind_to_index_mode("nonsense"), None);
    }

    #[test]
    fn effective_invalidation_indexes_prefers_per_subgraph_over_all() {
        let cfg = subgraph_config(
            Some(InvalidationIndexes::default()),
            Some(("payments", indexes_with(&[IndexMode::CacheTag]))),
        );
        let indexes = effective_invalidation_indexes(&cfg, "payments");
        assert!(!indexes.subgraph);
        assert!(!indexes.r#type);
        assert!(indexes.cache_tag);
    }

    #[test]
    fn effective_invalidation_indexes_falls_back_to_all_when_no_per_subgraph_entry() {
        let cfg = subgraph_config(Some(indexes_with(&[IndexMode::Type])), None);
        let indexes = effective_invalidation_indexes(&cfg, "anything");
        assert!(!indexes.subgraph);
        assert!(indexes.r#type);
        assert!(!indexes.cache_tag);
    }

    #[test]
    fn effective_invalidation_indexes_falls_back_to_default_when_no_config() {
        let cfg = subgraph_config(None, None);
        let indexes = effective_invalidation_indexes(&cfg, "anything");
        assert!(indexes.subgraph);
        assert!(indexes.r#type);
        assert!(indexes.cache_tag);
    }

    #[test]
    fn find_disabled_mode_rejection_returns_none_when_all_kinds_allowed() {
        let cfg = subgraph_config(Some(InvalidationIndexes::default()), None);
        let body = vec![InvalidationRequest::Subgraph {
            subgraph: "users".to_string(),
        }];
        assert_eq!(find_disabled_mode_rejection(&cfg, &body), None);
    }

    #[test]
    fn find_disabled_mode_rejection_flags_subgraph_kind_when_disabled() {
        let cfg = subgraph_config(Some(indexes_with(&[IndexMode::CacheTag])), None);
        let body = vec![InvalidationRequest::Subgraph {
            subgraph: "users".to_string(),
        }];
        assert_eq!(
            find_disabled_mode_rejection(&cfg, &body),
            Some(("users".to_string(), "subgraph"))
        );
    }

    #[test]
    fn find_disabled_mode_rejection_flags_type_kind_when_disabled() {
        let cfg = subgraph_config(Some(indexes_with(&[IndexMode::CacheTag])), None);
        let body = vec![InvalidationRequest::Type {
            subgraph: "users".to_string(),
            r#type: "User".to_string(),
        }];
        assert_eq!(
            find_disabled_mode_rejection(&cfg, &body),
            Some(("users".to_string(), "type"))
        );
    }

    #[test]
    fn find_disabled_mode_rejection_flags_cache_tag_when_disabled() {
        let cfg = subgraph_config(
            Some(indexes_with(&[IndexMode::Subgraph, IndexMode::Type])),
            None,
        );
        let mut subgraphs = std::collections::HashSet::new();
        subgraphs.insert("users".to_string());
        let body = vec![InvalidationRequest::CacheTag {
            subgraphs,
            cache_tag: "homepage".to_string(),
        }];
        let rejection = find_disabled_mode_rejection(&cfg, &body);
        assert_eq!(rejection, Some(("users".to_string(), "cache_tag")));
    }

    #[test]
    fn find_disabled_mode_rejection_respects_per_subgraph_override() {
        // `all` has all three indexes; `payments` only allows cache_tag. A subgraph-kind request
        // against `payments` should be rejected even though `all` allows it.
        let cfg = subgraph_config(
            Some(InvalidationIndexes::default()),
            Some(("payments", indexes_with(&[IndexMode::CacheTag]))),
        );
        let body = vec![InvalidationRequest::Subgraph {
            subgraph: "payments".to_string(),
        }];
        assert_eq!(
            find_disabled_mode_rejection(&cfg, &body),
            Some(("payments".to_string(), "subgraph"))
        );
    }

    #[test]
    fn find_disabled_mode_rejection_picks_deterministic_subgraph_for_multi_subgraph_cache_tag() {
        // CacheTag requests carry a `HashSet<String>` of subgraph names; the rejection should
        // surface the lexicographically first subgraph so repeated calls return the same error.
        // `users` is disabled (subgraph index off); `orders` is fully enabled.
        let cfg = subgraph_config(
            Some(InvalidationIndexes::default()),
            Some(("users", indexes_with(&[IndexMode::Subgraph, IndexMode::Type]))),
        );
        let mut subgraphs = std::collections::HashSet::new();
        subgraphs.insert("users".to_string());
        subgraphs.insert("orders".to_string());
        let body = vec![InvalidationRequest::CacheTag {
            subgraphs,
            cache_tag: "homepage".to_string(),
        }];
        // The disabled subgraph is "users"; should be returned reliably.
        let rejection = find_disabled_mode_rejection(&cfg, &body);
        assert_eq!(rejection, Some(("users".to_string(), "cache_tag")));
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
mod tests {
    use std::collections::HashMap;

    use tokio::sync::broadcast;
    use tower::ServiceExt;

    use super::*;
    use crate::plugins::response_cache::plugin::StorageInterface;
    use crate::plugins::response_cache::storage::redis::Config;
    use crate::plugins::response_cache::storage::redis::Storage;
    use crate::services::router::body;

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_bad_shared_key"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                    ..Default::default()
                }),
            },
            subgraphs: HashMap::new(),
        });
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "testttt")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("test"),
                    },
                    InvalidationRequest::Type {
                        subgraph: String::from("test"),
                        r#type: String::from("Test"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key_subgraph() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_bad_shared_key_subgraph"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                    ..Default::default()
                }),
            },
            subgraphs: [(
                String::from("test"),
                Subgraph {
                    ttl: None,
                    enabled: Some(true),
                    redis: None,
                    private_id: None,
                    invalidation: Some(SubgraphInvalidationConfig {
                        enabled: true,
                        shared_key: String::from("test_test"),
                        ..Default::default()
                    }),
                },
            )]
            .into_iter()
            .collect(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test_test")
            .body(
                serde_json::to_vec(&[InvalidationRequest::Subgraph {
                    subgraph: String::from("foo"),
                }])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_bad_shared_key_subgraphs() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_bad_shared_key_subgraphs"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                    ..Default::default()
                }),
            },
            subgraphs: [
                (
                    String::from("foor"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        redis: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test"),
                            ..Default::default()
                        }),
                    },
                ),
                (
                    String::from("bar"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        redis: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test_bis"),
                            ..Default::default()
                        }),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test_test")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("foo"),
                    },
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("bar"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert_eq!(res.response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_good_shared_key_subgraphs() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_good_shared_key_subgraphs"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                    ..Default::default()
                }),
            },
            subgraphs: [
                (
                    String::from("foor"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        redis: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test"),
                            ..Default::default()
                        }),
                    },
                ),
                (
                    String::from("bar"),
                    Subgraph {
                        ttl: None,
                        enabled: Some(true),
                        redis: None,
                        private_id: None,
                        invalidation: Some(SubgraphInvalidationConfig {
                            enabled: true,
                            shared_key: String::from("test_test_bis"),
                            ..Default::default()
                        }),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test")
            .body(
                serde_json::to_vec(&[
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("foo"),
                    },
                    InvalidationRequest::Subgraph {
                        subgraph: String::from("bar"),
                    },
                ])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert!(res.response.status() != StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_invalidation_service_deny_unknown_fields() {
        let (_drop_tx, drop_rx) = broadcast::channel(2);
        let storage = Storage::new(
            &Config::test(false, "test_invalidation_service_good_shared_key_subgraphs"),
            drop_rx,
        )
        .await
        .unwrap();
        let storage = Arc::new(StorageInterface::from(storage));
        let invalidation = Invalidation::new(storage.clone()).await.unwrap();

        let config = Arc::new(SubgraphConfiguration {
            all: Subgraph {
                ttl: None,
                enabled: Some(true),
                redis: None,
                private_id: None,
                invalidation: Some(SubgraphInvalidationConfig {
                    enabled: true,
                    shared_key: String::from("test"),
                    ..Default::default()
                }),
            },
            subgraphs: HashMap::new(),
        });
        // Trying to invalidation with shared_key on subgraph test for a subgraph foo
        let service = InvalidationService::new(config, invalidation);
        let req = router::Request::fake_builder()
            .method(http::Method::POST)
            .header(AUTHORIZATION, "test")
            .body(
                serde_json::to_vec(&[serde_json::json!({
                    "kind": "type",
                    "subgraph": "foo",
                    "type": "User",
                    "key": {
                        "id": "1"
                    }
                })])
                .unwrap(),
            )
            .build()
            .unwrap();
        let res = service.oneshot(req).await.unwrap();
        assert_eq!(
            res.response.headers().get(&CONTENT_TYPE).unwrap(),
            &HeaderValue::from_static("application/json")
        );
        assert!(res.response.status() != StatusCode::UNAUTHORIZED);
        assert_eq!(res.response.status(), StatusCode::BAD_REQUEST);
        let response_body_str = body::into_string(res.response.into_body()).await.unwrap();
        assert!(
            response_body_str
                .contains("failed to deserialize the request body into JSON: unknown field")
        );
    }
}
