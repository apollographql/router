//! Circuit breaking plugin.
//!
//! Wraps subgraph and connector requests in the apollo-qos
//! [`CircuitBreakerLayer`](apollo_qos::circuit_breaker::CircuitBreakerLayer), so that a
//! dependency which is already failing stops receiving traffic instead of dragging the router
//! down with it. While a circuit is open the router answers the affected fetch immediately with
//! a `503`-flavoured GraphQL error rather than waiting on a call that is unlikely to succeed.
//!
//! Circuits are per target — one per subgraph and one per connector source — and each is
//! configured with an apollo-qos [`CircuitBreakerConfig`], selected by the `all`/`subgraphs` and
//! `connector.all`/`connector.sources` blocks: a target with an entry of its own is governed by
//! that entry, and every other target by `all`.
//!
//! ## What the circuit is allowed to see
//!
//! A circuit is only useful if what it counts is the target's health, so the plugin sits as
//! close to the target as `create_plugins` can put it: inside the plugins that answer requests
//! themselves — the response cache above all — and inside the ones that can fail a request on
//! the router's own account, such as a coprocessor breaking it or a rhai script throwing. Those
//! never reach the circuit, in either direction: a cache hit is served while the circuit is
//! open, because it needs nothing from the target, and a coprocessor that goes down does not
//! open circuits on subgraphs that are answering fine.
//!
//! The classifiers below close the same gap for the router-side rejections that *are* raised
//! under the plugin, by the connector request service itself.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_errors::miette::Diagnostic;
use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_qos::circuit_breaker::CircuitBreakerConfig;
use apollo_qos::circuit_breaker::CircuitBreakerLayer;
use apollo_qos::circuit_breaker::is_circuit_open;
use http::StatusCode;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;
use tower::BoxError;
use tower::ServiceBuilder;
use tower::ServiceExt;

use crate::graphql;
use crate::layers::ServiceBuilderExt;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::SubgraphResponse;
use crate::services::connector;
use crate::services::subgraph;

/// Extension code carried by the GraphQL error returned while a circuit is open.
///
/// Taken from the connector error rather than written out again, so the two paths cannot drift:
/// a rejected connector request gets its code from this same value, by way of
/// [`connector::request_service::Response::error_new`].
fn circuit_breaker_open_code() -> &'static str {
    Error::CircuitBreakerOpen.code()
}

/// Configuration for the circuit breaker plugin.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
// The generated JSON schema puts every definition in one namespace, which already holds
// apollo-qos's `CircuitBreakerConfig`, so the name has to say which plugin it belongs to.
#[schemars(rename = "CircuitBreakerPluginConfig")]
pub(crate) struct Config {
    /// Applied to every subgraph that has no entry of its own under `subgraphs`.
    all: Option<TargetConfig>,
    /// Applied to specific subgraphs, in place of `all` rather than on top of it: a subgraph
    /// listed here takes its options from its own block alone, and the default for every option
    /// that block leaves out. Set `enabled: false` to opt a subgraph out of `all` entirely.
    subgraphs: HashMap<String, TargetConfig>,
    /// Applied to Apollo Connectors requests.
    connector: ConnectorConfig,
}

/// Configuration for circuit breaking on Apollo Connectors requests.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
// The generated JSON schema puts every definition in one namespace, so the name has to say which
// plugin it belongs to.
#[schemars(rename = "CircuitBreakerConnectorConfig")]
struct ConnectorConfig {
    /// Applied to every connector source that has no entry of its own under `sources`.
    all: Option<TargetConfig>,
    /// Applied to specific connector sources, keyed by `<subgraph name>.<source name>`, in place
    /// of `all` rather than on top of it: a source listed here takes its options from its own
    /// block alone, and the default for every option that block leaves out. Set `enabled: false`
    /// to opt a source out of `all` entirely.
    sources: HashMap<String, TargetConfig>,
}

