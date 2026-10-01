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
//! Configuring the plugin at all protects every target: a block that is left out is the
//! apollo-qos defaults rather than an absence, so `circuit_breaker: {}` puts a default circuit in
//! front of every subgraph and every connector source. There is no per-target switch; a
//! deployment that wants no circuit breaking leaves the `circuit_breaker` section out, which is
//! what keeps `create_plugins` from building this plugin in the first place.
//!
//! ## Where the options are checked
//!
//! apollo-qos declares the constraints on its options — `min_requests` against `window_size`, the
//! bounds on each of them — as `#[config(validate = …)]` rules. The config structs below are
//! `#[configuration]`, so those rules reach every target's block. The router runs them while it
//! parses its configuration, at startup, on reload and in `router config validate`, and reports
//! each one at the offending key, such as `circuit_breaker.subgraphs.products.min_requests`.
//!
//! ## Where the circuit sits
//!
//! The plugin implements no wrap hook. `pipeline::stages` places its layers itself, and its entry
//! in `create_plugins` decides nothing about where it runs. Every fetch to a target passes through
//! two phases, and the circuit sits between them:
//!
//! - **Admission** is the router deciding whether to send the fetch at all: traffic shaping's
//!   rate limit and load shedding, and a connector's `max_requests`. Those layers sit above the
//!   circuit, so what they turn away never reaches it and is not recorded. Deduplication sits
//!   between admission and the circuit, so one call to a target is one sample, and every request
//!   joined to it receives the same answer, an open circuit's rejection included.
//! - **Execution** is everything done to fulfil an admitted fetch: the target's timeout, every
//!   plugin hook on the fetch's path (Rhai, coprocessors, the response cache, user plugins) and
//!   the call itself. All of it is beneath the circuit, so the circuit judges it. An expired
//!   timeout reaches the circuit as the `504` or `Error::GatewayTimeout` it is rendered into and
//!   counts, a coprocessor or plugin failing the fetch counts like any failure of the target's,
//!   and a response-cache hit counts as a success, which also means an open circuit turns cache
//!   hits away.
//!
//! The classifiers below therefore hold no rule for anything the router did itself: whatever the
//! router turns away is placed above the circuit instead. Telemetry sits above both phases and
//! records every answer, rejections included.
//!
//! A plugin that replaces the target's service instead of forwarding to it, such as
//! `experimental_mock_subgraphs`, stays behind the circuit. Only a request that never reaches a
//! target at all, a mapping-only connector, goes around it, by way of
//! [`Protected`](service::Protected).

use std::collections::HashMap;
use std::sync::Arc;

use apollo_configuration::configuration;
use apollo_federation::connectors::runtime::errors::Error;
use apollo_federation::connectors::runtime::http_json_transport::TransportRequest;
use apollo_federation::connectors::runtime::http_json_transport::TransportResponse;
use apollo_qos::circuit_breaker::CircuitBreakerConfig;
use apollo_qos::circuit_breaker::CircuitBreakerLayer;
use http::StatusCode;
use parking_lot::Mutex;
use tower::BoxError;

use self::service::CircuitLayer;
use self::service::Target;
use crate::graphql;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::services::SubgraphResponse;
use crate::services::connector;
use crate::services::http::IncompleteResponseBody;
use crate::services::subgraph;

mod service;

/// Configuration for the circuit breaker plugin.
#[configuration]
// The generated JSON schema puts every definition in one namespace, which already holds
// apollo-qos's `CircuitBreakerConfig`, so the name has to say which plugin it belongs to.
#[schemars(rename = "CircuitBreakerPluginConfig")]
pub(crate) struct Config {
    /// Applied to every subgraph that has no entry of its own under `subgraphs`.
    all: CircuitBreakerConfig,
    /// Applied to specific subgraphs, in place of `all` rather than on top of it: a subgraph
    /// listed here takes its options from its own block alone, and the default for every option
    /// that block leaves out.
    subgraphs: HashMap<String, CircuitBreakerConfig>,
    /// Applied to Apollo Connectors requests.
    connector: ConnectorConfig,
}

