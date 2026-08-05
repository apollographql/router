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
//! **Entity-resolver connectors (source-aware Phase 1, step 3).** Some entity
//! fetches select fields that are *not* themselves connector-backed — e.g.
//! `{ _entities … { … on User { username } } }`, where `username` has no
//! `User.username` connector but is resolved by pulling the whole `User` through
//! `Query.user @connect(entity: true)`. Such a fetch is stamped by matching the
//! resolver connector's *output type* to the entity type (`User`), not by
//! `(type, field)`. Scoped to [`EntityResolver::Explicit`] (a root-type field
//! resolving an entity); see the note under **Scope**.
//!
//! **Multi-connector merged fetches (source-aware Phase 1, step 2A).** Because
//! the raw-graph planner sees connectors as one subgraph, it merges root fields
//! backed by *different* connectors into a single `connectors` fetch (under
//! expansion these were distinct synthetic subgraphs → distinct fetches). Such a
//! fetch has no single connector identity, so instead of leaving it unstamped
//! (which mis-dispatches), [`stamp_connector_coordinates`] **splits** it into a
//! `Parallel` of per-connector fetches, each with its own sub-operation and
//! stamped coordinate — reconstructing, in the plan, the fetch decomposition
//! expansion used to get structurally. The unchanged Parallel/Sequence executor
//! then fans them out. See `split_root_field_fetch` in this module for the
//! (deliberately bounded) shapes handled.
//!
//! **Scope.** Handled: root-field connectors, single field-connector entity
//! fetches, multi-connector *root-field* merges (the split above), and
//! `Explicit` entity-resolver connectors (the output-type match above). Not yet
//! handled, left `None`:
//! * type-level entity-resolver connectors (`@connect` on the type itself —
//!   `TypeBatch`/`TypeSingle`);
//! * multi-connector merges involving *entity* fields (shared representations)
//!   or bound variables — left unsplit rather than split incorrectly;
//! * `Defer`/`Condition`/`Subscription` plan nodes.

use std::collections::HashSet;
use std::sync::Arc;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::executable::OperationType;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use shape::ShapeCase;

use crate::connectors::Connector;
use crate::connectors::EntityResolver;
use crate::connectors::SelectionAnalysis;
use crate::query_plan::FetchDataPathElement;
use crate::query_plan::FetchDataRewrite;
use crate::query_plan::FetchNode;
use crate::query_plan::ParallelNode;
use crate::query_plan::PlanNode;
use crate::query_plan::QueryPlan;
use crate::query_plan::TopLevelPlanNode;
use crate::query_plan::serializable_document::SerializableDocument;

/// Stamp every connector fetch in `plan` with its connector coordinate, resolved
/// authoritatively from `connectors`. Fetches to non-connector subgraphs, and
/// connector fetches that resolve ambiguously (see module scope), are left with
/// `connector == None`.
pub fn stamp_connector_coordinates(
    plan: &mut QueryPlan,
    connectors: &[Connector],
    schema: &Schema,
) {
    match plan.node.take() {
        Some(TopLevelPlanNode::Fetch(fetch)) => {
            let fetch = *fetch;
            plan.node = Some(match split_root_field_fetch(&fetch, connectors) {
                Some(split) => TopLevelPlanNode::Parallel(ParallelNode {
                    nodes: split
                        .into_iter()
                        .map(|f| PlanNode::Fetch(Box::new(f)))
                        .collect(),
                }),
                None => {
                    let mut fetch = fetch;
                    stamp_fetch(&mut fetch, connectors, schema);
                    TopLevelPlanNode::Fetch(Box::new(fetch))
                }
            });
        }
        Some(mut other) => {
            match &mut other {
                TopLevelPlanNode::Sequence(seq) => seq
                    .nodes
                    .iter_mut()
                    .for_each(|n| stamp_node(n, connectors, schema)),
                TopLevelPlanNode::Parallel(par) => par
                    .nodes
                    .iter_mut()
                    .for_each(|n| stamp_node(n, connectors, schema)),
                TopLevelPlanNode::Flatten(flatten) => {
                    stamp_node(&mut flatten.node, connectors, schema)
                }
                TopLevelPlanNode::Fetch(_) => unreachable!("handled above"),
                TopLevelPlanNode::Defer(_)
                | TopLevelPlanNode::Condition(_)
                | TopLevelPlanNode::Subscription(_) => {}
            }
            plan.node = Some(other);
        }
        None => {}
    }
}

