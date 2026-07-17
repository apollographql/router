//! Source-aware query planning — Phase 1: the connect-graph-builder (first slice).
//!
//! Bridges the Spike-A connector-condition derivation
//! ([`crate::connectors::source_aware`]) into the canonical query-graph edge
//! model. The proposal's target design reuses the existing
//! [`QueryGraphEdgeTransition::KeyResolution`] transition for connector-entering
//! edges (a new transition variant would fan out across ~7 exhaustive matches),
//! and models a connector's parent-data requirement as the edge's
//! `conditions: Option<Arc<SelectionSet>>` — exactly how `@key`/`@requires`
//! conditions are already represented (`build_query_graph.rs:1340`).
//!
//! This slice produces the edge *data* — the transition, the landing type, and
//! the condition `SelectionSet` — for each connector, without yet mutating a
//! live [`QueryGraph`](super::QueryGraph). Producing it as a standalone,
//! testable value first keeps the eventual integration into `build_query_graph`
//! a graft (push these onto the edge list) rather than a rewrite, and lets the
//! condition-conversion be differentially tested against the Spike-A output.

// Phase-1 seam: the edge fields are consumed when this is grafted into
// `build_query_graph`; until then they're read only by the tests below.
#![allow(dead_code)]

use std::sync::Arc;

use apollo_compiler::ast::NamedType;

use super::QueryGraphEdgeTransition;
use crate::connectors::Connector;
use crate::connectors::EntityResolver;
use crate::connectors::source_aware::derive_condition;
use crate::error::FederationError;
use crate::operation::SelectionSet;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::supergraph::extract_subgraphs_from_supergraph;
use crate::schema::field_set::parse_field_set;

/// The query-graph edge that *enters* a connector's source — the data a
/// `SourceEntering`/`KeyResolution` edge carries in the source-aware graph.
///
/// This is deliberately the shape of the canonical edge (transition +
/// conditions), so wiring it into `build_query_graph` is a direct `add_edge`.
#[derive(Debug)]
pub(crate) struct SourceEnteringEdge {
    /// The connector this edge dispatches to (direct id, no synthetic service name).
    pub(crate) connector_coordinate: String,
    /// The connector's entity-resolver kind.
    pub(crate) entity_resolver: Option<EntityResolver>,
    /// The type the condition (and the entity key) is rooted on.
    pub(crate) key_type: NamedType,
    /// The type the connector produces (its base type), when determinable.
    pub(crate) output_type: Option<NamedType>,
    /// Canonical transition kind — reused, not a new variant.
    pub(crate) transition: QueryGraphEdgeTransition,
    /// The parent-data condition guarding the edge, as a query-graph
    /// `SelectionSet` (converted from the Spike-A `FieldSet`).
    pub(crate) conditions: Option<Arc<SelectionSet>>,
}

/// Build the source-entering edge for `connector`, or `Ok(None)` when the
/// connector contributes no parent-data condition (a plain root-field connector,
/// or a legacy implicit resolver with no key — see
/// [`derive_condition`]). Those non-keyed cases are ordinary field-collection
/// edges and are out of scope for this slice.
pub(crate) fn build_source_entering_edge(
    connector: &Connector,
    schema: &ValidFederationSchema,
) -> Result<Option<SourceEnteringEdge>, FederationError> {
    let Some(condition) =
        derive_condition(connector, schema.schema()).map_err(FederationError::internal)?
    else {
        return Ok(None);
    };

    // The condition is already rooted on the entity/key type — take that type
    // straight off the FieldSet rather than re-deriving the resolver's key type.
    let key_type = condition.selection_set.ty.clone();
    let fields = condition.serialize().no_indent().to_string();

    // Reuse the exact conversion `build_query_graph` uses for @key conditions.
    let conditions = Arc::new(parse_field_set(schema, key_type.clone(), &fields, true)?);

    Ok(Some(SourceEnteringEdge {
        connector_coordinate: connector.id.coordinate(),
        entity_resolver: connector.entity_resolver.clone(),
        output_type: connector.id.directive.base_type_name(schema.schema()),
        key_type,
        transition: QueryGraphEdgeTransition::KeyResolution,
        conditions: Some(conditions),
    }))
}

