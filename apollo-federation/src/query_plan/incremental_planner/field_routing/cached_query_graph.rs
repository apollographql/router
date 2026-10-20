//! Memoizing wrapper around a QueryGraph. The caches here cover the
//! hot-path lookups (edge_for_field, edge_for_inline_fragment)
//! that depend only on the immutable query graph, so they are valid for the
//! entire planning session and never need checkpoint/rollback.

use std::cell::RefCell;
use std::sync::Arc;

use apollo_compiler::Name;
use hashbrown::HashMap;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use crate::error::FederationError;
use crate::operation::Field;
use crate::operation::InlineFragment;
use crate::operation::SelectionSet;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;

type EdgeForFieldCache = RefCell<HashMap<(NodeIndex, Name), Option<EdgeIndex>>>;
type EdgeForFragmentCache = RefCell<HashMap<(NodeIndex, Option<Name>), Option<EdgeIndex>>>;
type KeyExitsCache = RefCell<HashMap<NodeIndex, Arc<[KeyExit]>>>;

/// A key-resolution edge leaving a node for another subgraph.
pub(crate) struct KeyExit {
    pub(crate) target: NodeIndex,
    /// Index into [`CachedQueryGraph::subgraph_count`] slots, so visited
    /// sets can be indexed instead of comparing names.
    pub(crate) subgraph: usize,
    pub(crate) subgraph_name: Arc<str>,
    pub(crate) conditions: Option<Arc<SelectionSet>>,
}

/// Wraps an immutable `QueryGraph` with caches for lookups that recur
/// heavily during planning. Every cache is monotonic (grow-only) and keyed
/// on immutable query-graph data, so no rollback is needed.
pub(crate) struct CachedQueryGraph {
    pub(crate) query_graph: Arc<QueryGraph>,
    override_conditions: OverrideConditions,
    edge_for_field: EdgeForFieldCache,
    edge_for_fragment: EdgeForFragmentCache,
    key_exits: KeyExitsCache,
}

impl CachedQueryGraph {
    pub(crate) fn new(
        query_graph: Arc<QueryGraph>,
        override_conditions: OverrideConditions,
    ) -> Self {
        Self {
            query_graph,
            override_conditions,
            edge_for_field: RefCell::new(HashMap::new()),
            edge_for_fragment: RefCell::new(HashMap::new()),
            key_exits: RefCell::new(HashMap::new()),
        }
    }

    /// Slots for [`Self::subgraph_index`]: one per subgraph schema, plus one
    /// for the federated root source, the only source without one.
    pub(super) fn subgraph_count(&self) -> usize {
        self.query_graph.subgraph_schemas().len() + 1
    }

    pub(super) fn subgraph_index(&self, source: &str) -> usize {
        self.query_graph
            .subgraph_schemas()
            .get_index_of(source)
            .unwrap_or(self.query_graph.subgraph_schemas().len())
    }

    /// Key-resolution edges out of `node` into other nodes, in edge order.
    /// Chained key-hop search walks these for every field it routes, and
    /// scanning all of a node's out edges for them each time dominated it.
    pub(super) fn key_exits(&self, node: NodeIndex) -> Result<Arc<[KeyExit]>, FederationError> {
        if let Some(cached) = self.key_exits.borrow().get(&node) {
            return Ok(cached.clone());
        }
        let exits: Arc<[KeyExit]> = self
            .query_graph
            .out_edges(node)
            .into_iter()
            .filter(|edge| {
                matches!(
                    edge.weight().transition,
                    QueryGraphEdgeTransition::KeyResolution
                )
            })
            .map(|edge| {
                let target = edge.target();
                let source = &self.query_graph.node_weight(target)?.source;
                Ok(KeyExit {
                    target,
                    subgraph: self.subgraph_index(source),
                    subgraph_name: source.clone(),
                    conditions: edge.weight().conditions.clone(),
                })
            })
            .collect::<Result<_, FederationError>>()?;
        self.key_exits.borrow_mut().insert(node, exits.clone());
        Ok(exits)
    }

    /// Cached lookup: which edge from `node` resolves `field`?
    pub(super) fn edge_for_field(&self, node: NodeIndex, field: &Field) -> Option<EdgeIndex> {
        let field_name = field.field_position.field_name().clone();
        let cache_key = (node, field_name);
        if let Some(cached) = self.edge_for_field.borrow().get(&cache_key) {
            return *cached;
        }
        let result = self
            .query_graph
            .edge_for_field(node, field, &self.override_conditions);
        self.edge_for_field.borrow_mut().insert(cache_key, result);
        result
    }

    /// Cached lookup: which Downcast edge from `node` matches the inline
    /// fragment's type condition?
    pub(super) fn edge_for_inline_fragment(
        &self,
        node: NodeIndex,
        inline_fragment: &InlineFragment,
    ) -> Option<EdgeIndex> {
        let cond_name = inline_fragment
            .type_condition_position
            .as_ref()
            .map(|pos| pos.type_name().clone());
        let cache_key = (node, cond_name);
        if let Some(cached) = self.edge_for_fragment.borrow().get(&cache_key) {
            return *cached;
        }
        let result = self
            .query_graph
            .edge_for_inline_fragment(node, inline_fragment);
        self.edge_for_fragment
            .borrow_mut()
            .insert(cache_key, result);
        result
    }
}