/// Configuration for circuit breaking on Apollo Connectors requests.
#[configuration]
// The generated JSON schema puts every definition in one namespace, so the name has to say which
// plugin it belongs to.
#[schemars(rename = "CircuitBreakerConnectorConfig")]
struct ConnectorConfig {
    /// Applied to every connector source that has no entry of its own under `sources`.
    all: CircuitBreakerConfig,
    /// Applied to specific connector sources, keyed by `<subgraph name>.<source name>`, in place
    /// of `all` rather than on top of it: a source listed here takes its options from its own
    /// block alone, and the default for every option that block leaves out.
    ///
    /// A `@connect` with no `@source` has no source name to be keyed by, so it takes the name
    /// the router synthesizes from the directive's position instead — `products.products_Query_products_0`
    /// for a `@connect` on `Query.products` in the `products` subgraph. Those names are not
    /// meant to be written here: configure sourceless connectors through `all`.
    sources: HashMap<String, CircuitBreakerConfig>,
}

/// A classifier deciding whether a response the inner service considered successful should still
/// count as a failure against the circuit.
///
/// A function pointer rather than a closure so that the resulting
/// [`CircuitBreakerLayer`] has a nameable type, which the per-target layer cache needs.
type Classifier<Res> = fn(&Res) -> bool;

/// The circuits for one kind of target: all subgraphs, or all connector sources.
///
/// Holds the configuration for each target, and caches the layer built for each one. The cache is
/// what makes a circuit a circuit: however many times the router asks for a service for a given
/// target, every one of them has to record its outcomes against the same state.
///
/// The state lives as long as the plugin instance, so it starts empty again whenever the router
/// rebuilds its plugins — on a configuration change or a schema reload — as the per-subgraph
/// state in the traffic shaping plugin does: every circuit comes back closed. Carrying a circuit
/// across a reload would take apollo-qos support, because each circuit owns metric instruments
/// bound to the meter provider of the pipeline that created it.
struct Circuits<Res> {
    /// Config for targets with no entry of their own.
    all: CircuitBreakerConfig,
    /// Config per explicitly-configured target, which stands in for `all` rather than layering
    /// over it.
    named: HashMap<String, CircuitBreakerConfig>,
    classifier: Classifier<Res>,
    layers: Mutex<HashMap<String, CircuitBreakerLayer<Classifier<Res>>>>,
}

impl<Res> Circuits<Res> {
    fn new(
        all: CircuitBreakerConfig,
        named: HashMap<String, CircuitBreakerConfig>,
        classifier: Classifier<Res>,
    ) -> Self {
        Self {
            all,
            named,
            classifier,
            layers: Mutex::new(HashMap::new()),
        }
    }

    /// The layer protecting `target`.
    ///
    /// A target with an entry of its own is governed by that entry alone, and every other target
    /// by `all`.
    fn layer(&self, target: &str) -> CircuitBreakerLayer<Classifier<Res>> {
        let config = self.named.get(target).unwrap_or(&self.all);

        self.layers
            .lock()
            .entry(target.to_string())
            .or_insert_with(|| CircuitBreakerLayer::new(target, config.clone(), self.classifier))
            .clone()
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

        Ok(Self {
            subgraphs: Circuits::new(config.all, config.subgraphs, subgraph_response_is_failure),
            connectors: Circuits::new(
                config.connector.all,
                config.connector.sources,
                connector_response_is_failure,
            ),
        })
    }
}

impl CircuitBreaker {
    /// Returns a layer that puts a subgraph's service behind the circuit named after it.
    pub(crate) fn subgraph_circuit_layer(&self, name: &str) -> CircuitLayer<SubgraphTarget> {
        let target = SubgraphTarget {
            name: Arc::from(name),
        };
        CircuitLayer::new(self.subgraphs.layer(name), target)
    }

    /// Returns a layer that puts a connector source's service behind the circuit named by its
    /// `<subgraph name>.<source name>` key.
    pub(crate) fn connector_source_circuit_layer(
        &self,
        source: &str,
    ) -> CircuitLayer<SourceTarget> {
        CircuitLayer::new(self.connectors.layer(source), SourceTarget)
    }
}

/// A subgraph, behind the circuit named after it.
#[derive(Clone)]
pub(crate) struct SubgraphTarget {
    /// Read once when the service is built, rather than off each request, and turned into the
    /// `String` a response carries only for the rare request a circuit rejects.
    name: Arc<str>,
}