fn stamp_node(node: &mut PlanNode, connectors: &[Connector], schema: &Schema) {
    match node {
        PlanNode::Fetch(_) => {
            // Take the fetch out so we can either stamp it in place or replace
            // the whole node with a split `Parallel` (avoids a borrow conflict).
            let placeholder = PlanNode::Parallel(ParallelNode { nodes: Vec::new() });
            let PlanNode::Fetch(fetch) = std::mem::replace(node, placeholder) else {
                unreachable!()
            };
            let fetch = *fetch;
            *node = match split_root_field_fetch(&fetch, connectors) {
                Some(split) => PlanNode::Parallel(ParallelNode {
                    nodes: split
                        .into_iter()
                        .map(|f| PlanNode::Fetch(Box::new(f)))
                        .collect(),
                }),
                None => {
                    let mut fetch = fetch;
                    stamp_fetch(&mut fetch, connectors, schema);
                    PlanNode::Fetch(Box::new(fetch))
                }
            };
        }
        PlanNode::Sequence(seq) => seq
            .nodes
            .iter_mut()
            .for_each(|n| stamp_node(n, connectors, schema)),
        PlanNode::Parallel(par) => par
            .nodes
            .iter_mut()
            .for_each(|n| stamp_node(n, connectors, schema)),
        PlanNode::Flatten(flatten) => stamp_node(&mut flatten.node, connectors, schema),
        // Out of scope for this increment (see module docs).
        PlanNode::Defer(_) | PlanNode::Condition(_) => {}
    }
}

fn stamp_fetch(fetch: &mut FetchNode, connectors: &[Connector], schema: &Schema) {
    let Ok(doc) = fetch.operation_document.as_parsed() else {
        return;
    };
    let Ok(op) = doc.operations.get(None) else {
        return;
    };

    let mut targets: Vec<(String, String)> = Vec::new();
    collect_targets(
        op.selection_set.ty.as_str(),
        &op.selection_set,
        &mut targets,
    );

    // Match targets against connectors in *this* fetch's subgraph.
    let subgraph = fetch.subgraph_name.as_ref();
    let mut matched: Vec<&Connector> = Vec::new();
    for (parent_type, field) in &targets {
        let simple = format!("{parent_type}.{field}");
        for connector in connectors {
            if connector.id.subgraph_name.as_str() == subgraph
                && connector.id.directive.simple_name() == simple
                && !matched
                    .iter()
                    .any(|m| m.id.coordinate() == connector.id.coordinate())
            {
                matched.push(connector);
            }
        }
    }

    // Field-connector match: exactly one connector → carry its identity. Zero
    // (non-connector subgraph) → fall through to the entity-resolver match below.
    // The many-connector case is handled earlier by `split_root_field_fetch`, so
    // a fetch reaching here with >1 match is one we deliberately don't split (see
    // that function's guards) and is left None.
    if let [connector] = matched.as_slice() {
        fetch.connector = Some(connector.id.coordinate());
        return;
    }
    if !matched.is_empty() {
        return;
    }

    // Entity-resolver match (step 3). An `_entities` fetch whose selected fields
    // are *not* themselves connector-backed (so field matching above found
    // nothing) is served by the connector that resolves the whole entity — e.g.
    // `username` on `User` is resolved through `Query.user @connect(entity: true)`
    // (`EntityResolver::Explicit`), whose `simple_name` is `Query.user`, not
    // `User.username`. Match by the entity's *output type* instead of by
    // `(parent_type, field)`.
    //
    // Scope: `EntityResolver::Explicit` only (a root-type field resolving an
    // entity). Type-level entity-resolver connectors (`TypeBatch`/`TypeSingle`,
    // `@connect` on the type itself) remain a documented follow-on. `Implicit`
    // resolvers are field connectors and are already handled by the field match
    // above.
    let entity_types: HashSet<&str> = targets.iter().map(|(t, _)| t.as_str()).collect();
    let [entity_type] = entity_types.into_iter().collect::<Vec<_>>()[..] else {
        // Zero or multiple entity types in one fetch → no single identity; leave
        // None (documented scope).
        return;
    };
    let mut resolvers: Vec<&Connector> = Vec::new();
    for connector in connectors {
        if connector.id.subgraph_name.as_str() == subgraph
            && matches!(connector.entity_resolver, Some(EntityResolver::Explicit))
            && connector
                .id
                .directive
                .base_type_name(schema)
                .is_some_and(|t| t.as_str() == entity_type)
            && !resolvers
                .iter()
                .any(|r| r.id.coordinate() == connector.id.coordinate())
        {
            resolvers.push(connector);
        }
    }
    if let [connector] = resolvers.as_slice() {
        fetch.connector = Some(connector.id.coordinate());
    }
}

