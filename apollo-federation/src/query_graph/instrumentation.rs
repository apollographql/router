use serde_json_bytes::json;
use serde_json_bytes::Value;

use super::graph_path::ClosedPath;
use super::QueryGraph;

impl QueryGraph {
    pub(crate) fn to_json(&self) -> Value {
        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for i in self.graph().node_indices() {
            let node = &self.graph()[i];
            nodes.push(json!({
              "id": i.index(),
              "label": node.type_.to_string(),
              "source": node.source,
            }));
        }

        for i in self.graph().edge_indices() {
            if let Some((n1, n2)) = self.graph().edge_endpoints(i) {
                let edge = &self.graph()[i];
                edges.push(json!({
                  "id": i.index(),
                  "head": n1.index(),
                  "tail": n2.index(),
                  "label": edge.to_string(),
                }));
            }
        }

        json!({
          "nodes": nodes,
          "edges": edges,
        })
    }
}

impl ClosedPath {
    pub(crate) fn to_json(&self) -> Value {
        json!({
          "kind": "ClosedPath",
          "paths": self.paths.0.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
          "selection_set": self.selection_set.as_ref().map(|a| a.to_string()),
        })
    }
}