impl Target for SubgraphTarget {
    type Request = subgraph::Request;
    type Response = subgraph::Response;

    fn bypasses_circuit(_request: &subgraph::Request) -> bool {
        false
    }

    fn reject(&self, request: subgraph::Request) -> subgraph::Response {
        SubgraphResponse::error_builder()
            .status_code(StatusCode::SERVICE_UNAVAILABLE)
            .error(circuit_breaker_open_error())
            .context(request.context)
            .subgraph_name(self.name.to_string())
            .id(request.id)
            .build()
    }
}

/// A connector source, behind the circuit named by its `<subgraph name>.<source name>` key.
#[derive(Clone)]
pub(crate) struct SourceTarget;

impl Target for SourceTarget {
    type Request = connector::request_service::Request;
    type Response = connector::request_service::Response;

    /// A mapping-only connector never makes a request, so it has nothing to do with the health
    /// of the source it names. It may still name one, and share that source's circuit, so it
    /// has to go around the circuit rather than be turned away while it is open.
    fn bypasses_circuit(request: &connector::request_service::Request) -> bool {
        matches!(request.transport_request, TransportRequest::MappingOnly)
    }

    /// The rejection has to carry the request's response key, or the whole `ConnectResponse`
    /// fails with an error naming no field: `ConnectorService::execute` joins its request
    /// futures with `?`.
    fn reject(
        &self,
        request: connector::request_service::Request,
    ) -> connector::request_service::Response {
        connector::request_service::Response::error_new(
            request.context,
            request.connector.id.subgraph_name.clone(),
            Error::CircuitBreakerOpen,
            CIRCUIT_BREAKER_OPEN_MESSAGE,
            request.key,
        )
    }
}

/// Counts a subgraph response as a failure against the circuit when it has a 5xx or a `429`
/// status, or when its body was cut off after the headers arrived.
///
/// A `429` says the subgraph is overloaded, which is what the circuit is for. Any other 4xx says
/// something about the request rather than about the subgraph's health, and a successful response
/// carrying GraphQL errors says the subgraph answered, so neither counts. An `Err` from beneath the
/// circuit always counts as a failure, whatever this returns — though the subgraph service reports
/// a failed fetch as a `500` response rather than an `Err`, and a body it failed to read with the
/// status the headers carried, which is why it marks the response with [`IncompleteResponseBody`].
fn subgraph_response_is_failure(response: &subgraph::Response) -> bool {
    status_is_failure(response.response.status())
        || response
            .response
            .extensions()
            .get::<IncompleteResponseBody>()
            .is_some()
}

/// Counts a connector response as a failure against the circuit when the fetch failed during
/// execution, or when the source answered with a 5xx or a `429` status or with a body that never
/// arrived in full.
///
/// Every error a connector response carries from beneath the circuit is execution failing: the
/// source not answering, the target timeout expiring, or a coprocessor or plugin failing the
/// fetch. The errors the router raises when it declines to send a request, such as
/// `Error::RateLimited` and `Error::RequestLimitExceeded`, are raised above the circuit and never
/// reach it.
fn connector_response_is_failure(response: &connector::request_service::Response) -> bool {
    match &response.transport_result {
        Err(_) => true,
        Ok(TransportResponse::Http(http_response)) => {
            status_is_failure(http_response.inner.status)
                || http_response
                    .inner
                    .extensions
                    .get::<IncompleteResponseBody>()
                    .is_some()
        }
        // Mapping-only requests never touch the network, and go around the circuit before they
        // could get here.
        Ok(TransportResponse::MappingOnly) => false,
    }
}

/// A status that says the target failed or is overloaded, rather than that the request was at
/// fault.
fn status_is_failure(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

const CIRCUIT_BREAKER_OPEN_MESSAGE: &str =
    "Your request was rejected because the circuit breaker for this service is open";

/// The error returned while a circuit is open. Its code comes from the connector error, so that
/// rejected subgraph and connector requests cannot drift apart.
fn circuit_breaker_open_error() -> graphql::Error {
    graphql::Error::builder()
        .message(CIRCUIT_BREAKER_OPEN_MESSAGE)
        .extension_code(Error::CircuitBreakerOpen.code())
        .build()
}

register_private_plugin!("apollo", "circuit_breaker", CircuitBreaker);

#[cfg(test)]
mod tests;
