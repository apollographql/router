//! Externalization plugin

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bytes::Bytes;
use futures::StreamExt;
use futures::TryStreamExt;
use futures::future::ready;
use futures::stream::once;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;
use http::header;
use http_body_util::BodyExt;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::Layer;
use tower::Service;
use tower::ServiceBuilder;
use tower::ServiceExt;
use tower::timeout::TimeoutLayer;
use tower::util::MapFutureLayer;

use crate::Context;
use crate::configuration::shared::Client;
use crate::context::context_key_from_deprecated;
use crate::context::context_key_to_deprecated;
use crate::error::Error;
use crate::graphql;
use crate::json_ext::Value;
use crate::layers::ServiceBuilderExt;
use crate::layers::ServiceExt as _;
use crate::layers::async_checkpoint::AsyncCheckpointLayer;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::telemetry::config_new::conditions::Condition;
use crate::plugins::telemetry::config_new::router::selectors::RouterSelector;
use crate::plugins::telemetry::config_new::subgraph::selectors::SubgraphSelector;
use crate::register_private_plugin;
use crate::services;
use crate::services::PATH_QUERY_PARAM;
use crate::services::external::Control;
use crate::services::external::DEFAULT_EXTERNALIZATION_TIMEOUT;
use crate::services::external::EXTERNALIZABLE_VERSION;
use crate::services::external::Externalizable;
use crate::services::external::PipelineStep;
use crate::services::external::externalize_header_map;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;
use crate::services::router::body::RouterBody;
use crate::services::subgraph;

#[cfg(test)]
mod test;

mod connector;
mod execution;
mod supergraph;

pub(crate) const EXTERNAL_SPAN_NAME: &str = "external_plugin";
const COPROCESSOR_ERROR_EXTENSION: &str = "ERROR";
const COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION: &str = "EXTERNAL_DESERIALIZATION_ERROR";

// Type alias for coprocessor HTTP client - uses HttpClientService with timeout
type HTTPClientService = tower::timeout::Timeout<crate::services::http::HttpClientService>;

#[async_trait::async_trait]
impl PluginPrivate for CoprocessorPlugin<HTTPClientService> {
    type Config = Conf;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let client_config = init.config.client.clone().unwrap_or_default();

        if matches!(
            init.config.router.request.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.router.request.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.router.response.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.router.response.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.supergraph.request.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.supergraph.request.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.supergraph.response.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.supergraph.response.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.execution.request.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.execution.request.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.execution.response.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.execution.response.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.subgraph.all.request.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.subgraph.all.request.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.subgraph.all.response.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.subgraph.all.response.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.connector.all.request.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.connector.all.request.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }
        if matches!(
            init.config.connector.all.response.context,
            ContextConf::Deprecated(true)
        ) {
            tracing::warn!(
                "Configuration `coprocessor.connector.all.response.context: true` is deprecated. See https://go.apollo.dev/o/coprocessor-context"
            );
        }

        // Validate all coprocessor URLs
        validate_coprocessor_url(&init.config.url, "coprocessor.url")?;
        if let Some(ref url) = init.config.router.request.url {
            validate_coprocessor_url(url, "coprocessor.router.request.url")?;
        }
        if let Some(ref url) = init.config.router.response.url {
            validate_coprocessor_url(url, "coprocessor.router.response.url")?;
        }
        if let Some(ref url) = init.config.supergraph.request.url {
            validate_coprocessor_url(url, "coprocessor.supergraph.request.url")?;
        }
        if let Some(ref url) = init.config.supergraph.response.url {
            validate_coprocessor_url(url, "coprocessor.supergraph.response.url")?;
        }
        if let Some(ref url) = init.config.execution.request.url {
            validate_coprocessor_url(url, "coprocessor.execution.request.url")?;
        }
        if let Some(ref url) = init.config.execution.response.url {
            validate_coprocessor_url(url, "coprocessor.execution.response.url")?;
        }
        if let Some(ref url) = init.config.subgraph.all.request.url {
            validate_coprocessor_url(url, "coprocessor.subgraph.all.request.url")?;
        }
        if let Some(ref url) = init.config.subgraph.all.response.url {
            validate_coprocessor_url(url, "coprocessor.subgraph.all.response.url")?;
        }

        // Use shared HttpClientService infrastructure instead of duplicated client creation
        let tls_root_store =
            crate::services::http::service::HttpClientService::native_roots_store();
        let http_client_service =
            crate::services::http::service::HttpClientService::from_config_for_coprocessor(
                &tls_root_store,
                client_config,
            )?;

        let client = TimeoutLayer::new(init.config.timeout).layer(http_client_service);

        CoprocessorPlugin::new(client, init.config, init.supergraph_sdl)
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        self.router_service(service)
    }

    fn supergraph_service(
        &self,
        service: services::supergraph::BoxService,
    ) -> services::supergraph::BoxService {
        self.supergraph_service(service)
    }

    fn execution_service(
        &self,
        service: services::execution::BoxService,
    ) -> services::execution::BoxService {
        self.execution_service(service)
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: subgraph::BoxCloneSyncService,
    ) -> subgraph::BoxCloneSyncService {
        self.subgraph_service(name, service)
    }

    fn connector_request_service(
        &self,
        service: crate::services::connector::request_service::BoxService,
        source_name: String,
    ) -> crate::services::connector::request_service::BoxService {
        self.connector_request_service(&source_name, service)
    }
}

// This macro allows us to use it in our plugin registry!
// register_private_plugin takes a group name, and a plugin name.
//
// In order to keep the plugin names consistent,
// we use using the `Reverse domain name notation`
register_private_plugin!(
    "apollo",
    "coprocessor",
    CoprocessorPlugin<HTTPClientService>
);

// -------------------------------------------------------------------------------------------------------

/// This is where the real implementation happens.
/// The structure above calls the functions defined below.
///
/// This structure is generic over the HTTP Service so we can test the plugin seamlessly.
#[derive(Debug)]
struct CoprocessorPlugin<C>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    http_client: C,
    configuration: Conf,
    sdl: Arc<String>,
}