/// The set of top-level fields a connector's `selection` actually returns on its
/// output object, derived via the [`SelectionAnalysis`] caching pathway. This is
/// the per-connector field availability that expansion used to encode
/// structurally (a minimal synthetic subgraph exposing only these fields); the
/// collapsed source-aware graph loses it, so we recompute it from the selection.
///
/// `__typename` is excluded (always implicitly available). Returns `None` when
/// the output shape is not an object (e.g. a field connector returning a scalar)
/// — those are not the root/entity over-merge case this guards.
pub(crate) fn connector_provided_fields(connector: &Connector) -> Option<HashSet<String>> {
    let analysis = SelectionAnalysis::new(connector.selection.clone());
    let shape = analysis.output_shape();
    match shape.case() {
        ShapeCase::Object { fields, .. } => Some(
            fields
                .iter()
                .map(|(name, _)| name.to_string())
                .filter(|name| name != "__typename")
                .collect(),
        ),
        _ => None,
    }
}

/// The connector on `{parent_type}.{field}` within `subgraph`, if any.
fn connector_for<'a>(
    parent_type: &str,
    field: &str,
    subgraph: &str,
    connectors: &'a [Connector],
) -> Option<&'a Connector> {
    let simple = format!("{parent_type}.{field}");
    connectors
        .iter()
        .find(|c| c.id.subgraph_name.as_str() == subgraph && c.id.directive.simple_name() == simple)
}