/// Collect the source-entering edges for **every** connector in a
/// (non-expanded) supergraph — the full set the source-aware builder will add
/// to the query graph.
///
/// Extracts the supergraph's subgraphs *without* connector expansion, builds
/// each subgraph's connectors, and gathers a [`SourceEnteringEdge`] per keyed
/// connector. Connectors that contribute no condition (plain root fields,
/// legacy implicit-without-key) are skipped — see [`build_source_entering_edge`].
pub(crate) fn build_connector_source_edges(
    supergraph_schema: &FederationSchema,
) -> Result<Vec<SourceEnteringEdge>, FederationError> {
    let subgraphs = extract_subgraphs_from_supergraph(supergraph_schema, Some(true))?;

    let mut edges = Vec::new();
    for (_, subgraph) in subgraphs.subgraphs.iter() {
        let schema = &subgraph.schema;
        // A subgraph with no connect link yields no connectors — skip it.
        let Ok(connectors) = Connector::from_schema(schema.schema(), &subgraph.name) else {
            continue;
        };
        for connector in &connectors {
            if let Some(edge) = build_source_entering_edge(connector, schema)? {
                edges.push(edge);
            }
        }
    }
    Ok(edges)
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use insta::glob;

    use super::build_connector_source_edges;
    use super::build_source_entering_edge;
    use crate::connectors::Connector;
    use crate::query_graph::QueryGraphEdgeTransition;
    use crate::schema::FederationSchema;
    use crate::schema::ValidFederationSchema;
    use crate::supergraph::extract_subgraphs_from_supergraph;

    /// Normalize a selection rendering to a whitespace- and brace-insensitive
    /// token form: the query-graph `SelectionSet` renders with outer braces
    /// (`{ id }`) while the `FieldSet` renders without (`id`), but the field
    /// tokens between them must match.
    fn tokens(s: &str) -> Vec<String> {
        s.split_whitespace()
            .filter(|t| *t != "{" && *t != "}")
            .map(|t| t.to_string())
            .collect()
    }

    /// Every connector with a Spike-A condition yields a `KeyResolution`
    /// source-entering edge whose condition `SelectionSet` faithfully carries
    /// the derived key — proving the Spike-A condition converts losslessly into
    /// the canonical query-graph edge representation.
    #[test]
    fn source_entering_edges_carry_the_derived_condition() {
        let mut edges_built = 0usize;

        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("../connectors/expand/tests/schemas/expand", "*.graphql", |path| {
                let sdl = std::fs::read_to_string(path).unwrap();
                let supergraph_schema =
                    FederationSchema::new(Schema::parse(&sdl, "supergraph.graphql").unwrap())
                        .unwrap();
                let Ok(subgraphs) =
                    extract_subgraphs_from_supergraph(&supergraph_schema, Some(true))
                else {
                    return;
                };

                for (_, subgraph) in subgraphs.subgraphs.iter() {
                    let valid_schema: &ValidFederationSchema = &subgraph.schema;
                    let Ok(connectors) =
                        Connector::from_schema(valid_schema.schema(), &subgraph.name)
                    else {
                        continue;
                    };

                    for connector in &connectors {
                        // Skip connectors whose key derivation errors on this
                        // fixture — not a builder bug.
                        if connector.resolvable_key(valid_schema.schema()).is_err() {
                            continue;
                        }

                        let edge = build_source_entering_edge(connector, valid_schema)
                            .unwrap_or_else(|e| {
                                panic!("edge build failed for {} in {path:?}: {e}",
                                    connector.id.coordinate())
                            });

                        match edge {
                            Some(edge) => {
                                assert_eq!(
                                    edge.transition,
                                    QueryGraphEdgeTransition::KeyResolution,
                                    "connector-entering edge must reuse KeyResolution for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                                let conditions = edge.conditions.expect(
                                    "a source-entering edge always carries a condition",
                                );
                                assert!(
                                    !conditions.is_empty(),
                                    "empty condition for {} in {path:?}",
                                    connector.id.coordinate()
                                );

                                // The condition SelectionSet must carry the same
                                // fields the Spike-A derivation produced.
                                let derived = crate::connectors::source_aware::derive_condition(
                                    connector,
                                    valid_schema.schema(),
                                )
                                .unwrap()
                                .unwrap();
                                assert_eq!(
                                    tokens(&conditions.to_string()),
                                    tokens(&derived.serialize().no_indent().to_string()),
                                    "condition field mismatch for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                                edges_built += 1;
                            }
                            None => {
                                // No condition ⇒ no key. Consistent with Spike A
                                // deriving None for this connector.
                                assert!(
                                    crate::connectors::source_aware::derive_condition(
                                        connector,
                                        valid_schema.schema()
                                    )
                                    .unwrap()
                                    .is_none(),
                                    "edge was None but a condition was derived for {} in {path:?}",
                                    connector.id.coordinate()
                                );
                            }
                        }
                    }
                }
            });
        });

        assert!(
            edges_built > 0,
            "no source-entering edges built — did the fixture glob match?"
        );
    }

    /// The supergraph-level collector gathers a `KeyResolution` edge with a
    /// non-empty condition for every keyed connector across all subgraphs of a
    /// (non-expanded) supergraph.
    #[test]
    fn build_connector_source_edges_collects_keyed_connectors() {
        let mut total = 0usize;

        insta::with_settings!({prepend_module_to_snapshot => false}, {
            glob!("../connectors/expand/tests/schemas/expand", "*.graphql", |path| {
                let sdl = std::fs::read_to_string(path).unwrap();
                let Ok(supergraph) =
                    FederationSchema::new(Schema::parse(&sdl, "supergraph.graphql").unwrap())
                else {
                    return;
                };
                // Skip fixtures that don't extract into subgraphs cleanly.
                let Ok(edges) = build_connector_source_edges(&supergraph) else {
                    return;
                };

                for edge in &edges {
                    assert_eq!(
                        edge.transition,
                        QueryGraphEdgeTransition::KeyResolution,
                        "non-KeyResolution connector edge in {path:?}"
                    );
                    let conditions = edge
                        .conditions
                        .as_ref()
                        .expect("a source-entering edge always carries a condition");
                    assert!(!conditions.is_empty(), "empty condition in {path:?}");
                }
                total += edges.len();
            });
        });

        assert!(
            total > 0,
            "no connector source edges collected across fixtures"
        );
    }
}