impl<C> CoprocessorPlugin<C>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    fn new(http_client: C, configuration: Conf, sdl: Arc<String>) -> Result<Self, BoxError> {
        Ok(Self {
            http_client,
            configuration,
            sdl,
        })
    }

    fn router_service(&self, service: router::BoxService) -> router::BoxService {
        self.configuration.router.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
            self.configuration.response_validation,
        )
    }

    fn supergraph_service(
        &self,
        service: services::supergraph::BoxService,
    ) -> services::supergraph::BoxService {
        self.configuration.supergraph.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
            self.configuration.response_validation,
        )
    }

    fn execution_service(
        &self,
        service: services::execution::BoxService,
    ) -> services::execution::BoxService {
        self.configuration.execution.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            self.sdl.clone(),
            self.configuration.response_validation,
        )
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: subgraph::BoxCloneSyncService,
    ) -> subgraph::BoxCloneSyncService {
        self.configuration.subgraph.all.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            name.to_string(),
            self.configuration.response_validation,
        )
    }

    fn connector_request_service(
        &self,
        source_name: &str,
        service: crate::services::connector::request_service::BoxService,
    ) -> crate::services::connector::request_service::BoxService {
        self.configuration.connector.all.as_service(
            self.http_client.clone(),
            service,
            self.configuration.url.clone(),
            source_name.to_string(),
        )
    }
}
/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RouterRequestConf {
    /// Condition to trigger this stage
    pub(super) condition: Option<Condition<RouterSelector>>,
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: ContextConf,
    /// Send the body
    pub(super) body: bool,
    /// Send the SDL
    pub(super) sdl: bool,
    /// Send the path
    pub(super) path: bool,
    /// Send the method
    pub(super) method: bool,
    /// The coprocessor URL for this stage (overrides the global URL if specified)
    pub(super) url: Option<String>,
}

/// What information is passed to a router request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RouterResponseConf {
    /// Condition to trigger this stage
    pub(super) condition: Condition<RouterSelector>,
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: ContextConf,
    /// Send the body
    pub(super) body: bool,
    /// Send the SDL
    pub(super) sdl: bool,
    /// Send the HTTP status
    pub(super) status_code: bool,
    /// The coprocessor URL for this stage (overrides the global URL if specified)
    pub(super) url: Option<String>,
}
/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphRequestConf {
    /// Condition to trigger this stage
    pub(super) condition: Condition<SubgraphSelector>,
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: ContextConf,
    /// Send the body
    pub(super) body: bool,
    /// Send the subgraph URI
    pub(super) uri: bool,
    /// Send the method URI
    pub(super) method: bool,
    /// Send the service name
    pub(super) service_name: bool,
    /// Send the subgraph request id
    pub(super) subgraph_request_id: bool,
    /// The coprocessor URL for this stage (overrides the global URL if specified)
    pub(super) url: Option<String>,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphResponseConf {
    /// Condition to trigger this stage
    pub(super) condition: Condition<SubgraphSelector>,
    /// Send the headers
    pub(super) headers: bool,
    /// Send the context
    pub(super) context: ContextConf,
    /// Send the body (can be true/false or selective with data/errors/extensions)
    pub(super) body: BodyConf,
    /// Send the service name
    pub(super) service_name: bool,
    /// Send the http status
    pub(super) status_code: bool,
    /// Send the subgraph request id
    pub(super) subgraph_request_id: bool,
    /// The coprocessor URL for this stage (overrides the global URL if specified)
    pub(super) url: Option<String>,
}

/// Configures the externalization plugin
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "CoprocessorConfig")]
struct Conf {
    /// The url you'd like to offload processing to (can be overridden per-stage). Supports HTTP/HTTPS (http://127.0.0.1:8081/urlpath) and Unix Domain Socket (unix:///path/to/socket) URLs
    url: String,
    client: Option<Client>,
    /// The timeout for external requests
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    #[schemars(with = "String", default = "default_timeout")]
    #[serde(default = "default_timeout")]
    timeout: Duration,
    /// Response validation defaults to true
    #[serde(default = "default_response_validation")]
    response_validation: bool,
    /// The router stage request/response configuration
    #[serde(default)]
    router: RouterStage,
    /// The supergraph stage request/response configuration
    #[serde(default)]
    supergraph: supergraph::SupergraphStage,
    /// The execution stage request/response configuration
    #[serde(default)]
    execution: execution::ExecutionStage,
    /// The subgraph stage request/response configuration
    #[serde(default)]
    subgraph: SubgraphStages,
    /// The connector stage request/response configuration
    #[serde(default)]
    connector: connector::ConnectorStages,
}

/// Configuration for which body fields to send to coprocessor
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, JsonSchema)]
#[serde(untagged)]
pub(super) enum BodyConf {
    /// Send entire body (true) or nothing (false)
    All(bool),
    /// Send specific fields
    Selective(BodyFieldsConf),
}

impl Default for BodyConf {
    fn default() -> Self {
        BodyConf::All(false)
    }
}

impl BodyConf {
    /// Returns true if data or errors fields should be sent
    /// Used to determine if GraphQL spec validation is needed
    pub(super) fn should_send_data_or_errors(&self) -> bool {
        match self {
            BodyConf::All(send) => *send,
            BodyConf::Selective(fields) => fields.data || fields.errors,
        }
    }
}

/// Configuration for selective body fields
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct BodyFieldsConf {
    /// Send the data field
    pub(super) data: bool,
    /// Send the errors field
    pub(super) errors: bool,
    /// Send the extensions field
    pub(super) extensions: bool,
}

/// Configures the context
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, untagged)]
pub(super) enum ContextConf {
    /// Deprecated configuration using a boolean
    Deprecated(bool),
    NewContextConf(NewContextConf),
}

impl ContextConf {
    fn is_deprecated(&self) -> bool {
        match self {
            Self::Deprecated(v) => *v,
            Self::NewContextConf(c) => *c == NewContextConf::Deprecated,
        }
    }
}

impl Default for ContextConf {
    fn default() -> Self {
        Self::Deprecated(false)
    }
}

/// Configures the context
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(super) enum NewContextConf {
    /// Send all context keys to coprocessor
    All,
    /// Send all context keys using deprecated names (from router 1.x) to coprocessor
    Deprecated,
    /// Only send the list of context keys to coprocessor
    Selective(Arc<HashSet<String>>),
}

