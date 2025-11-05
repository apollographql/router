mod execution;
mod router;
mod subgraph;
mod supergraph;

use rhai::Engine;

/// Register all context-specific properties and methods on the Rhai engine.
///
/// This registers properties for different pipeline stages:
/// - Router: First stage, can mutate originating HTTP request
/// - Supergraph: After parsing GraphQL, can mutate supergraph request
/// - Execution: During query execution, can mutate supergraph request
/// - Subgraph: Before calling subgraphs, originating request is read-only
pub(super) fn register(engine: &mut Engine) {
    router::register(engine);
    supergraph::register(engine);
    execution::register(engine);
    subgraph::register(engine);
}
