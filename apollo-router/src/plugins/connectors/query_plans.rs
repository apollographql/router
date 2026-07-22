use std::sync::Arc;

use apollo_federation::connectors::Connector;
use indexmap::IndexMap;

use crate::Context;
use crate::query_planner::PlanNode;

type ConnectorsByServiceName = Arc<IndexMap<Arc<str>, Connector>>;

pub(crate) fn store_connectors(
    context: &Context,
    connectors_by_service_name: Arc<IndexMap<Arc<str>, Connector>>,
) {
    context
        .extensions()
        .with_lock(|lock| lock.insert::<ConnectorsByServiceName>(connectors_by_service_name));
}

pub(crate) fn get_connectors(context: &Context) -> Option<ConnectorsByServiceName> {
    context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorsByServiceName>().cloned())
}

/// Source-aware connector dispatch keyed by stable connector **coordinate**
/// (`ConnectId::coordinate`) instead of by synthetic subgraph service name.
///
/// In an expanded supergraph every connector becomes its own synthetic
/// subgraph, so a fetch node's `service_name` uniquely identifies the connector
/// — that is how `ConnectorService::call` (`connector_service.rs`) selects it.
/// Source-aware planning collapses *all* connectors into a single `connectors`
/// subgraph, so `service_name` is `"connectors"` for every connector fetch and
/// no longer disambiguates them. The `ConnectFetchDescriptor.coordinate`
/// (`apollo_federation::connectors::source_aware`) does. This is the lookup
/// that source-aware dispatch keys on in place of `ConnectorsByServiceName`.
///
/// Deliberately no `Context` store/get accessor yet: that plumbing lands
/// together with the source-aware fetch-node producer that reads it, so we
/// don't introduce an unexercised recording path.
type ConnectorsByCoordinate = Arc<IndexMap<String, Connector>>;

/// Re-index a connector set by coordinate. Coordinates are unique per connector
/// (`ConnectId::coordinate` includes the connect-directive index), so the
/// re-index is lossless — one entry per connector.
// Not yet wired into a production caller (only the source-aware dispatch path,
// which does not exist in the router yet, will read it); exercised by tests.
#[allow(dead_code)]
pub(crate) fn connectors_by_coordinate(
    connectors_by_service_name: &ConnectorsByServiceName,
) -> ConnectorsByCoordinate {
    Arc::new(
        connectors_by_service_name
            .values()
            .map(|connector| (connector.id.coordinate(), connector.clone()))
            .collect(),
    )
}

type ConnectorLabels = Arc<IndexMap<Arc<str>, String>>;

pub(crate) fn store_connectors_labels(context: &Context, labels_by_service_name: ConnectorLabels) {
    context
        .extensions()
        .with_lock(|lock| lock.insert(labels_by_service_name));
}

pub(crate) fn replace_connector_service_names_text(
    text: Option<Arc<String>>,
    context: &Context,
) -> Option<Arc<String>> {
    let replacements = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorLabels>().cloned());
    if let Some(replacements) = replacements {
        text.as_ref().map(|text| {
            let mut text = text.to_string();
            for (service_name, label) in replacements.iter() {
                text = text.replace(&**service_name, label.as_ref());
            }
            Arc::new(text)
        })
    } else {
        text
    }
}

