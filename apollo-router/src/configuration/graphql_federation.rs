//! Configuration of GraphQL Federation support (source schemas resolving entities through
//! `@lookup` fields). Preview.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

use crate::configuration::subgraph::SubgraphConfiguration;

/// GraphQL Federation (formerly composite schemas) support: supergraphs that include source
/// schemas, whose entities are resolved through `@lookup` fields. Preview; requires the
/// incremental query planner.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct GraphqlFederation {
    /// Allow supergraphs that include GraphQL Federation source schemas.
    pub(crate) enabled: bool,
    /// How lookup requests are sent to each subgraph.
    pub(crate) subgraph: SubgraphConfiguration<LookupBatching>,
}

/// Batching of lookup requests to a subgraph, following the draft "Batching" appendix of the
/// GraphQL-over-HTTP specification. The subgraph must support it: responses are read as
/// `application/jsonl`, one result per line with `variableIndex` and `requestIndex`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub(crate) struct LookupBatching {
    /// Send all the entities of a lookup fetch in one request, with a list of variable sets
    /// (variable batching). When disabled, each entity is fetched with its own request.
    pub(crate) variable_batching: bool,
    /// Combine the lookup fetches to this subgraph that run at the same time into one HTTP
    /// request (request batching).
    pub(crate) request_batching: bool,
    /// The maximum number of operations (variable sets) in one batched HTTP request. Larger
    /// batches are split into several requests.
    pub(crate) maximum_size: Option<usize>,
}
