mod execution;
mod http;
mod router;
mod service_http;
mod subgraph;
mod supergraph;

use rhai::Engine;

/// Register all context-specific properties and methods on the Rhai engine.
///
/// This registers properties for different pipeline stages:
/// - Http: Raw HTTP layer, can mutate method, uri, headers, body (string)
/// - Router: First stage, can mutate originating HTTP request
/// - Supergraph: After parsing GraphQL, can mutate supergraph request
/// - Execution: During query execution, can mutate supergraph request
/// - Subgraph: Before calling subgraphs, originating request is read-only
/// - ServiceHttp: Outbound service HTTP (service_http), method, uri, headers, body, service_name
pub(super) fn register(engine: &mut Engine) {
    http::register(engine);
    service_http::register(engine);
    router::register(engine);
    supergraph::register(engine);
    execution::register(engine);
    subgraph::register(engine);
}