impl ContextConf {
    pub(crate) fn get_context(&self, ctx: &Context) -> Option<Context> {
        match self {
            Self::NewContextConf(NewContextConf::All) => Some(ctx.clone()),
            Self::NewContextConf(NewContextConf::Deprecated) | Self::Deprecated(true) => {
                let mut new_ctx = Context::from_iter(ctx.iter().map(|elt| {
                    (
                        context_key_to_deprecated(elt.key().clone()),
                        elt.value().clone(),
                    )
                }));
                new_ctx.id = ctx.id.clone();

                Some(new_ctx)
            }
            Self::NewContextConf(NewContextConf::Selective(context_keys)) => {
                let mut new_ctx = Context::from_iter(ctx.iter().filter_map(|elt| {
                    if context_keys.contains(elt.key()) {
                        Some((elt.key().clone(), elt.value().clone()))
                    } else {
                        None
                    }
                }));
                new_ctx.id = ctx.id.clone();

                Some(new_ctx)
            }
            Self::Deprecated(false) => None,
        }
    }
}

fn default_timeout() -> Duration {
    DEFAULT_EXTERNALIZATION_TIMEOUT
}

fn default_response_validation() -> bool {
    true
}

/// Validate a coprocessor URL.
/// Returns an error if the URL is invalid or if it's a Unix socket URL with an empty path.
pub(crate) fn validate_coprocessor_url(url: &str, config_path: &str) -> Result<(), BoxError> {
    if let Some(path) = url.strip_prefix("unix://") {
        if path.is_empty() {
            return Err(format!(
                "{config_path}: Unix socket URL must include a path (e.g., 'unix:///var/run/coprocessor.sock')"
            )
            .into());
        }
        // Basic sanity check - path should be absolute
        if !path.starts_with('/') {
            return Err(format!(
                "{config_path}: Unix socket path should be absolute (e.g., 'unix:///var/run/coprocessor.sock'), got 'unix://{path}'"
            )
            .into());
        }

        // WARN: this might cause us heart burn later, but since filenames can include `?` we
        // should emit a warning if we have a `?` and yet no `path=` rather than return an error
        // and hope that folks see this in their logs if they're getting a bunch of request errors
        if path.contains('?') && !path.contains(PATH_QUERY_PARAM) {
            tracing::warn!(
                "{config_path}: Unix sockets should use valid query parameters if using `?` (e.g., 'unix:///var/run/coprocessor.sock?path=some_path'), got 'unix://{path}'"
            );
        }
    } else {
        // Validate HTTP/HTTPS URLs can be parsed
        url.parse::<http::Uri>()
            .map_err(|e| format!("{config_path}: invalid URL '{url}': {e}"))?;
    }
    Ok(())
}

/// Update the target context based on the context returned from the coprocessor.
/// This function handles both updates/inserts and deletions:
/// - Keys present in the returned context (with non-null values) are updated/inserted
/// - Keys that were sent to the coprocessor but are missing from the returned context are deleted
pub(crate) fn update_context_from_coprocessor(
    target_context: &Context,
    context_returned: Context,
    context_config: &ContextConf,
) -> Result<(), BoxError> {
    // Collect keys that are in the returned context
    let mut keys_returned = HashSet::with_capacity(context_returned.len());

    for (mut key, value) in context_returned.try_into_iter()? {
        // Handle deprecated key names - convert back to actual key names
        if context_config.is_deprecated() {
            key = context_key_from_deprecated(key);
        }

        keys_returned.insert(key.clone());
        target_context.insert_json_value(key, value);
    }

    // Delete keys that were sent but are missing from the returned context
    // If the context config is selective, only delete keys that are in the selective list
    match context_config {
        ContextConf::NewContextConf(NewContextConf::Selective(context_keys)) => {
            target_context.retain(|key, _v| {
                if keys_returned.contains(key) {
                    return true;
                } else if context_keys.contains(key) {
                    return false;
                }
                true
            });
        }
        _ => target_context.retain(|key, _v| keys_returned.contains(key)),
    }

    Ok(())
}

fn record_coprocessor_duration(stage: PipelineStep, duration: Duration) {
    f64_histogram!(
        "apollo.router.operations.coprocessor.duration",
        "Time spent waiting for the coprocessor to answer, in seconds",
        duration.as_secs_f64(),
        coprocessor.stage = stage.to_string()
    );
}

fn record_coprocessor_operation(stage: PipelineStep, succeeded: bool) {
    u64_counter!(
        "apollo.router.operations.coprocessor",
        "Total run operations with co-processors enabled",
        1,
        "coprocessor.stage" = stage.to_string(),
        "coprocessor.succeeded" = succeeded
    );
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default)]
pub(super) struct RouterStage {
    /// The request configuration
    pub(super) request: RouterRequestConf,
    /// The response configuration
    pub(super) response: RouterResponseConf,
}

impl RouterStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: router::BoxService,
        default_url: String,
        sdl: Arc<String>,
        response_validation: bool,
    ) -> router::BoxService
    where
        C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<HttpRequest>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let coprocessor_url = request_config.url.clone().unwrap_or(default_url.clone());
            let http_client = http_client.clone();
            let sdl = sdl.clone();

            AsyncCheckpointLayer::new(move |request: router::Request| {
                let request_config = request_config.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let sdl = sdl.clone();

                async move {
                    let mut succeeded = true;
                    let mut executed = false;
                    let result = process_router_request_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        request,
                        request_config,
                        response_validation,
                        &mut executed,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!("coprocessor: router request stage error: {error}");
                        error
                    });
                    if executed {
                        record_coprocessor_operation(PipelineStep::RouterRequest, succeeded);
                    }
                    result
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();
            let coprocessor_url = response_config.url.clone().unwrap_or(default_url);
            MapFutureLayer::new(move |fut| {
                let sdl = sdl.clone();
                let coprocessor_url = coprocessor_url.clone();
                let http_client = http_client.clone();
                let response_config = response_config.clone();

                async move {
                    let response: router::Response = fut.await?;
                    let mut succeeded = true;
                    let mut executed = false;
                    let result = process_router_response_stage(
                        http_client,
                        coprocessor_url,
                        sdl,
                        response,
                        response_config,
                        response_validation,
                        &mut executed,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!("coprocessor: router response stage error: {error}");
                        error
                    });
                    if executed {
                        record_coprocessor_operation(PipelineStep::RouterResponse, succeeded);
                    };
                    result
                }
            })
        });

        fn external_service_span() -> impl Fn(&router::Request) -> tracing::Span + Clone {
            move |_request: &router::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(router::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            .option_layer(response_layer)
            .buffered() // XXX: Added during backpressure fixing
            .service(service)
            .boxed()
    }
}

