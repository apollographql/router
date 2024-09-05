use std::sync::Arc;

use ahash::HashSet;
use apollo_federation::sources::connect::Connector;
use indexmap::IndexMap;
use itertools::Itertools;

pub(crate) const CONNECT_SPAN_NAME: &str = "connect";
pub(crate) const CONNECTOR_TYPE_HTTP: &str = "http";

pub(crate) fn record_connect_metrics(connectors: &IndexMap<Arc<str>, Connector>) {
    connectors
        .values()
        .group_by(|connector| connector.spec)
        .into_iter()
        .for_each(|(spec, connectors)| {
            let mut all_connectors = 0;
            let mut unique_subgraphs = HashSet::default();
            for connector in connectors {
                all_connectors += 1;
                unique_subgraphs.insert(connector.id.subgraph_name.clone());
            }

            u64_counter!(
                "apollo.router.connectors.requests",
                "How many requests have been made to connectors",
                1,
                "connectors.count" = all_connectors,
                "spec.version" = spec.as_str(),
                "spec.subgraphs" = unique_subgraphs.len() as i64
            );
        });
}