/// Circuit breaking configuration for one subgraph or connector source, or for `all` of them.
///
/// Everything apollo-qos takes, plus the router's own `enabled`, in one block. The switch is the
/// router's because apollo-qos configs are present or absent rather than enabled or disabled:
/// opting a single target out of an `all` block is this plugin's problem to express, and an
/// `enabled` on [`CircuitBreakerConfig`] would be a field apollo-qos could only ignore.
///
/// ## Why the options are held as JSON
///
/// apollo-qos declares the constraints on its options as apollo-configuration attributes, and
/// apollo-configuration checks those against the JSON a configuration arrived as — a Rust value
/// cannot carry them, and [`CircuitBreakerConfig`] has no `Serialize` to turn one back into JSON
/// with. So the block keeps its options as they came and hands them to [`parse_options`], which
/// deserializes them and checks them in the same pass.
///
/// ## Why `additionalProperties` is set by hand
///
/// This is the one config struct in the plugin without `#[serde(deny_unknown_fields)]`, because
/// neither place it could go denies anything:
///
/// - On this struct, serde rejects it outright as a compile error: it cannot know which keys
///   belong to a flattened field.
/// - The one apollo-configuration already puts on [`CircuitBreakerConfig`] cannot fire while the
///   options are flattened. schemars drops the inner `additionalProperties: false` because, kept,
///   it would reject `enabled`.
///
/// So the denial goes in the generated schema instead, and `extend` puts it there. That is the
/// check a user's `router.yaml` meets first, and the only one that can point at the offending key
/// in the document rather than name it in a message. [`parse_options`] catches the same key a
/// second time, over the JSON the options arrived as, which is what a configuration built in code
/// rather than parsed from YAML meets instead.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
// The generated JSON schema puts every definition in one namespace, so the name has to say which
// plugin it belongs to.
#[schemars(rename = "CircuitBreakerTargetConfig")]
#[schemars(extend("additionalProperties" = false))]
struct TargetConfig {
    /// Whether to apply circuit breaking to this target. Set it to `false` to opt a subgraph or
    /// source out of an `all` block, keeping any options set beside it for when it goes back on.
    #[serde(default = "enabled_by_default")]
    #[schemars(default = "enabled_by_default")]
    enabled: bool,

    /// Everything else in the block: the options apollo-qos takes, each defaulting to the
    /// apollo-qos default.
    ///
    /// Deserialized by [`parse_options`] rather than here, for the reason above. The schema is
    /// still [`CircuitBreakerConfig`]'s, so which options a user can set — and the documentation
    /// they read for them — are unchanged by holding them this way.
    #[serde(flatten)]
    #[schemars(with = "CircuitBreakerConfig")]
    options: RawOptions,
}

/// One target block's apollo-qos options, as the JSON they arrived as.
type RawOptions = serde_json::Map<String, serde_json::Value>;

fn enabled_by_default() -> bool {
    true
}

impl TargetConfig {
    /// The apollo-qos options this block asks for, or `None` when it switches circuit breaking
    /// off for the target.
    fn into_options(self) -> Option<RawOptions> {
        self.enabled.then_some(self.options)
    }
}

/// Deserialize one block's apollo-qos options, running every check apollo-configuration makes of
/// them — which nothing else in the router would.
///
/// The router validates its configuration against the JSON schema generated from these types, and
/// that is not the whole of what apollo-configuration checks: the constraints apollo-qos declares
/// with `#[config(validate = …)]` are Rust functions, some of them reading one field against
/// another — `min_requests` against `window_size`. `apollo_configuration::parse_json` runs the
/// schema, deserialize, and rule passes over the options in one go, so the options reach here as
/// JSON rather than as a [`CircuitBreakerConfig`] the plugin has already committed to.
///
/// `path` is where in the router's configuration the block lives, because each message locates
/// the value only within the one block it validated — `/min_requests` — not which target's block
/// that was. It has to name a block the user can go and edit: `circuit_breaker.all` and
/// `circuit_breaker.connector.all` are different blocks, and a message that said only `all`
/// would be as likely to send them to the wrong one as the right one.
fn parse_options(options: RawOptions, path: &str) -> Result<CircuitBreakerConfig, String> {
    apollo_configuration::parse_json(serde_json::Value::Object(options))
        .map_err(|error| format!("{path}: {}", messages(&error).join("; ")))
}

/// Every message a configuration failure carries, in the order apollo-configuration reported them.
///
/// A failure that broke several rules says only "schema validation error" for itself and holds one
/// diagnostic per broken rule beneath it, so the messages that actually name the options are a
/// level down. Recursive because how deep they sit is the diagnostic's to decide, not ours.
fn messages(error: &dyn Diagnostic) -> Vec<String> {
    let nested = error
        .related()
        .map(|related| related.flat_map(messages).collect::<Vec<_>>())
        .unwrap_or_default();

    // A diagnostic with nothing beneath it is the one carrying a message — and so is one whose
    // related iterator turns out to be empty, which would otherwise report nothing at all.
    if nested.is_empty() {
        vec![error.to_string()]
    } else {
        nested
    }
}

/// Where in the router's configuration one [`Circuits`]' blocks live, for error messages.
struct ConfigPaths {
    /// Path of the `all` block.
    all: &'static str,
    /// Path of the map of named targets. Each target's name is appended to it.
    named: &'static str,
}

const SUBGRAPH_PATHS: ConfigPaths = ConfigPaths {
    all: "circuit_breaker.all",
    named: "circuit_breaker.subgraphs",
};

