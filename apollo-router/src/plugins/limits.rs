use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use tower::BoxError;
use tower::ServiceExt;

use crate::plugin::Plugin;
use crate::plugin::PluginInit;
use crate::plugins::telemetry::utils::TracingUtils;
use crate::services::router::BoxService;
use crate::spec::operation_limits::OperationLimits;

/// A plugin for limits.

/// Configuration for operation limits, parser limits, HTTP limits, etc.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub(crate) struct Config {
    /// If set, requests with operations deeper than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Counts depth of an operation, looking at its selection sets,
    /// including fields in fragments and inline fragments. The following
    /// example has a depth of 3.
    ///
    /// ```graphql
    /// query getProduct {
    ///   book { # 1
    ///     ...bookDetails
    ///   }
    /// }
    ///
    /// fragment bookDetails on Book {
    ///   details { # 2
    ///     ... on ProductDetailsBook {
    ///       country # 3
    ///     }
    ///   }
    /// }
    /// ```
    pub(crate) max_depth: Option<u32>,

    /// If set, requests with operations higher than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_DEPTH_LIMIT"}`
    ///
    /// Height is based on simple merging of fields using the same name or alias,
    /// but only within the same selection set.
    /// For example `name` here is only counted once and the query has height 3, not 4:
    ///
    /// ```graphql
    /// query {
    ///     name { first }
    ///     name { last }
    /// }
    /// ```
    ///
    /// This may change in a future version of Apollo Router to do
    /// [full field merging across fragments][merging] instead.
    ///
    /// [merging]: https://spec.graphql.org/October2021/#sec-Field-Selection-Merging]
    pub(crate) max_height: Option<u32>,

    /// If set, requests with operations with more root fields than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_ROOT_FIELDS_LIMIT"}`
    ///
    /// This limit counts only the top level fields in a selection set,
    /// including fragments and inline fragments.
    pub(crate) max_root_fields: Option<u32>,

    /// If set, requests with operations with more aliases than this maximum
    /// are rejected with a HTTP 400 Bad Request response and GraphQL error with
    /// `"extensions": {"code": "MAX_ALIASES_LIMIT"}`
    pub(crate) max_aliases: Option<u32>,

    /// If set to true (which is the default is dev mode),
    /// requests that exceed a `max_*` limit are *not* rejected.
    /// Instead they are executed normally, and a warning is logged.
    pub(crate) warn_only: bool,

    /// Limit recursion in the GraphQL parser to protect against stack overflow.
    /// default: 4096
    pub(crate) parser_max_recursion: usize,

    /// Limit the number of tokens the GraphQL parser processes before aborting.
    pub(crate) parser_max_tokens: usize,

    /// Limit the size of incoming HTTP requests read from the network,
    /// to protect against running out of memory. Default: 2000000 (2 MB)
    pub(crate) experimental_http_max_request_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // These limits are opt-in
            max_depth: None,
            max_height: None,
            max_root_fields: None,
            max_aliases: None,
            warn_only: false,
            experimental_http_max_request_bytes: 2_000_000,
            parser_max_tokens: 15_000,

            // This is `apollo-parser`’s default, which protects against stack overflow
            // but is still very high for "reasonable" queries.
            // https://docs.rs/apollo-parser/0.2.8/src/apollo_parser/parser/mod.rs.html#368
            parser_max_recursion: 4096,
        }
    }
}

struct Limits {}

#[derive(Default)]
pub(crate) struct Limited {
    operational_limits: OperationLimits<bool>,
    request_size: bool,
}

impl Limited {
    pub(crate) fn request_size() -> Self {
        Limited {
            request_size: true,
            ..Default::default()
        }
    }
}

impl From<&OperationLimits<bool>> for Limited {
    fn from(limits: &OperationLimits<bool>) -> Self {
        Limited {
            operational_limits: *limits,
            ..Default::default()
        }
    }
}

#[async_trait::async_trait]
impl Plugin for Limits {
    type Config = Config;

    async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError>
    where
        Self: Sized,
    {
        Ok(Self {})
    }

    fn router_service(&self, service: BoxService) -> BoxService {
        service
            .map_future(|f| async {
                let response = f.await;
                if let Ok(response) = response.as_ref() {
                    if let Some(limited) = response.context.private_entries.lock().get::<Limited>()
                    {
                        tracing::info!(
                            monotonic_counter.apollo.router.operations.limits = 1u64,
                            limits.failed.operation.aliases =
                                limited.operational_limits.aliases.or_empty(),
                            limits.failed.operation.depth =
                                limited.operational_limits.depth.or_empty(),
                            limits.failed.operation.height =
                                limited.operational_limits.height.or_empty(),
                            limits.failed.operation.root_fields =
                                limited.operational_limits.root_fields.or_empty(),
                            limits.failed.request.size = limited.request_size.or_empty(),
                        );
                    } else {
                        tracing::info!(monotonic_counter.apollo.router.operations.limits = 1u64,);
                    }
                } else {
                    tracing::info!(monotonic_counter.apollo.router.operations.limits = 1u64,);
                }

                response
            })
            .boxed()
    }
}

register_plugin!("apollo", "limits", Limits);