// -----------------------------------------------------------------------------------------

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphStages {
    #[serde(default)]
    pub(super) all: SubgraphStage,
}

/// What information is passed to a subgraph request/response stage
#[derive(Clone, Debug, Default, Deserialize, PartialEq, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SubgraphStage {
    #[serde(default)]
    pub(super) request: SubgraphRequestConf,
    #[serde(default)]
    pub(super) response: SubgraphResponseConf,
}

impl SubgraphStage {
    pub(crate) fn as_service<C>(
        &self,
        http_client: C,
        service: subgraph::BoxCloneSyncService,
        default_url: String,
        service_name: String,
        response_validation: bool,
    ) -> subgraph::BoxCloneSyncService
    where
        C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
            + Clone
            + Send
            + Sync
            + 'static,
        <C as tower::Service<HttpRequest>>::Future: Send + 'static,
    {
        let request_layer = (self.request != Default::default()).then_some({
            let request_config = self.request.clone();
            let http_client = http_client.clone();
            let coprocessor_url = request_config.url.clone().unwrap_or(default_url.clone());
            let service_name = service_name.clone();
            AsyncCheckpointLayer::new(move |request: subgraph::Request| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();
                let service_name = service_name.clone();
                let request_config = request_config.clone();

                async move {
                    let mut succeeded = true;
                    let mut executed = false;
                    let result = process_subgraph_request_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        request,
                        request_config,
                        response_validation,
                        &mut executed,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!("coprocessor: subgraph request stage error: {error}");
                        error
                    });
                    if executed {
                        record_coprocessor_operation(PipelineStep::SubgraphRequest, succeeded);
                    }
                    result
                }
            })
        });

        let response_layer = (self.response != Default::default()).then_some({
            let response_config = self.response.clone();
            let coprocessor_url = response_config.url.clone().unwrap_or(default_url);

            MapFutureLayer::new(move |fut| {
                let http_client = http_client.clone();
                let coprocessor_url = coprocessor_url.clone();
                let response_config = response_config.clone();
                let service_name = service_name.clone();

                async move {
                    let response: subgraph::Response = fut.await?;

                    let mut succeeded = true;
                    let mut executed = false;
                    let result = process_subgraph_response_stage(
                        http_client,
                        coprocessor_url,
                        service_name,
                        response,
                        response_config,
                        response_validation,
                        &mut executed,
                    )
                    .await
                    .map_err(|error| {
                        succeeded = false;
                        tracing::error!("coprocessor: subgraph response stage error: {error}");
                        error
                    });
                    if executed {
                        record_coprocessor_operation(PipelineStep::SubgraphResponse, succeeded);
                    }
                    result
                }
            })
        });

        fn external_service_span() -> impl Fn(&subgraph::Request) -> tracing::Span + Clone {
            move |_request: &subgraph::Request| {
                tracing::info_span!(
                    EXTERNAL_SPAN_NAME,
                    "external service" = stringify!(subgraph::Request),
                    "otel.kind" = "INTERNAL"
                )
            }
        }

        ServiceBuilder::new()
            .instrument(external_service_span())
            .option_layer(request_layer)
            .option_layer(response_layer)
            .buffered() // XXX: Added during backpressure fixing
            .service(service)
            .boxed_clone_sync()
    }
}

