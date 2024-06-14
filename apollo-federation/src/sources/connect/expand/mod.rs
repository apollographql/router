use apollo_compiler::validation::Valid;
use apollo_compiler::NodeStr;
use indexmap::IndexMap;

use super::Connector;
use crate::error::FederationError;
use crate::ApiSchemaOptions;
use crate::Supergraph;

pub enum ExpansionResult {
    Expanded {
        raw_sdl: String,
        api_schema: Valid<apollo_compiler::Schema>,
        connectors_by_service_name: IndexMap<NodeStr, Connector>,
    },
    Unchanged,
}

/// The key to connectors 2024 implementation — when a subgraph contains
/// connectors, we break it up into new subgraphs, one per connector, and
/// merge the result again. This means the query planner can create Fetch
/// Nodes targeting individual connectors.
pub fn expand_connectors(supergraph_str: &str) -> Result<ExpansionResult, FederationError> {
    if !supergraph_str.contains("specs.apollo.dev/connect/v") {
        return Ok(ExpansionResult::Unchanged);
    }

    let supergraph = Supergraph::new(supergraph_str)?;
    let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
        include_defer: true,
        include_stream: true,
    })?;
    let subgraphs = supergraph.extract_subgraphs()?;

    let mut connectors_by_service_name: IndexMap<NodeStr, Connector> = Default::default();
    for (original_subgraph_name, subgraph) in subgraphs {
        match Connector::from_valid_schema(&subgraph.schema, original_subgraph_name.into()) {
            Err(_) => {
                // todo log issue?
            }
            Ok(connectors) if !connectors.is_empty() => {
                for (id, connector) in connectors {
                    connectors_by_service_name.insert(id.derived_subgraph_name(), connector);
                }
            }
            _ => {
                return Ok(ExpansionResult::Unchanged);
            }
        };
    }

    let hack = include_str!("./hack.graphql");

    Ok(ExpansionResult::Expanded {
        raw_sdl: hack.to_string(),
        api_schema: api_schema.schema().clone(),
        connectors_by_service_name,
    })
}
