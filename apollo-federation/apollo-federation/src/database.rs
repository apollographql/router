use std::sync::Arc;

use apollo_compiler::Schema;

use crate::link::{
    database::links_metadata,
    spec::{Identity, APOLLO_SPEC_DOMAIN},
    Link,
};
use crate::subgraph::Subgraphs;
use crate::{Supergraph, SupergraphError};

// TODO: we should define this as part as some more generic "JoinSpec" definition, but need
// to define the ground work for that in `apollo-at-link` first.
pub fn join_link_identity() -> Identity {
    Identity {
        domain: APOLLO_SPEC_DOMAIN.to_string(),
        name: "join".to_string(),
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

pub fn extract_subgraphs(_supergraph: &Supergraph) -> Result<Subgraphs, SupergraphError> {
    // TODO
    Ok(Subgraphs::new())
}
