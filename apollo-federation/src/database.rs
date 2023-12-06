use std::sync::Arc;

use apollo_compiler::name;
use apollo_compiler::Schema;

use crate::error::FederationError;
use crate::link::database::links_metadata;
use crate::link::spec::{Identity, APOLLO_SPEC_DOMAIN};
use crate::link::Link;
use crate::subgraph::Subgraphs;
use crate::Supergraph;

// TODO: we should define this as part as some more generic "JoinSpec" definition, but need
// to define the ground work for that in `apollo-at-link` first.
pub fn join_link_identity() -> Identity {
    Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: name!("join"),
    }
}

pub fn join_link(schema: &Schema) -> Arc<Link> {
    links_metadata(schema)
        // TODO: error handling?
        .unwrap_or_default()
        .unwrap_or_default()
        .for_identity(&join_link_identity())
        .expect("The presence of the join link should have been validated on construction")
}

pub fn extract_subgraphs(_supergraph: &Supergraph) -> Result<Subgraphs, FederationError> {
    // TODO
    Ok(Subgraphs::new())
}