/// If `fetch` is a **multi-connector root-field merge**, return one single-
/// connector [`FetchNode`] per connector (each with its own sub-operation,
/// stamped coordinate, and partitioned rewrites). Otherwise return `None` and
/// let [`stamp_fetch`] handle it in place.
///
/// Deliberately bounded (see module docs): only a plain root-field `Query` fetch
/// with no defer `id`, no `requires`, and no `variable_usages`, whose every
/// top-level selection is a connector-backed field, and which spans >1 distinct
/// connector. Anything else is left for `stamp_fetch` (0/1 connector) or left
/// `None` (genuinely unsupported), never split incorrectly.
fn split_root_field_fetch(fetch: &FetchNode, connectors: &[Connector]) -> Option<Vec<FetchNode>> {
    // Guards: only the simple, side-channel-free root-field shape is safe to split.
    if fetch.id.is_some()
        || !fetch.requires.is_empty()
        || !fetch.variable_usages.is_empty()
        || fetch.operation_kind != OperationType::Query
    {
        return None;
    }

    let parsed = fetch.operation_document.as_parsed().ok()?;
    let op = parsed.operations.get(None).ok()?;
    let root_type = op.selection_set.ty.as_str();
    let subgraph = fetch.subgraph_name.as_ref();

    // Map every top-level selection to a connector; bail unless all are
    // connector-backed plain fields. Group field names by connector coordinate,
    // preserving first-seen order.
    let mut groups: Vec<(String, Vec<Name>)> = Vec::new();
    for selection in &op.selection_set.selections {
        let Selection::Field(field) = selection else {
            return None;
        };
        let name = field.name.as_str();
        if name == "_entities" || name == "__typename" {
            return None;
        }
        let connector = connector_for(root_type, name, subgraph, connectors)?;
        let coordinate = connector.id.coordinate();
        match groups.iter_mut().find(|(c, _)| *c == coordinate) {
            Some((_, fields)) => fields.push(field.name.clone()),
            None => groups.push((coordinate, vec![field.name.clone()])),
        }
    }

    // Only a genuine merge (>1 connector) is split; a single connector is
    // ordinary stamping.
    if groups.len() < 2 {
        return None;
    }

    let base: ExecutableDocument = (***parsed).clone();
    let mut out = Vec::with_capacity(groups.len());
    for (coordinate, field_names) in &groups {
        let keep: HashSet<Name> = field_names.iter().cloned().collect();

        // Sub-operation: the merged operation with only this connector's fields.
        let mut doc = base.clone();
        let op_node = doc.operations.anonymous.as_mut()?;
        op_node
            .make_mut()
            .selection_set
            .selections
            .retain(|s| matches!(s, Selection::Field(f) if keep.contains(&f.name)));
        // A subset of an already-valid operation is still valid; keep both the
        // parsed and serialized forms so the plan's `Display` (which reads the
        // parsed doc) and the router's later re-parse both work.
        let doc = SerializableDocument::from_parsed(Valid::assume_valid(doc));

        let mut split = fetch.clone();
        split.operation_document = doc;
        split.connector = Some(coordinate.clone());
        split.output_rewrites = filter_rewrites(&fetch.output_rewrites, &keep);
        split.context_rewrites = filter_rewrites(&fetch.context_rewrites, &keep);
        split.input_rewrites = Arc::new(filter_rewrites(&fetch.input_rewrites, &keep));
        out.push(split);
    }
    Some(out)
}

/// Keep the rewrites whose path targets one of `keep`'s top-level fields. A
/// rewrite whose path has no leading `Key` (e.g. starts at `Parent`) is
/// ambiguous, so it's kept on every partition (conservative).
fn filter_rewrites(
    rewrites: &[Arc<FetchDataRewrite>],
    keep: &HashSet<Name>,
) -> Vec<Arc<FetchDataRewrite>> {
    rewrites
        .iter()
        .filter(|rw| rewrite_targets_kept_field(rw, keep))
        .cloned()
        .collect()
}

fn rewrite_targets_kept_field(rewrite: &FetchDataRewrite, keep: &HashSet<Name>) -> bool {
    let path = match rewrite {
        FetchDataRewrite::ValueSetter(v) => &v.path,
        FetchDataRewrite::KeyRenamer(k) => &k.path,
    };
    for element in path {
        match element {
            FetchDataPathElement::Key(name, _) => return keep.contains(name),
            // Skip a leading type-condition; it doesn't select a field.
            FetchDataPathElement::TypenameEquals(_) => continue,
            // No leading field key to key on — keep it everywhere.
            FetchDataPathElement::AnyIndex(_) | FetchDataPathElement::Parent => return true,
        }
    }
    true
}

