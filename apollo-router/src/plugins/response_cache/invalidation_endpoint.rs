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

/// Which invalidation index modes to maintain for cached entries.
///
/// Each mode corresponds to one of the kinds of invalidation requests documented at
/// <https://www.apollographql.com/docs/graphos/routing/performance/caching/response-caching/invalidation>.
/// Disabling a mode skips writing the corresponding Redis ZSET index entries on cache inserts,
/// at the cost of being unable to invalidate cached entries by that mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IndexMode {
    /// Maintain the `subgraph-{name}` ZSET. Enables `By subgraph` invalidation requests.
    Subgraph,
    /// Maintain the by-type ZSET keyed by `subgraph:{name}:type:{type}`. Enables `By type`
    /// invalidation requests.
    Type,
    /// Maintain ZSETs for user-supplied cache tags from `apolloCacheTags`,
    /// `apolloEntityCacheTags`, and resolved `@cacheTag` directive values. Enables
    /// `By cache tag` invalidation requests.
    CacheTag,
}

/// Default invalidation index modes: all three, for backward compatibility with
/// deployments predating the `index_modes` setting.
pub(crate) fn default_index_modes() -> Vec<IndexMode> {
    vec![IndexMode::Subgraph, IndexMode::Type, IndexMode::CacheTag]
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields, default)]
pub(crate) struct SubgraphInvalidationConfig {
    /// Enable the invalidation
    pub(crate) enabled: bool,
    /// Shared key needed to request the invalidation endpoint
    pub(crate) shared_key: String,
    /// Which invalidation index modes to maintain for this subgraph's cached entries.
    /// Defaults to all three (`subgraph`, `type`, `cache_tag`) for backward compatibility.
    ///
    /// Customers who only invalidate by cache tag can set this to `["cache_tag"]` to avoid
    /// Redis CPU and memory cost from maintaining the `by subgraph` and `by type` indexes.
    /// Setting this to `[]` disables all invalidation indexing; the `/invalidation` endpoint
    /// will return HTTP 400 for any request against the affected subgraph.
    #[serde(default = "default_index_modes")]
    pub(crate) index_modes: Vec<IndexMode>,
}

impl Default for SubgraphInvalidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            shared_key: String::default(),
            index_modes: default_index_modes(),
        }
    }
}