const CONNECTOR_PATHS: ConfigPaths = ConfigPaths {
    all: "circuit_breaker.connector.all",
    named: "circuit_breaker.connector.sources",
};

/// A classifier deciding whether a response the inner service considered successful should still
/// count as a failure against the circuit.
///
/// A function pointer rather than a closure so that the resulting
/// [`CircuitBreakerLayer`] has a nameable type, which the per-target layer cache needs.
type Classifier<Res> = fn(&Res) -> bool;

/// The circuits for one kind of target: all subgraphs, or all connector sources.
///
/// Holds the resolved-and-validated configuration for each target, and caches the layer built
/// for each one. The cache is what makes a circuit a circuit: however many times the router asks
/// for a service for a given target, every one of them has to record its outcomes against the
/// same state.
///
/// The state lives as long as the plugin instance, so it starts empty again whenever the router
/// rebuilds its plugins — on a configuration change or a schema reload — as the per-subgraph
/// state in the traffic shaping plugin does.
struct Circuits<Res> {
    /// Config for targets with no entry of their own, if `all` asked for circuit breaking.
    all: Option<CircuitBreakerConfig>,
    /// Config per explicitly-configured target, which stands in for `all` rather than layering
    /// over it. `None` marks a target switched off with `enabled: false`.
    named: HashMap<String, Option<CircuitBreakerConfig>>,
    classifier: Classifier<Res>,
    layers: Mutex<HashMap<String, CircuitBreakerLayer<Classifier<Res>>>>,
}

impl<Res> Circuits<Res> {
    /// Resolve and validate the configuration for every target, failing router startup rather
    /// than a later request when an option set is invalid.
    ///
    /// Reports every invalid block rather than stopping at the first, so a user with two of them
    /// fixes both in one pass, and in the order the blocks are named in the configuration rather
    /// than in `HashMap` order, so two runs of the same router report the same thing.
    fn new(
        paths: ConfigPaths,
        all: Option<TargetConfig>,
        named: HashMap<String, TargetConfig>,
        classifier: Classifier<Res>,
    ) -> Result<Self, Vec<String>> {
        let mut errors = Vec::new();

        let all = all
            .and_then(TargetConfig::into_options)
            .and_then(|options| {
                parse_options(options, paths.all)
                    .map_err(|e| errors.push(e))
                    .ok()
            });

        let mut named = named.into_iter().collect::<Vec<_>>();
        named.sort_by(|(left, _), (right, _)| left.cmp(right));

        let named = named
            .into_iter()
            .map(|(name, target)| {
                let path = format!("{}.{name}", paths.named);
                let config = target.into_options().and_then(|options| {
                    parse_options(options, &path)
                        .map_err(|e| errors.push(e))
                        .ok()
                });
                (name, config)
            })
            .collect();

        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self {
            all,
            named,
            classifier,
            layers: Mutex::new(HashMap::new()),
        })
    }

    /// The layer protecting `target`, or `None` when circuit breaking is not enabled for it.
    ///
    /// A target with an entry of its own is governed by that entry alone, so an entry carrying
    /// `enabled: false` opts the target out of `all`.
    fn layer(&self, target: &str) -> Option<CircuitBreakerLayer<Classifier<Res>>> {
        let config = match self.named.get(target) {
            Some(config) => config.as_ref(),
            None => self.all.as_ref(),
        }?;

        Some(
            self.layers
                .lock()
                .entry(target.to_string())
                .or_insert_with(|| {
                    CircuitBreakerLayer::new(target, config.clone(), self.classifier)
                })
                .clone(),
        )
    }
}

pub(crate) struct CircuitBreaker {
    subgraphs: Circuits<subgraph::Response>,
    connectors: Circuits<connector::request_service::Response>,
}

#[async_trait::async_trait]
impl PluginPrivate for CircuitBreaker {
    type Config = Config;

    async fn new(init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
        let config = init.config;
        // Both halves are resolved before either is allowed to fail the plugin, so a
        // configuration with an invalid subgraph block *and* an invalid connector block reports
        // both rather than hiding the second behind the first.
        let subgraphs = Circuits::new(
            SUBGRAPH_PATHS,
            config.all,
            config.subgraphs,
            subgraph_response_is_failure,
        );
        let connectors = Circuits::new(
            CONNECTOR_PATHS,
            config.connector.all,
            config.connector.sources,
            connector_response_is_failure,
        );

        match (subgraphs, connectors) {
            (Ok(subgraphs), Ok(connectors)) => Ok(Self {
                subgraphs,
                connectors,
            }),
            (subgraphs, connectors) => {
                let mut errors = subgraphs.err().unwrap_or_default();
                errors.extend(connectors.err().unwrap_or_default());
                Err(errors.join("\n").into())
            }
        }
    }