// -----------------------------------------------------------------------------------------
/// This function receives a mutable `executed` flag so the caller can know
/// whether this stage actually ran before an early return or error. This is
/// required because metric recording happens outside this function.
///
/// Using `&mut` here is not the most idiomatic Rust pattern, but it was the
/// least intrusive way to expose this information without refactoring all
/// router stage processing functions.
async fn process_router_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    mut request: router::Request,
    mut request_config: RouterRequestConf,
    response_validation: bool,
    executed: &mut bool,
) -> Result<ControlFlow<router::Response, router::Request>, BoxError>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    let should_be_executed = request_config
        .condition
        .as_mut()
        .map(|c| c.evaluate_request(&request) == Some(true))
        .unwrap_or(true);
    if !should_be_executed {
        return Ok(ControlFlow::Continue(request));
    }
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.router_request.into_parts();
    let bytes = router::body::into_bytes(body).await?;

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers));

    // HTTP GET requests don't have a body
    let body_to_send = request_config
        .body
        .then(|| String::from_utf8(bytes.to_vec()))
        .transpose()
        .unwrap_or_default();

    let path_to_send = request_config.path.then(|| parts.uri.to_string());

    let context_to_send = request_config.context.get_context(&request.context);
    let sdl_to_send = request_config.sdl.then(|| sdl.clone().to_string());

    let payload = Externalizable::router_builder()
        .stage(PipelineStep::RouterRequest)
        .control(Control::default())
        .id(request.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_sdl(sdl_to_send)
        .and_path(path_to_send)
        .method(parts.method.to_string())
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    // Use a fresh context for the coprocessor HTTP call. The pipeline's request
    // context may carry extensions (eg, AWS SigV4 SigningParamsConfig used in the
    // HttpClientService) intended for subgraph requests, not for the coprocessor
    // endpoint
    //
    // WARN: be careful if you're changing out this context to using the request's context; see
    // above, but also validate what happens downstream for that context
    let co_processor_result = payload
        .call(http_client, &coprocessor_url, Context::new())
        .await;
    // Indicate the stage was executed to raise execution metric on parent
    *executed = true;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::RouterRequest, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let mut co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    // Thirdly, we need to interpret the control flow which may have been
    // updated by our co-processor and decide if we should proceed or stop.

    if matches!(control, Control::Break(_)) {
        // Ensure the code is a valid http status code
        let code = control.get_http_status()?;

        // At this point our body is a String. Try to get a valid JSON value from it
        let body_as_value = co_processor_output
            .body
            .as_ref()
            .and_then(|b| serde_json::from_str(b).ok())
            .unwrap_or(Value::Null);
        // Now we have some JSON, let's see if it's the right "shape" to create a graphql_response.
        // If it isn't, we create a graphql error response
        let graphql_response = match body_as_value {
            Value::Null => graphql::Response::builder()
                .errors(vec![
                    Error::builder()
                        .message(co_processor_output.body.take().unwrap_or_default())
                        .extension_code(COPROCESSOR_ERROR_EXTENSION)
                        .build(),
                ])
                .build(),
            _ => deserialize_coprocessor_response(body_as_value, response_validation),
        };

        let res = router::Response::builder()
            .errors(graphql_response.errors)
            .extensions(graphql_response.extensions)
            .status_code(code)
            .context(request.context);

        let mut res = match (graphql_response.label, graphql_response.data) {
            (Some(label), Some(data)) => res.label(label).data(data).build()?,
            (Some(label), None) => res.label(label).build()?,
            (None, Some(data)) => res.data(data).build()?,
            (None, None) => res.build()?,
        };
        if let Some(headers) = co_processor_output.headers {
            *res.response.headers_mut() = internalize_header_map(headers)?;
        }

        if let Some(context) = co_processor_output.context {
            for (mut key, value) in context.try_into_iter()? {
                if let ContextConf::NewContextConf(NewContextConf::Deprecated) =
                    &request_config.context
                {
                    key = context_key_from_deprecated(key);
                }
                res.context.upsert_json_value(key, move |_current| value);
            }
        }

        return Ok(ControlFlow::Break(res));
    }

    // Finally, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming request with the updated bits if they
    // are present in our co_processor_output.

    let new_body = match co_processor_output.body {
        Some(bytes) => router::body::from_bytes(bytes),
        None => router::body::from_bytes(bytes),
    };

    request.router_request = http::Request::from_parts(parts, new_body);

    if let Some(context) = co_processor_output.context {
        for (mut key, value) in context.try_into_iter()? {
            if let ContextConf::NewContextConf(NewContextConf::Deprecated) = &request_config.context
            {
                key = context_key_from_deprecated(key);
            }
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *request.router_request.headers_mut() = internalize_header_map(headers)?;
    }

    Ok(ControlFlow::Continue(request))
}

/// This function receives a mutable `executed` flag so the caller can know
/// whether this stage actually ran before an early return or error. This is
/// required because metric recording happens outside this function.
///
/// Using `&mut` here is not the most idiomatic Rust pattern, but it was the
/// least intrusive way to expose this information without refactoring all
/// router stage processing functions.
async fn process_router_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    sdl: Arc<String>,
    mut response: router::Response,
    response_config: RouterResponseConf,
    _response_validation: bool, // Router responses don't implement GraphQL validation - streaming responses bypass handle_graphql_response
    executed: &mut bool,
) -> Result<router::Response, BoxError>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    if !response_config.condition.evaluate_response(&response) {
        return Ok(response);
    }
    // split the response into parts + body
    let (parts, body) = response.response.into_parts();

    // we split the body (which is a stream) into first response + rest of responses,
    // for which we will implement mapping later
    let mut stream = body.into_data_stream();
    let first = stream.next().await.transpose()?;
    let rest = stream;

    // If first is None, or contains an error we return an error
    let bytes = match first {
        Some(b) => b,
        None => {
            tracing::error!(
                "Coprocessor cannot convert body into future due to problem with first part"
            );
            return Err(BoxError::from(
                "Coprocessor cannot convert body into future due to problem with first part",
            ));
        }
    };

    // Now we process our first chunk of response
    // Encode headers, body, status, context, sdl to create a payload
    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers));
    let body_to_send = response_config
        .body
        .then(|| std::str::from_utf8(&bytes).map(|s| s.to_string()))
        .transpose()?;
    let status_to_send = response_config.status_code.then(|| parts.status.as_u16());
    let context_to_send = response_config.context.get_context(&response.context);
    let sdl_to_send = response_config.sdl.then(|| sdl.clone().to_string());

    let payload = Externalizable::router_builder()
        .stage(PipelineStep::RouterResponse)
        .id(response.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_sdl(sdl_to_send.clone())
        .build();

    // Second, call our co-processor and get a reply.
    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    // Use a fresh context for the coprocessor HTTP call. The pipeline's request
    // context may carry extensions (eg, AWS SigV4 SigningParamsConfig used in the
    // HttpClientService) intended for subgraph requests, not for the coprocessor
    // endpoint
    //
    // WARN: be careful if you're changing out this context to using the request's context; see
    // above, but also validate what happens downstream for that context
    let co_processor_result = payload
        .call(http_client.clone(), &coprocessor_url, Context::new())
        .await;
    // Indicate the stage was executed to raise execution metric on parent
    *executed = true;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::RouterResponse, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::RouterResponse)?;

    // Third, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming response with the updated bits if they
    // are present in our co_processor_output. If they aren't present, just use the
    // bits that we sent to the co_processor.

    let new_body = match co_processor_output.body {
        Some(bytes) => router::body::from_bytes(bytes),
        None => router::body::from_bytes(bytes),
    };

    response.response = http::Response::from_parts(parts, new_body);

    if let Some(control) = co_processor_output.control {
        *response.response.status_mut() = control.get_http_status()?
    }

    if let Some(context) = co_processor_output.context {
        update_context_from_coprocessor(&response.context, context, &response_config.context)?;
    }

    if let Some(headers) = co_processor_output.headers {
        *response.response.headers_mut() = internalize_header_map(headers)?;
    }

    // Now break our co-processor modified response back into parts
    let (parts, body) = response.response.into_parts();

    // Clone all the bits we need
    let context = response.context.clone();
    let map_context = response.context.clone();

    // Map the rest of our body to process subsequent chunks of response
    let mapped_stream = rest
        .map_err(BoxError::from)
        .and_then(move |deferred_response| {
            let generator_client = http_client.clone();
            let generator_coprocessor_url = coprocessor_url.clone();
            let generator_map_context = map_context.clone();
            let generator_sdl_to_send = sdl_to_send.clone();
            let generator_id = map_context.id.clone();
            let context_conf = response_config.context.clone();

            async move {
                let bytes = deferred_response.to_vec();
                let body_to_send = response_config
                    .body
                    .then(|| String::from_utf8(bytes.clone()))
                    .transpose()?;
                let generator_map_context = generator_map_context.clone();
                let context_to_send = context_conf.get_context(&generator_map_context);

                // Note: We deliberately DO NOT send headers or status_code even if the user has
                // requested them. That's because they are meaningless on a deferred response and
                // providing them will be a source of confusion.
                let payload = Externalizable::router_builder()
                    .stage(PipelineStep::RouterResponse)
                    .id(generator_id)
                    .and_body(body_to_send)
                    .and_context(context_to_send)
                    .and_sdl(generator_sdl_to_send)
                    .build();

                // Second, call our co-processor and get a reply.
                tracing::debug!(?payload, "externalized output");
                // Use a fresh context for the coprocessor HTTP call. The pipeline's request
                // context may carry extensions (eg, AWS SigV4 SigningParamsConfig used in the
                // HttpClientService) intended for subgraph requests, not for the coprocessor
                // endpoint
                //
                // WARN: be careful if you're changing out this context to using the request's context; see
                // above, but also validate what happens downstream for that context
                let co_processor_result = payload
                    .call(generator_client, &generator_coprocessor_url, Context::new())
                    .await;
                tracing::debug!(?co_processor_result, "co-processor returned");
                let co_processor_output = co_processor_result?;

                validate_coprocessor_output(&co_processor_output, PipelineStep::RouterResponse)?;

                // Third, process our reply and act on the contents. Our processing logic is
                // that we replace "bits" of our incoming response with the updated bits if they
                // are present in our co_processor_output. If they aren't present, just use the
                // bits that we sent to the co_processor.
                let final_bytes: Bytes = match co_processor_output.body {
                    Some(bytes) => bytes.into(),
                    None => bytes.into(),
                };

                if let Some(context) = co_processor_output.context {
                    update_context_from_coprocessor(
                        &generator_map_context,
                        context,
                        &context_conf,
                    )?;
                }

                // We return the final_bytes into our stream of response chunks
                Ok(final_bytes)
            }
        });

    // Create our response stream which consists of the bytes from our first body chained with the
    // rest of the responses in our mapped stream.
    let bytes = router::body::into_bytes(body).await.map_err(BoxError::from);
    let final_stream = RouterBody::new(http_body_util::StreamBody::new(
        once(ready(bytes))
            .chain(mapped_stream)
            .map(|b| b.map(http_body::Frame::data).map_err(axum::Error::new)),
    ));

    // Finally, return a response which has a Body that wraps our stream of response chunks
    router::Response::http_response_builder()
        .context(context)
        .response(http::Response::from_parts(parts, final_stream))
        .build()
}
// -----------------------------------------------------------------------------------------------------