/// Collect the `(parent_type, field)` pairs a fetch operation targets. Root
/// fields yield `(root_type, field)`; an `_entities` field descends into its
/// inline fragments, yielding `(fragment_type, field)` for each non-`__typename`
/// field.
fn collect_targets(
    parent_type: &str,
    selection_set: &SelectionSet,
    out: &mut Vec<(String, String)>,
) {
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
                    if let Selection::Field(field) = inner
                        && field.name.as_str() != "__typename"
                    {
                        out.push((ty.to_string(), field.name.to_string()));
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
    use crate::query_graph::build_federated_query_graph;
    use crate::query_plan::query_planner::QueryPlanner;
    use crate::query_plan::query_planner::QueryPlannerConfig;
    use crate::schema::FederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    /// Collect `(subgraph, connector_coordinate_option)` for every fetch in a
    /// (possibly stamped) plan, in traversal order.
    fn fetch_stamps(plan: &QueryPlan) -> Vec<(String, Option<String>)> {
        fn walk(node: &PlanNode, out: &mut Vec<(String, Option<String>)>) {
            match node {
                PlanNode::Fetch(f) => out.push((f.subgraph_name.to_string(), f.connector.clone())),
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
        let api = supergraph
            .to_api_schema(ApiSchemaOptions::default())
            .unwrap();
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
            let mut plan = planner
                .build_query_plan(&doc, None, Default::default())
                .unwrap();
            stamp_connector_coordinates(
                &mut plan,
                &connectors,
                planner.supergraph_schema().schema(),
            );
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
            let mut plan = planner
                .build_query_plan(&doc, None, Default::default())
                .unwrap();
            stamp_connector_coordinates(
                &mut plan,
                &connectors,
                planner.supergraph_schema().schema(),
            );
            let stamps = fetch_stamps(&plan);

            // Every graphql fetch is left unstamped (not a connector).
            for (sg, coord) in &stamps {
                if sg == "graphql" {
                    assert_eq!(
                        coord, &None,
                        "graphql fetch must not be stamped: {stamps:?}"
                    );
                }
            }
            // The connector entity fetch for `d` is stamped with User.d.
            assert!(
                stamps.iter().any(|(sg, c)| sg == "connectors"
                    && c.as_deref().is_some_and(|c| c.contains("User.d"))),
                "connector entity fetch stamped with User.d coordinate, got {stamps:?}"
            );
            // And the connectors root fetch for `user` is stamped with Query.user.
            assert!(
                stamps.iter().any(|(sg, c)| sg == "connectors"
                    && c.as_deref().is_some_and(|c| c.contains("Query.user["))),
                "connectors root fetch stamped with Query.user coordinate, got {stamps:?}"
            );
        }
    }

    /// The SelectionAnalysis-backed provided-field primitive precisely locates
    /// the over-merge (side-effect #3): `Query.users` provides `{id, name}` but
    /// not `username`, while the `Query.user` entity resolver provides it. This
    /// is the signal the output-shape split uses to reconstruct the connector
    /// boundary expansion got structurally.
    #[test]
    fn connector_provided_fields_locates_over_merge() {
        let sdl = include_str!("../connectors/expand/tests/schemas/expand/steelthread.graphql");
        let fed = FederationSchema::new(Schema::parse(sdl, "s.graphql").unwrap()).unwrap();
        let subgraphs = extract_subgraphs_from_supergraph(&fed, Some(false)).unwrap();
        let connectors: Vec<Connector> = subgraphs
            .subgraphs
            .values()
            .flat_map(|sg| Connector::from_schema(sg.schema.schema(), &sg.name).unwrap_or_default())
            .collect();
        let by_coord = |needle: &str| {
            connectors
                .iter()
                .find(|c| c.id.coordinate().contains(needle))
                .unwrap_or_else(|| panic!("connector {needle} not found"))
        };
        let users = connector_provided_fields(by_coord("Query.users")).unwrap();
        let user = connector_provided_fields(by_coord("Query.user[")).unwrap();
        assert!(
            users.contains("name") && !users.contains("username"),
            "Query.users provides name but not username, got {users:?}"
        );
        assert!(
            user.contains("username"),
            "Query.user entity resolver provides username, got {user:?}"
        );
    }
}
