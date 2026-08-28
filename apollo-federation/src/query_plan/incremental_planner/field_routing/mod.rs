//! Field-level routing as a BULB search space.
//!
//! The planner walks the operation selection-by-selection ("pendings"),
//! consulting the query graph for where each field can be resolved:
//! - [`state`]: mutable search state (pending stack, checkpoints).
//! - [`routing`]: enumerating and ranking options for a selection.
//! - [`commit`]: applying a chosen option to the fetch graph.
//! - [`conditions`]: condition satisfiability for @requires / @key.
//! - [`requires`]: hop-edge inputs and condition paths.
//!
//! This file holds the search-space type and the
//! [`BulbSearchSpace`] implementation.

mod conditions;
mod requires;
mod routing;
pub(super) mod state;

use std::sync::Arc;

use apollo_compiler::Name;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
#[allow(unused_imports)]
pub(crate) use state::PendingSelection;
#[allow(unused_imports)]
pub(crate) use state::PlanState;

use super::shared_path::SharedPath;
use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::InlineFragment;
use crate::operation::Selection;
use crate::operation::SelectionId;
use crate::operation::SelectionSet;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraph;
use crate::query_graph::graph_path::operation::OpPathElement;
use crate::schema::ValidFederationSchema;
use crate::schema::position::CompositeTypeDefinitionPosition;

/// Cache key for routing options. Captures the selection identity at a QG node.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RoutingCacheKey {
    Field(Name),
    InlineFragment(Option<Name>),
}

/// Subgraph, type position, and schema at a query graph node.
pub(super) struct NodeSource {
    pub(super) subgraph: Arc<str>,
    pub(super) type_pos: CompositeTypeDefinitionPosition,
    pub(super) schema: ValidFederationSchema,
}

/// Search space presenting field-level routing decisions as a BULB problem.
pub(crate) struct FieldRoutingSearchSpace {
    pub(crate) query_graph: Arc<QueryGraph>,
    pub(crate) supergraph_schema: ValidFederationSchema,
    pub(crate) override_conditions: OverrideConditions,
    pub(crate) inconsistent_abstract_types: Arc<apollo_compiler::collections::IndexSet<Name>>,
}

impl FieldRoutingSearchSpace {
    pub(super) fn node_source(&self, node: NodeIndex) -> Result<NodeSource, FederationError> {
        let data = self.query_graph.node_weight(node)?;
        Ok(NodeSource {
            subgraph: data.source.clone(),
            type_pos: data.type_.clone().try_into()?,
            schema: self.query_graph.schema_by_source(&data.source)?.clone(),
        })
    }

    /// Select `__typename` in `fetch_node` at `base_path` so the executor
    /// can identify the concrete type for entity representations.
    pub(super) fn append_typename(
        &self,
        state: &mut PlanState,
        fetch_node: NodeIndex,
        base_path: &SharedPath<Arc<OpPathElement>>,
        source: &NodeSource,
    ) {
        let typename = Arc::new(OpPathElement::Field(Field::new_introspection_typename(
            &source.schema,
            &source.type_pos,
            None,
        )));
        state
            .graph
            .append_selection(fetch_node, &base_path.pushed(typename), None);
    }

    /// Op path at which selections enter an entity fetch group: entity
    /// fetches start from the `_Entity` union, so everything nests under a
    /// `... on <ConcreteType>` condition rebased onto the supergraph schema
    /// (which OpPaths reference).
    pub(super) fn entity_root_path(
        &self,
        type_name: &Name,
    ) -> Result<SharedPath<Arc<OpPathElement>>, FederationError> {
        let rebased: CompositeTypeDefinitionPosition =
            self.supergraph_schema.get_type(type_name)?.try_into()?;
        let condition = InlineFragment {
            schema: self.supergraph_schema.clone(),
            parent_type_position: rebased.clone(),
            type_condition_position: Some(rebased),
            directives: Default::default(),
            selection_id: SelectionId::new(),
        };
        Ok(SharedPath::new().pushed(Arc::new(OpPathElement::InlineFragment(condition))))
    }

    /// Can condition fields simply be selected in the fetch at `node`?
    /// True when the subgraph resolves every field itself and none carries
    /// @requires (which draws on an entity representation and needs its own
    /// fetch). The graph-based check complements the schema-based one:
    /// @external fields may still resolve at `node` when it is a
    /// provides-copy created by an ancestor's @provides.
    pub(super) fn can_resolve_in_place(
        &self,
        node: NodeIndex,
        conditions: &Arc<SelectionSet>,
        source: &NodeSource,
    ) -> Result<bool, FederationError> {
        let satisfiable = self.can_satisfy(
            conditions,
            &source.type_pos,
            &source.subgraph,
            &source.schema,
        ) || self.conditions_resolvable_at_node(node, conditions)?;
        Ok(satisfiable && !self.conditions_have_requires(node, conditions)?)
    }

    /// Outgoing edge indices from a query graph node, sorted and filtered.
    pub(super) fn out_edge_indices(&self, node: NodeIndex) -> Vec<EdgeIndex> {
        self.query_graph
            .out_edges(node)
            .into_iter()
            .map(|edge_ref| edge_ref.id())
            .collect()
    }

    /// Find the outgoing edge for a field at a query graph node.
    pub(super) fn edge_for_field(&self, node: NodeIndex, field: &Field) -> Option<EdgeIndex> {
        self.query_graph
            .edge_for_field(node, field, &self.override_conditions)
    }

    /// Find the outgoing downcast edge for an inline fragment at a query
    /// graph node.
    pub(super) fn edge_for_inline_fragment(
        &self,
        node: NodeIndex,
        fragment: &InlineFragment,
    ) -> Option<EdgeIndex> {
        self.query_graph.edge_for_inline_fragment(node, fragment)
    }
}

/// Short human-readable label for a selection, for logging.
pub(super) fn selection_label(selection: &Selection) -> String {
    match selection {
        Selection::Field(f) => f.field.field_position.to_string(),
        Selection::InlineFragment(f) => {
            format!("... on {:?}", f.inline_fragment.type_condition_position)
        }
    }
}