/// This function receives a mutable `executed` flag so the caller can know
/// whether this stage actually ran before an early return or error. This is
/// required because metric recording happens outside this function.
///
/// Using `&mut` here is not the most idiomatic Rust pattern, but it was the
/// least intrusive way to expose this information without refactoring all
/// router stage processing functions.
async fn process_subgraph_request_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut request: subgraph::Request,
    mut request_config: SubgraphRequestConf,
    response_validation: bool,
    executed: &mut bool,
) -> Result<ControlFlow<subgraph::Response, subgraph::Request>, BoxError>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    if request_config.condition.evaluate_request(&request) != Some(true) {
        return Ok(ControlFlow::Continue(request));
    }
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our request and prepare our
    // external call. Use our configuration to figure out which data to send.
    let (parts, body) = request.subgraph_request.into_parts();

    let headers_to_send = request_config
        .headers
        .then(|| externalize_header_map(&parts.headers));

    let body_to_send = request_config
        .body
        .then(|| serde_json_bytes::to_value(&body))
        .transpose()?;
    let context_to_send = request_config.context.get_context(&request.context);
    let uri = request_config.uri.then(|| parts.uri.to_string());
    let subgraph_name = service_name.clone();
    let service_name = request_config.service_name.then_some(service_name);
    let subgraph_request_id = request_config
        .subgraph_request_id
        .then_some(request.id.clone());

    let payload = Externalizable::subgraph_builder()
        .stage(PipelineStep::SubgraphRequest)
        .control(Control::default())
        .id(request.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .method(parts.method.to_string())
        .and_service_name(service_name)
        .and_uri(uri)
        .and_subgraph_request_id(subgraph_request_id)
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    // Use a fresh context for the coprocessor HTTP call. The pipeline's request
    // context may carry extensions (eg, AWS SigV4 SigningParamsConfig used in the
    // HttpClientService) intended for subgraph requests, not for the coprocessor
    // endpoint
    //
    // WARN: be careful if you're changing out this context to using the request's context; see
    // above, but also validate what happens downstream for that context
    let co_processor_result = payload
        .call(http_client, &coprocessor_url, Context::new())
        .await;
    // Indicate the stage was executed to raise execution metric on parent
    *executed = true;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::SubgraphRequest, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;
    validate_coprocessor_output(&co_processor_output, PipelineStep::SubgraphRequest)?;
    // unwrap is safe here because validate_coprocessor_output made sure control is available
    let control = co_processor_output.control.expect("validated above; qed");

    // Thirdly, we need to interpret the control flow which may have been
    // updated by our co-processor and decide if we should proceed or stop.

    if matches!(control, Control::Break(_)) {
        // Ensure the code is a valid http status code
        let code = control.get_http_status()?;

        let res = {
            let graphql_response = match co_processor_output.body.unwrap_or(Value::Null) {
                Value::String(s) => graphql::Response::builder()
                    .errors(vec![
                        Error::builder()
                            .message(s.as_str().to_owned())
                            .extension_code(COPROCESSOR_ERROR_EXTENSION)
                            .build(),
                    ])
                    .build(),
                value => deserialize_coprocessor_response(value, response_validation),
            };

            let mut http_response = http::Response::builder()
                .status(code)
                .body(graphql_response)?;
            if let Some(headers) = co_processor_output.headers {
                *http_response.headers_mut() = internalize_header_map(headers)?;
            }

            let subgraph_response = subgraph::Response {
                response: http_response,
                context: request.context,
                subgraph_name,
                id: request.id,
            };

            if let Some(context) = co_processor_output.context {
                for (mut key, value) in context.try_into_iter()? {
                    if let ContextConf::NewContextConf(NewContextConf::Deprecated) =
                        &request_config.context
                    {
                        key = context_key_from_deprecated(key);
                    }
                    subgraph_response
                        .context
                        .upsert_json_value(key, move |_current| value);
                }
            }

            subgraph_response
        };
        return Ok(ControlFlow::Break(res));
    }

    // Finally, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming request with the updated bits if they
    // are present in our co_processor_output.
    let new_body: graphql::Request = match co_processor_output.body {
        Some(value) => serde_json_bytes::from_value(value)?,
        None => body,
    };

    request.subgraph_request = http::Request::from_parts(parts, new_body);

    if let Some(context) = co_processor_output.context {
        for (mut key, value) in context.try_into_iter()? {
            if let ContextConf::NewContextConf(NewContextConf::Deprecated) = &request_config.context
            {
                key = context_key_from_deprecated(key);
            }
            request
                .context
                .upsert_json_value(key, move |_current| value);
        }
    }

    if let Some(headers) = co_processor_output.headers {
        *request.subgraph_request.headers_mut() = internalize_header_map(headers)?;
    }

    if let Some(uri) = co_processor_output.uri {
        *request.subgraph_request.uri_mut() = uri.parse()?;
    }

    Ok(ControlFlow::Continue(request))
}

