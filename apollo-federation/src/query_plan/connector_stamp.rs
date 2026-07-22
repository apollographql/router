//! Source-aware Phase 1, B-2a: **authoritative connector-coordinate stamping.**
//!
//! The raw-graph planner treats connectors as one ordinary subgraph, so it emits
//! correct plans but discards *which* connector each fetch targets (all share the
//! `connectors` subgraph name). Rather than recover that identity heuristically
//! at execution, this pass determines it **once, authoritatively, at plan time**
//! from the ground-truth [`Connector`] set and stamps it onto each connector
//! fetch's [`FetchNode::connector`] (the B-1 identity channel, which already
//! flows federation → router through the router's plan conversion).
//!
//! Matching is by the fetch's target `(type, field)`:
//! * a root fetch `{ users … }` → the connector on `Query.users`;
//! * an entity fetch `{ _entities … { … on User { d } } }` → the connector on
//!   `User.d`.
//!
//! **Scope of this increment:** root-field connectors and single field-connector
//! entity fetches (the steelthread shapes). Not yet handled, left `None`:
//! * type-level entity-resolver connectors (`@connect` on the type itself);
//! * a single `connectors` fetch that merges fields from *multiple* connectors
//!   (the fetch-merge-keys-on-subgraph-name case) — such a fetch resolves to >1
//!   connector and is deliberately left unstamped rather than guessed.
//! * `Defer`/`Condition`/`Subscription` plan nodes.

use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;

use crate::connectors::Connector;
use crate::query_plan::FetchNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;

/// Stamp every connector fetch in `plan` with its connector coordinate, resolved
/// authoritatively from `connectors`. Fetches to non-connector subgraphs, and
/// connector fetches that resolve ambiguously (see module scope), are left with
/// `connector == None`.
pub fn stamp_connector_coordinates(plan: &mut QueryPlan, connectors: &[Connector]) {
    match plan.node.as_mut() {
        Some(TopLevelPlanNode::Fetch(fetch)) => stamp_fetch(fetch, connectors),
        Some(TopLevelPlanNode::Sequence(seq)) => {
            seq.nodes.iter_mut().for_each(|n| stamp_node(n, connectors))
        }
        Some(TopLevelPlanNode::Parallel(par)) => {
            par.nodes.iter_mut().for_each(|n| stamp_node(n, connectors))
        }
        Some(TopLevelPlanNode::Flatten(flatten)) => stamp_node(&mut flatten.node, connectors),
        Some(TopLevelPlanNode::Defer(_))
        | Some(TopLevelPlanNode::Condition(_))
        | Some(TopLevelPlanNode::Subscription(_))
        | None => {}
    }
}

fn stamp_node(node: &mut PlanNode, connectors: &[Connector]) {
    match node {
        PlanNode::Fetch(fetch) => stamp_fetch(fetch, connectors),
        PlanNode::Sequence(seq) => seq.nodes.iter_mut().for_each(|n| stamp_node(n, connectors)),
        PlanNode::Parallel(par) => par.nodes.iter_mut().for_each(|n| stamp_node(n, connectors)),
        PlanNode::Flatten(flatten) => stamp_node(&mut flatten.node, connectors),
        // Out of scope for this increment (see module docs).
        PlanNode::Defer(_) | PlanNode::Condition(_) => {}
    }
}

fn stamp_fetch(fetch: &mut FetchNode, connectors: &[Connector]) {
    let Ok(doc) = fetch.operation_document.as_parsed() else {
        return;
    };
    let Ok(op) = doc.operations.get(None) else {
        return;
    };

    let mut targets: Vec<(String, String)> = Vec::new();
    collect_targets(op.selection_set.ty.as_str(), &op.selection_set, &mut targets);

    // Match targets against connectors in *this* fetch's subgraph.
    let subgraph = fetch.subgraph_name.as_ref();
    let mut matched: Vec<&Connector> = Vec::new();
    for (parent_type, field) in &targets {
        let simple = format!("{parent_type}.{field}");
        for connector in connectors {
            if connector.id.subgraph_name.as_str() == subgraph
                && connector.id.directive.simple_name() == simple
                && !matched.iter().any(|m| m.id.coordinate() == connector.id.coordinate())
            {
                matched.push(connector);
            }
        }
    }

    // Exactly one connector → carry its identity. Zero (non-connector subgraph)
    // or many (merged multi-connector fetch, out of scope) → leave None.
    if let [connector] = matched.as_slice() {
        fetch.connector = Some(connector.id.coordinate());
    }
}

