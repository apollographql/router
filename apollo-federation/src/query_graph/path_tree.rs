use crate::query_graph::graph_path::OpGraphPathTrigger;
use crate::query_graph::QueryGraph;
use crate::query_plan::operation::NormalizedSelectionSet;
use petgraph::graph::{EdgeIndex, NodeIndex};
use std::hash::Hash;
use std::sync::Arc;

/// A "merged" tree representation for a vector of `GraphPath`s that start at a common query graph
/// node, in which each node of the tree corresponds to a node in the query graph, and a tree's node
/// has a child for every unique pair of edge and trigger.
// PORT_NOTE: The JS codebase additionally has a property `triggerEquality`; this existed because
// Typescript doesn't have a native way of associating equality/hash functions with types, so they
// were passed around manually. This isn't the case with Rust, where we instead implement trigger
// equality via `PartialEq` and `Hash`.
#[derive(Debug)]
pub(crate) struct PathTree<TTrigger: Eq + Hash, TEdge: Into<Option<EdgeIndex>>> {
    /// The query graph of which this is a path tree.
    graph: Arc<QueryGraph>,
    /// The query graph node at which the path tree starts.
    node: NodeIndex,
    /// Note that `ClosedPath`s have an optimization which splits them into paths and a selection
    /// set representing a trailing query to a single subgraph at the final nodes of the paths. For
    /// such paths where this `PathTree`'s node corresponds to that final node, those selection sets
    /// are collected here. This is really an optimization to avoid unnecessary merging of selection
    /// sets when they query a single subgraph.
    local_selection_sets: Vec<Arc<NormalizedSelectionSet>>,
    /// The child `PathTree`s for this `PathTree` node. There is a child for every unique pair of
    /// edge and trigger present at this particular sub-path within the `GraphPath`s covered by this
    /// `PathTree` node.
    childs: Vec<Arc<PathTreeChild<TTrigger, TEdge>>>,
}

#[derive(Debug)]
pub(crate) struct PathTreeChild<TTrigger: Eq + Hash, TEdge: Into<Option<EdgeIndex>>> {
    /// The edge connecting this child to its parent.
    edge: TEdge,
    /// The trigger for the edge connecting this child to its parent.
    trigger: Arc<TTrigger>,
    /// The conditions required to be fetched if this edge is taken.
    conditions: Arc<OpPathTree>,
    /// The child `PathTree` reached by taking the edge.
    tree: Arc<PathTree<TTrigger, TEdge>>,
}

/// A `PathTree` whose triggers are operation elements (essentially meaning that the constituent
/// `GraphPath`s were guided by a GraphQL operation).
pub(crate) type OpPathTree = PathTree<OpGraphPathTrigger, Option<EdgeIndex>>;
