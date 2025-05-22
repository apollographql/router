use petgraph::graph::EdgeIndex;

use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::graph_path::GraphPath;

/// A `GraphPath` whose triggers are query graph transitions in some other query graph (essentially
/// meaning that the path has been guided by a walk through that other query graph).
#[allow(dead_code)]
pub(crate) type TransitionGraphPath = GraphPath<QueryGraphEdgeTransition, EdgeIndex>;
