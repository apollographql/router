//! A specialization of `TransitionGraphPath`.
use petgraph::graph::EdgeIndex;

use crate::error::FederationError;
use crate::query_graph::QueryGraphEdge;
use crate::query_graph::QueryGraphNode;
use crate::query_graph::graph_path::TransitionGraphPath;
use crate::schema::ValidFederationSchema;

impl TransitionGraphPath {
    pub(crate) fn head_node(&self) -> Result<&QueryGraphNode, FederationError> {
        self.graph.node_weight(self.head)
    }

    pub(crate) fn tail_node(&self) -> Result<&QueryGraphNode, FederationError> {
        self.graph.node_weight(self.tail)
    }
}

// Query graph accessors
// - `self` is used to access the underlying query graph, not its path data.
impl TransitionGraphPath {
    pub(crate) fn schema_by_source(
        &self,
        source: &str,
    ) -> Result<&ValidFederationSchema, FederationError> {
        self.graph.schema_by_source(source)
    }

    pub(crate) fn edge_weight(
        &self,
        edge_index: EdgeIndex,
    ) -> Result<&QueryGraphEdge, FederationError> {
        self.graph.edge_weight(edge_index)
    }

    #[allow(unused)]
    pub(crate) fn edge_head(
        &self,
        edge_index: EdgeIndex,
    ) -> Result<&QueryGraphNode, FederationError> {
        let (head, _) = self.graph.edge_endpoints(edge_index)?;
        self.graph.node_weight(head)
    }

    #[allow(unused)]
    pub(crate) fn edge_tail(
        &self,
        edge_index: EdgeIndex,
    ) -> Result<&QueryGraphNode, FederationError> {
        let (_, tail) = self.graph.edge_endpoints(edge_index)?;
        self.graph.node_weight(tail)
    }
}