    fn subgraph_service(
        &self,
        name: &str,
        service: subgraph::BoxCloneService,
    ) -> subgraph::BoxCloneService {
        let Some(layer) = self.subgraphs.layer(name) else {
            return service;
        };

        // The name is the same for every request this service will ever see, so it is read once
        // here rather than cloned off each request: an `Arc` for the per-request capture, and a
        // `String` only for the rare request a circuit actually rejects. The id has to come off
        // the request, which is why there is a request-data closure at all.
        let name = Arc::<str>::from(name);

        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &subgraph::Request| (req.context.clone(), req.id.clone()),
                move |(context, id), future| {
                    let name = name.clone();
                    async move {
                        let response: Result<SubgraphResponse, BoxError> = future.await;
                        match response {
                            Err(err) if is_circuit_open(&*err) => {
                                Ok(SubgraphResponse::error_builder()
                                    .status_code(StatusCode::SERVICE_UNAVAILABLE)
                                    .error(circuit_breaker_open_error())
                                    .context(context)
                                    .subgraph_name(name.to_string())
                                    .id(id)
                                    .build())
                            }
                            _ => response,
                        }
                    }
                },
            )
            .layer(layer)
            .service(service)
            .boxed_clone()
    }

    fn connector_request_service(
        &self,
        service: connector::request_service::BoxCloneService,
        source_name: String,
    ) -> connector::request_service::BoxCloneService {
        let Some(layer) = self.connectors.layer(&source_name) else {
            return service;
        };

        ServiceBuilder::new()
            .map_future_with_request_data(
                |req: &connector::request_service::Request| {
                    (
                        req.context.clone(),
                        req.connector.id.subgraph_name.to_string(),
                        req.key.clone(),
                    )
                },
                |(context, subgraph_name, response_key), future| async move {
                    let response: Result<connector::request_service::Response, BoxError> =
                        future.await;
                    match response {
                        Err(err) if is_circuit_open(&*err) => {
                            Ok(connector::request_service::Response::error_new(
                                context,
                                subgraph_name,
                                Error::CircuitBreakerOpen,
                                CIRCUIT_BREAKER_OPEN_MESSAGE,
                                response_key,
                            ))
                        }
                        _ => response,
                    }
                },
            )
            .layer(layer)
            .service(service)
            .boxed_clone()
    }
}

/// Counts a subgraph response with a 5xx status as a failure against the circuit.
///
/// A 4xx says something about the request rather than about the subgraph's health, and a
/// successful response carrying GraphQL errors says the subgraph answered, so neither counts. A
/// transport-level `Err` from the subgraph always counts as a failure, whatever this returns —
/// though the subgraph service reports a failed fetch as a `500` response rather than an `Err`.
fn subgraph_response_is_failure(response: &subgraph::Response) -> bool {
    response.response.status().is_server_error()
}

/// Counts a connector response as a failure against the circuit when the source failed to answer
/// or answered with a 5xx status.
///
/// Everything the router decided for itself, without asking the source, is left out: the circuit
/// exists to describe the source's health, and a request the router turned away never gave the
/// source a chance to be healthy or otherwise. Counting those lets a `max_requests` cap on one
/// oversized operation open the circuit for every other operation using the source.
///
/// Matched exhaustively on purpose: a new kind of connector failure has to be classified here
/// rather than inheriting whichever side of this an `_` arm happened to fall on.
fn connector_response_is_failure(response: &connector::request_service::Response) -> bool {
    match &response.transport_result {
        // The source was asked and did not answer.
        Err(Error::TransportFailure(_)) => true,
        // The router's own doing: a `max_requests` cap it enforces itself, a timeout or rate
        // limit traffic shaping applied, or a circuit already open. Which of these can even
        // reach a classifier depends on where the plugins sit relative to each other, and that
        // is not something a classifier should have to know.
        Err(
            Error::RequestLimitExceeded
            | Error::RateLimited
            | Error::GatewayTimeout
            | Error::CircuitBreakerOpen,
        ) => false,
        Ok(TransportResponse::Http(http_response)) => http_response.inner.status.is_server_error(),
        // Mapping-only responses never touch the network either.
        Ok(TransportResponse::MappingOnly) => false,
    }
}

const CIRCUIT_BREAKER_OPEN_MESSAGE: &str =
    "Your request was rejected because the circuit breaker for this service is open";

fn circuit_breaker_open_error() -> graphql::Error {
    graphql::Error::builder()
        .message(CIRCUIT_BREAKER_OPEN_MESSAGE)
        .extension_code(circuit_breaker_open_code())
        .build()
}

register_private_plugin!("apollo", "circuit_breaker", CircuitBreaker);

#[cfg(test)]
mod tests;