impl SubgraphInvalidationConfig {
    /// Resolved set of active index modes, for fast membership checks on the hot path.
    pub(crate) fn index_mode_set(&self) -> std::collections::HashSet<IndexMode> {
        self.index_modes.iter().copied().collect()
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

/// Resolve the effective `index_modes` set for `subgraph_name` from the per-subgraph configuration.
/// Falls back to the `all` block when no per-subgraph entry exists, and finally to the documented
/// default (all three modes) when neither defines an `invalidation` block.
fn effective_index_modes(
    config: &SubgraphConfiguration<Subgraph>,
    subgraph_name: &str,
) -> std::collections::HashSet<IndexMode> {
    if let Some(subgraph_invalidation) = config
        .subgraphs
        .get(subgraph_name)
        .and_then(|s| s.invalidation.as_ref())
    {
        return subgraph_invalidation.index_mode_set();
    }
    if let Some(all_invalidation) = config.all.invalidation.as_ref() {
        return all_invalidation.index_mode_set();
    }
    default_index_modes().into_iter().collect()
}

/// Scan the parsed invalidation request batch for any item whose `kind` is not present in the
/// effective `index_modes` of its target subgraph(s). Returns `Some((subgraph_name, kind))` for
/// the first offending pair so the caller can render a precise 400 response, or `None` when every
/// request is permitted.
fn find_disabled_mode_rejection(
    config: &SubgraphConfiguration<Subgraph>,
    body: &[InvalidationRequest],
) -> Option<(String, &'static str)> {
    for request in body {
        let kind_str = request.kind();
        let Some(mode) = invalidation_kind_to_index_mode(kind_str) else {
            continue;
        };
        for subgraph in request.subgraph_names() {
            let modes = effective_index_modes(config, &subgraph);
            if !modes.contains(&mode) {
                return Some((subgraph, kind_str));
            }
        }
    }
    None
}

#[cfg(test)]
mod index_modes_tests {
    use std::collections::HashMap;

    use super::*;
    use crate::plugins::response_cache::plugin::Subgraph;

    fn subgraph_config(
        all_modes: Option<Vec<IndexMode>>,
        per_subgraph: Option<(&str, Vec<IndexMode>)>,
    ) -> SubgraphConfiguration<Subgraph> {
        let all = Subgraph {
            ttl: None,
            enabled: Some(true),
            redis: None,
            private_id: None,
            invalidation: all_modes.map(|modes| SubgraphInvalidationConfig {
                enabled: true,
                shared_key: String::from("k"),
                index_modes: modes,
            }),
        };
        let subgraphs = if let Some((name, modes)) = per_subgraph {
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
                        index_modes: modes,
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
    fn default_index_modes_contains_all_three() {
        let modes = default_index_modes();
        assert_eq!(modes.len(), 3);
        assert!(modes.contains(&IndexMode::Subgraph));
        assert!(modes.contains(&IndexMode::Type));
        assert!(modes.contains(&IndexMode::CacheTag));
    }

    #[test]
    fn subgraph_invalidation_config_default_has_all_three_modes() {
        let cfg = SubgraphInvalidationConfig::default();
        let set = cfg.index_mode_set();
        assert_eq!(set.len(), 3);
        assert!(set.contains(&IndexMode::Subgraph));
        assert!(set.contains(&IndexMode::Type));
        assert!(set.contains(&IndexMode::CacheTag));
    }

    #[test]
    fn subgraph_invalidation_config_yaml_default_round_trip() {
        // Omitting index_modes from YAML should default to all three modes.
        let yaml = "enabled: true\nshared_key: secret\n";
        let cfg: SubgraphInvalidationConfig = serde_yaml::from_str(yaml).unwrap();
        let set = cfg.index_mode_set();
        assert_eq!(set.len(), 3);
        assert!(set.contains(&IndexMode::Subgraph));
        assert!(set.contains(&IndexMode::Type));
        assert!(set.contains(&IndexMode::CacheTag));
    }

    #[test]
    fn subgraph_invalidation_config_yaml_explicit_cache_tag_only() {
        let yaml = "enabled: true\nshared_key: secret\nindex_modes:\n  - cache_tag\n";
        let cfg: SubgraphInvalidationConfig = serde_yaml::from_str(yaml).unwrap();
        let set = cfg.index_mode_set();
        assert_eq!(set.len(), 1);
        assert!(set.contains(&IndexMode::CacheTag));
        assert!(!set.contains(&IndexMode::Subgraph));
        assert!(!set.contains(&IndexMode::Type));
    }

    #[test]
    fn subgraph_invalidation_config_yaml_empty_modes() {
        let yaml = "enabled: true\nshared_key: secret\nindex_modes: []\n";
        let cfg: SubgraphInvalidationConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.index_mode_set().is_empty());
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
    fn effective_index_modes_prefers_per_subgraph_over_all() {
        let cfg = subgraph_config(
            Some(default_index_modes()),
            Some(("payments", vec![IndexMode::CacheTag])),
        );
        let modes = effective_index_modes(&cfg, "payments");
        assert_eq!(modes.len(), 1);
        assert!(modes.contains(&IndexMode::CacheTag));
    }

    #[test]
    fn effective_index_modes_falls_back_to_all_when_no_per_subgraph_entry() {
        let cfg = subgraph_config(Some(vec![IndexMode::Type]), None);
        let modes = effective_index_modes(&cfg, "anything");
        assert_eq!(modes.len(), 1);
        assert!(modes.contains(&IndexMode::Type));
    }

    #[test]
    fn effective_index_modes_falls_back_to_default_when_no_config() {
        let cfg = subgraph_config(None, None);
        let modes = effective_index_modes(&cfg, "anything");
        assert_eq!(modes.len(), 3);
    }

    #[test]
    fn find_disabled_mode_rejection_returns_none_when_all_kinds_allowed() {
        let cfg = subgraph_config(Some(default_index_modes()), None);
        let body = vec![InvalidationRequest::Subgraph {
            subgraph: "users".to_string(),
        }];
        assert_eq!(find_disabled_mode_rejection(&cfg, &body), None);
    }

    #[test]
    fn find_disabled_mode_rejection_flags_subgraph_kind_when_disabled() {
        let cfg = subgraph_config(Some(vec![IndexMode::CacheTag]), None);
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
        let cfg = subgraph_config(Some(vec![IndexMode::CacheTag]), None);
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
            Some(vec![IndexMode::Subgraph, IndexMode::Type]),
            None,
        );
        let mut subgraphs = std::collections::HashSet::new();
        subgraphs.insert("users".to_string());
        let body = vec![InvalidationRequest::CacheTag {
            subgraphs,
            cache_tag: "homepage".to_string(),
        }];
        let rejection = find_disabled_mode_rejection(&cfg, &body);
        assert_eq!(
            rejection,
            Some(("users".to_string(), "cache_tag"))
        );
    }

    #[test]
    fn find_disabled_mode_rejection_respects_per_subgraph_override() {
        // `all` has all three; `payments` only allows cache_tag. A subgraph-kind request
        // against `payments` should be rejected even though `all` allows it.
        let cfg = subgraph_config(
            Some(default_index_modes()),
            Some(("payments", vec![IndexMode::CacheTag])),
        );
        let body = vec![InvalidationRequest::Subgraph {
            subgraph: "payments".to_string(),
        }];
        assert_eq!(
            find_disabled_mode_rejection(&cfg, &body),
            Some(("payments".to_string(), "subgraph"))
        );
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