/// This function receives a mutable `executed` flag so the caller can know
/// whether this stage actually ran before an early return or error. This is
/// required because metric recording happens outside this function.
///
/// Using `&mut` here is not the most idiomatic Rust pattern, but it was the
/// least intrusive way to expose this information without refactoring all
/// router stage processing functions.
async fn process_subgraph_response_stage<C>(
    http_client: C,
    coprocessor_url: String,
    service_name: String,
    mut response: subgraph::Response,
    response_config: SubgraphResponseConf,
    response_validation: bool,
    executed: &mut bool,
) -> Result<subgraph::Response, BoxError>
where
    C: Service<HttpRequest, Response = HttpResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <C as tower::Service<HttpRequest>>::Future: Send + 'static,
{
    if !response_config.condition.evaluate_response(&response) {
        return Ok(response);
    }
    // Call into our out of process processor with a body of our body
    // First, extract the data we need from our response and prepare our
    // external call. Use our configuration to figure out which data to send.

    let (parts, body) = response.response.into_parts();

    let headers_to_send = response_config
        .headers
        .then(|| externalize_header_map(&parts.headers));

    let status_to_send = response_config.status_code.then(|| parts.status.as_u16());

    let body_to_send = filter_graphql_response_body(&body, &response_config.body);
    let context_to_send = response_config.context.get_context(&response.context);
    let service_name = response_config.service_name.then_some(service_name);
    let subgraph_request_id = response_config
        .subgraph_request_id
        .then_some(response.id.clone());

    let payload = Externalizable::subgraph_builder()
        .stage(PipelineStep::SubgraphResponse)
        .id(response.context.id.clone())
        .and_headers(headers_to_send)
        .and_body(body_to_send)
        .and_context(context_to_send)
        .and_status_code(status_to_send)
        .and_service_name(service_name)
        .and_subgraph_request_id(subgraph_request_id)
        .build();

    tracing::debug!(?payload, "externalized output");
    let start = Instant::now();
    // Use a fresh context for the coprocessor HTTP call. The pipeline's request
    // context may carry extensions (eg, AWS SigV4 SigningParamsConfig used in the
    // HttpClientService) intended for subgraph requests, not for the coprocessor
    // endpoint
    //
    // WARN: be careful if you're changing out this context to using the request's context; see
    // above, but also validate what happens downstream for that context
    let co_processor_result = payload
        .call(http_client, &coprocessor_url, Context::new())
        .await;
    // Indicate the stage was executed to raise execution metric on parent
    *executed = true;
    let duration = start.elapsed();
    record_coprocessor_duration(PipelineStep::SubgraphResponse, duration);

    tracing::debug!(?co_processor_result, "co-processor returned");
    let co_processor_output = co_processor_result?;

    validate_coprocessor_output(&co_processor_output, PipelineStep::SubgraphResponse)?;

    // Check if the incoming GraphQL response was valid according to GraphQL spec
    let incoming_payload_was_valid = was_incoming_payload_valid(&body, &response_config.body);

    // Third, process our reply and act on the contents. Our processing logic is
    // that we replace "bits" of our incoming response with the updated bits if they
    // are present in our co_processor_output. If they aren't present, just use the
    // bits that we sent to the co_processor.

    let new_body = handle_graphql_response(
        body,
        co_processor_output.body,
        response_validation,
        incoming_payload_was_valid,
        &response_config.body,
    )?;

    response.response = http::Response::from_parts(parts, new_body);

    if let Some(control) = co_processor_output.control {
        *response.response.status_mut() = control.get_http_status()?
    }

    if let Some(context) = co_processor_output.context {
        update_context_from_coprocessor(&response.context, context, &response_config.context)?;
    }

    if let Some(headers) = co_processor_output.headers {
        *response.response.headers_mut() = internalize_header_map(headers)?;
    }

    Ok(response)
}

// -----------------------------------------------------------------------------------------

fn validate_coprocessor_output<T>(
    co_processor_output: &Externalizable<T>,
    expected_step: PipelineStep,
) -> Result<(), BoxError> {
    if co_processor_output.version != EXTERNALIZABLE_VERSION {
        return Err(BoxError::from(format!(
            "Coprocessor returned the wrong version: expected `{}` found `{}`",
            EXTERNALIZABLE_VERSION, co_processor_output.version,
        )));
    }
    if co_processor_output.stage != expected_step.to_string() {
        return Err(BoxError::from(format!(
            "Coprocessor returned the wrong stage: expected `{}` found `{}`",
            expected_step, co_processor_output.stage,
        )));
    }
    if co_processor_output.control.is_none() && co_processor_output.stage.ends_with("Request") {
        return Err(BoxError::from(format!(
            "Coprocessor response is missing the `control` parameter in the `{}` stage. You must specify \"control\": \"Continue\" or \"control\": \"Break\"",
            co_processor_output.stage,
        )));
    }
    Ok(())
}

/// Convert a HashMap into a HeaderMap
pub(super) fn internalize_header_map(
    input: HashMap<String, Vec<String>>,
) -> Result<HeaderMap<HeaderValue>, BoxError> {
    // better than nothing even though it doesnt account for the values len
    let mut output = HeaderMap::with_capacity(input.len());
    for (k, values) in input
        .into_iter()
        .filter(|(k, _)| k != header::CONTENT_LENGTH.as_str())
    {
        for v in values {
            let key = HeaderName::from_str(k.as_ref())?;
            let value = HeaderValue::from_str(v.as_ref())?;
            output.append(key, value);
        }
    }
    Ok(output)
}

