//! Memoizing wrapper around a QueryGraph. The caches here cover the
//! hot-path lookups (out_edges, edge_for_field, edge_for_inline_fragment)
//! that depend only on the immutable query graph, so they are valid for the
//! entire planning session and never need checkpoint/rollback.

use std::cell::RefCell;
use std::sync::Arc;

use apollo_compiler::Name;
use hashbrown::HashMap;
use petgraph::Direction;
use petgraph::graph::EdgeIndex;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use crate::operation::Field;
use crate::operation::InlineFragment;
use crate::query_graph::OverrideConditions;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;

type OutEdgesCache = RefCell<HashMap<NodeIndex, Arc<Vec<EdgeIndex>>>>;
type EdgeForFieldCache = RefCell<HashMap<(NodeIndex, Name), Option<EdgeIndex>>>;
type EdgeForFragmentCache = RefCell<HashMap<(NodeIndex, Option<Name>), Option<EdgeIndex>>>;

/// Wraps an immutable `QueryGraph` with caches for lookups that recur
/// heavily during planning. Every cache is monotonic (grow-only) and keyed
/// on immutable query-graph data, so no rollback is needed.
pub(crate) struct CachedQueryGraph {
    pub(crate) query_graph: Arc<QueryGraph>,
    override_conditions: OverrideConditions,
    out_edges: OutEdgesCache,
    edge_for_field: EdgeForFieldCache,
    edge_for_fragment: EdgeForFragmentCache,
}

impl CachedQueryGraph {
    pub(crate) fn new(
        query_graph: Arc<QueryGraph>,
        override_conditions: OverrideConditions,
    ) -> Self {
        Self {
            query_graph,
            override_conditions,
            out_edges: RefCell::new(HashMap::new()),
            edge_for_field: RefCell::new(HashMap::new()),
            edge_for_fragment: RefCell::new(HashMap::new()),
        }
    }

    /// Sorted outgoing edge indices for `node`, cached. Excludes self-key
    /// and self-root-type-resolution edges.
    pub(super) fn out_edges(&self, node: NodeIndex) -> Arc<Vec<EdgeIndex>> {
        if let Some(cached) = self.out_edges.borrow().get(&node) {
            return cached.clone();
        }
        let mut edges: Vec<_> = self
            .query_graph
            .graph()
            .edges_directed(node, Direction::Outgoing)
            .filter(|edge_ref| {
                !(edge_ref.source() == edge_ref.target()
                    && matches!(
                        edge_ref.weight().transition,
                        QueryGraphEdgeTransition::KeyResolution
                            | QueryGraphEdgeTransition::RootTypeResolution { .. }
                    ))
            })
            .map(|e| e.id())
            .collect();
        edges.sort();
        let result = Arc::new(edges);
        self.out_edges.borrow_mut().insert(node, result.clone());
        result
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