pub(crate) fn replace_connector_service_names(
    plan: Arc<PlanNode>,
    context: &Context,
) -> Arc<PlanNode> {
    let replacements = context
        .extensions()
        .with_lock(|lock| lock.get::<ConnectorLabels>().cloned());

    return if let Some(replacements) = replacements {
        let mut plan = plan.clone();
        recurse(Arc::make_mut(&mut plan), &replacements);
        plan
    } else {
        plan
    };

    fn recurse(plan: &mut PlanNode, replacements: &IndexMap<Arc<str>, String>) {
        match plan {
            PlanNode::Sequence { nodes } => {
                for node in nodes {
                    recurse(node, replacements);
                }
            }
            PlanNode::Parallel { nodes } => {
                for node in nodes {
                    recurse(node, replacements);
                }
            }
            PlanNode::Fetch(node) => {
                if let Some(service_name) = replacements.get(&node.service_name) {
                    node.service_name = service_name.clone().into();
                }
            }
            PlanNode::Flatten(flatten) => {
                recurse(&mut flatten.node, replacements);
            }
            PlanNode::Defer { primary, deferred } => {
                if let Some(primary) = primary.node.as_mut() {
                    recurse(primary, replacements);
                }
                for deferred in deferred {
                    if let Some(node) = &mut deferred.node {
                        recurse(Arc::make_mut(node), replacements);
                    }
                }
            }
            PlanNode::Subscription { primary: _, rest } => {
                // ignoring subscriptions because connectors are not supported
                if let Some(node) = rest {
                    recurse(node, replacements);
                }
            }
            PlanNode::Condition {
                if_clause,
                else_clause,
                ..
            } => {
                if let Some(if_clause) = if_clause.as_mut() {
                    recurse(if_clause, replacements);
                }
                if let Some(else_clause) = else_clause.as_mut() {
                    recurse(else_clause, replacements);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Name;
    use apollo_compiler::Schema;
    use apollo_compiler::name;
    use apollo_federation::connectors::ConnectId;
    use apollo_federation::connectors::ConnectSpec;
    use apollo_federation::connectors::Connector;
    use apollo_federation::connectors::HttpJsonTransport;
    use apollo_federation::connectors::JSONSelection;
    use indexmap::IndexMap;

    use super::*;

    /// A minimal root-field connector on `Query.<field>`, shaped like the
    /// connectors the source-aware planner emits fetches for. `field` drives the
    /// coordinate (`connectors:Query.<field>[0]`).
    fn root_field_connector(field: &str) -> Connector {
        let schema = Schema::parse_and_validate("type Query { hello: String }", "./").unwrap();
        Connector {
            spec: ConnectSpec::V0_1,
            schema_subtypes_map: Connector::subtypes_map_from_schema(&schema),
            id: ConnectId::new(
                "connectors".into(),
                None,
                name!(Query),
                Name::new(field).unwrap(),
                None,
                0,
            ),
            transport: Some(HttpJsonTransport {
                source_template: "http://localhost/api".parse().ok(),
                connect_template: format!("/{field}").parse().unwrap(),
                ..Default::default()
            }),
            selection: JSONSelection::parse("id name").unwrap(),
            entity_resolver: None,
            config: Default::default(),
            max_requests: None,
            batch_settings: None,
            request_headers: Default::default(),
            response_headers: Default::default(),
            request_variable_keys: Default::default(),
            response_variable_keys: Default::default(),
            error_settings: Default::default(),
            label: "test".into(),
        }
    }

    /// Slice 1 of the source-aware fetch seam: dispatch must key on connector
    /// coordinate, not synthetic service name. Prove the re-index is lossless
    /// and that a connector resolves by its coordinate.
    #[test]
    fn connectors_re_index_by_coordinate() {
        let users = root_field_connector("users");
        let posts = root_field_connector("posts");
        let users_coord = users.id.coordinate();
        let posts_coord = posts.id.coordinate();
        assert_ne!(users_coord, posts_coord, "coordinates must disambiguate");

        // In the expanded world these would be keyed by *distinct* synthetic
        // subgraph names; here we stand those in with the coordinates themselves
        // — the point is that re-indexing recovers a coordinate-keyed lookup
        // regardless of the service-name keys.
        let by_service_name: ConnectorsByServiceName = Arc::new(IndexMap::from_iter([
            (Arc::from("connectors_Query_users_0"), users),
            (Arc::from("connectors_Query_posts_0"), posts),
        ]));

        let by_coordinate = connectors_by_coordinate(&by_service_name);

        // Lossless: one entry per connector.
        assert_eq!(by_coordinate.len(), by_service_name.len());
        // Resolves the right connector by coordinate — the source-aware dispatch key.
        let resolved = by_coordinate
            .get(&users_coord)
            .expect("users connector resolvable by coordinate");
        assert_eq!(resolved.id.coordinate(), users_coord);
        assert!(by_coordinate.contains_key(&posts_coord));
    }
}