// Helper function to apply common post-processing to deserialized GraphQL responses
fn apply_response_post_processing(
    mut new_body: graphql::Response,
    original_response_body: &graphql::Response,
    body_conf: &BodyConf,
) -> graphql::Response {
    // Needs to take back these 2 fields because it's skipped by serde
    new_body.subscribed = original_response_body.subscribed;
    new_body.created_at = original_response_body.created_at;

    // Preserve fields that weren't sent to the coprocessor
    match body_conf {
        BodyConf::All(true) => {
            // All fields were sent, no need to preserve anything
        }
        BodyConf::All(false) => {
            // Nothing was sent, should not happen in this code path
        }
        BodyConf::Selective(fields) => {
            // Preserve fields that weren't sent
            if !fields.data {
                new_body.data = original_response_body.data.clone();
            }
            if !fields.errors {
                new_body.errors = original_response_body.errors.clone();
            }
            if !fields.extensions {
                new_body.extensions = original_response_body.extensions.clone();
            }
        }
    }

    // Required because for subscription if data is Some(Null) it won't cut the subscription
    // And in some languages they don't have any differences between Some(Null) and Null
    if original_response_body.data == Some(Value::Null)
        && new_body.data.is_none()
        && new_body.subscribed == Some(true)
    {
        new_body.data = Some(Value::Null);
    }
    new_body
}

/// Check if a GraphQL response is minimally valid according to the GraphQL spec.
/// A response is invalid if it has no data AND no errors.
pub(super) fn is_graphql_response_minimally_valid(response: &graphql::Response) -> bool {
    // According to GraphQL spec, a response without data must contain at least one error
    response.data.is_some() || !response.errors.is_empty()
}

/// Check if the incoming payload was valid for conditional validation purposes.
/// Returns true if data/errors were not sent to coprocessor OR if the response is minimally valid.
/// Note: Extensions-only configurations skip GraphQL spec validation since extensions don't affect validity.
pub(super) fn was_incoming_payload_valid(
    response: &graphql::Response,
    body_conf: &BodyConf,
) -> bool {
    if body_conf.should_send_data_or_errors() {
        // If we sent data or errors to the coprocessor, check if it was minimally valid per GraphQL spec
        is_graphql_response_minimally_valid(response)
    } else {
        // If we only sent extensions (or nothing), skip GraphQL spec validation
        true
    }
}

/// Filter a GraphQL response body based on configuration.
/// Returns None if no fields should be sent, or a Value containing only the configured fields.
pub(super) fn filter_graphql_response_body(
    response: &graphql::Response,
    body_conf: &BodyConf,
) -> Option<Value> {
    match body_conf {
        BodyConf::All(false) => None,
        BodyConf::All(true) => {
            Some(serde_json_bytes::to_value(response).expect("serialization will not fail"))
        }
        BodyConf::Selective(fields) => {
            if !fields.data && !fields.errors && !fields.extensions {
                return None;
            }
            let mut obj = serde_json_bytes::Map::new();
            if fields.data {
                if let Some(data) = &response.data {
                    obj.insert("data", data.clone());
                } else {
                    obj.insert("data", Value::Null);
                }
            }
            if fields.errors {
                obj.insert(
                    "errors",
                    serde_json_bytes::to_value(&response.errors)
                        .expect("serialization will not fail"),
                );
            }
            if fields.extensions {
                obj.insert("extensions", Value::Object(response.extensions.clone()));
            }
            Some(Value::Object(obj))
        }
    }
}

/// Deserializes a GraphQL response from a Value with optional validation
pub(super) fn deserialize_coprocessor_response(
    body_as_value: Value,
    response_validation: bool,
) -> graphql::Response {
    if response_validation {
        graphql::Response::from_value(body_as_value).unwrap_or_else(|error| {
            graphql::Response::builder()
                .errors(vec![
                    Error::builder()
                        .message(format!(
                            "couldn't deserialize coprocessor output body: {error}"
                        ))
                        .extension_code(COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION)
                        .build(),
                ])
                .build()
        })
    } else {
        // When validation is disabled, use the old behavior - just deserialize without GraphQL validation
        serde_json_bytes::from_value(body_as_value).unwrap_or_else(|error| {
            graphql::Response::builder()
                .errors(vec![
                    Error::builder()
                        .message(format!(
                            "couldn't deserialize coprocessor output body: {error}"
                        ))
                        .extension_code(COPROCESSOR_DESERIALIZATION_ERROR_EXTENSION)
                        .build(),
                ])
                .build()
        })
    }
}

pub(super) fn handle_graphql_response(
    original_response_body: graphql::Response,
    copro_response_body: Option<Value>,
    response_validation: bool,
    incoming_payload_was_valid: bool,
    body_conf: &BodyConf,
) -> Result<graphql::Response, BoxError> {
    // Enable conditional validation: only validate coprocessor responses when the incoming payload was valid.
    // This prevents validation failures for responses that were already invalid before being sent to the coprocessor.
    // Set to false to restore the previous behavior of always validating coprocessor responses when response_validation is true.
    const ENABLE_CONDITIONAL_VALIDATION: bool = true;

    // Only apply validation if response_validation is enabled AND either:
    // 1. Conditional validation is disabled, OR
    // 2. The incoming payload to the coprocessor was valid
    let should_validate =
        response_validation && (!ENABLE_CONDITIONAL_VALIDATION || incoming_payload_was_valid);

    Ok(match copro_response_body {
        Some(value) => {
            if should_validate {
                let new_body = graphql::Response::from_value(value)?;
                apply_response_post_processing(new_body, &original_response_body, body_conf)
            } else {
                // When validation is disabled, use the old behavior - just deserialize without GraphQL validation
                match serde_json_bytes::from_value::<graphql::Response>(value) {
                    Ok(new_body) => {
                        apply_response_post_processing(new_body, &original_response_body, body_conf)
                    }
                    Err(_) => {
                        // If deserialization fails completely, return original response
                        original_response_body
                    }
                }
            }
        }
        None => original_response_body,
    })
}