/// Collect the `(parent_type, field)` pairs a fetch operation targets. Root
/// fields yield `(root_type, field)`; an `_entities` field descends into its
/// inline fragments, yielding `(fragment_type, field)` for each non-`__typename`
/// field.
fn collect_targets(parent_type: &str, selection_set: &SelectionSet, out: &mut Vec<(String, String)>) {
    for selection in &selection_set.selections {
        match selection {
            Selection::Field(field) => {
                if field.name.as_str() == "_entities" {
                    collect_targets(parent_type, &field.selection_set, out);
                } else {
                    out.push((parent_type.to_string(), field.name.to_string()));
                }
            }
            Selection::InlineFragment(fragment) => {
                let ty = fragment
                    .type_condition
                    .as_ref()
                    .map(|t| t.as_str())
                    .unwrap_or(parent_type);
                for inner in &fragment.selection_set.selections {
                    if let Selection::Field(field) = inner {
                        if field.name.as_str() != "__typename" {
                            out.push((ty.to_string(), field.name.to_string()));
                        }
                    }
                }
            }
            Selection::FragmentSpread(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ExecutableDocument;
    use apollo_compiler::Schema;

    use super::*;
    use crate::ApiSchemaOptions;
    use crate::Supergraph;
    use crate::query_plan::query_planner::QueryPlanner;
    use crate::query_plan::query_planner::QueryPlannerConfig;
    use crate::query_graph::build_federated_query_graph;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    /// Collect `(subgraph, connector_coordinate_option)` for every fetch in a
    /// (possibly stamped) plan, in traversal order.
    fn fetch_stamps(plan: &QueryPlan) -> Vec<(String, Option<String>)> {
        fn walk(node: &PlanNode, out: &mut Vec<(String, Option<String>)>) {
            match node {
                PlanNode::Fetch(f) => {
                    out.push((f.subgraph_name.to_string(), f.connector.clone()))
                }
                PlanNode::Sequence(s) => s.nodes.iter().for_each(|n| walk(n, out)),
                PlanNode::Parallel(p) => p.nodes.iter().for_each(|n| walk(n, out)),
                PlanNode::Flatten(fl) => walk(&fl.node, out),
                PlanNode::Defer(_) | PlanNode::Condition(_) => {}
            }
        }
        let mut out = Vec::new();
        match &plan.node {
            Some(TopLevelPlanNode::Fetch(f)) => {
                out.push((f.subgraph_name.to_string(), f.connector.clone()))
            }
            Some(TopLevelPlanNode::Sequence(s)) => s.nodes.iter().for_each(|n| walk(n, &mut out)),
            Some(TopLevelPlanNode::Parallel(p)) => p.nodes.iter().for_each(|n| walk(n, &mut out)),
            Some(TopLevelPlanNode::Flatten(fl)) => walk(&fl.node, &mut out),
            _ => {}
        }
        out
    }

    #[test]
    fn stamps_connector_coordinates_over_raw_graph_plans() {
        let sdl = include_str!("../connectors/expand/tests/schemas/expand/steelthread.graphql");

        // Raw-graph planner (connectors treated as one ordinary subgraph).
        let supergraph = Supergraph::new_with_router_specs(sdl).unwrap();
        let api = supergraph.to_api_schema(ApiSchemaOptions::default()).unwrap();
        let graph = build_federated_query_graph(
            supergraph.schema.clone(),
            api.clone(),
            Some(false),
            Some(true),
        )
        .unwrap();
        let planner = QueryPlanner::from_query_graph(
            QueryPlannerConfig::default(),
            graph,
            supergraph.schema.clone(),
            api.clone(),
        )
        .unwrap();

        // Ground-truth connectors, built directly from the raw subgraphs.
        let fed = FederationSchema::new(Schema::parse(sdl, "s.graphql").unwrap()).unwrap();
        let subgraphs = extract_subgraphs_from_supergraph(&fed, Some(false)).unwrap();
        let connectors: Vec<Connector> = subgraphs
            .subgraphs
            .values()
            .flat_map(|sg| Connector::from_schema(sg.schema.schema(), &sg.name).unwrap_or_default())
            .collect();
        assert!(!connectors.is_empty(), "steelthread has connectors");

        // Root-field query: the single connectors fetch should be stamped with
        // the Query.users connector coordinate.
        {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ users { id name } }",
                "q.graphql",
            )
            .unwrap();
            let mut plan = planner.build_query_plan(&doc, None, Default::default()).unwrap();
            stamp_connector_coordinates(&mut plan, &connectors);
            let stamps = fetch_stamps(&plan);
            let connector_stamp = stamps
                .iter()
                .find(|(sg, _)| sg == "connectors")
                .expect("a connectors fetch");
            assert!(
                connector_stamp
                    .1
                    .as_deref()
                    .is_some_and(|c| c.contains("Query.users")),
                "root-field connectors fetch stamped with Query.users coordinate, got {stamps:?}"
            );
        }

        // Entity + @requires query: the connectors root fetch → Query.user, the
        // connectors entity fetch → User.d, and the graphql entity fetch (c) is
        // NOT a connector so stays None.
        {
            let doc = ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ user(id: \"1\") { name d } }",
                "q.graphql",
            )
            .unwrap();
            let mut plan = planner.build_query_plan(&doc, None, Default::default()).unwrap();
            stamp_connector_coordinates(&mut plan, &connectors);
            let stamps = fetch_stamps(&plan);

            // Every graphql fetch is left unstamped (not a connector).
            for (sg, coord) in &stamps {
                if sg == "graphql" {
                    assert_eq!(coord, &None, "graphql fetch must not be stamped: {stamps:?}");
                }
            }
            // The connector entity fetch for `d` is stamped with User.d.
            assert!(
                stamps
                    .iter()
                    .any(|(sg, c)| sg == "connectors"
                        && c.as_deref().is_some_and(|c| c.contains("User.d"))),
                "connector entity fetch stamped with User.d coordinate, got {stamps:?}"
            );
            // And the connectors root fetch for `user` is stamped with Query.user.
            assert!(
                stamps
                    .iter()
                    .any(|(sg, c)| sg == "connectors"
                        && c.as_deref().is_some_and(|c| c.contains("Query.user["))),
                "connectors root fetch stamped with Query.user coordinate, got {stamps:?}"
            );
        }
    }
}
