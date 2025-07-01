use std::collections::BTreeMap;
use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use itertools::Itertools;
use petgraph::visit::EdgeRef;

use crate::bail;
use crate::ensure;
use crate::error::FederationError;
use crate::query_graph::QueryGraph;
use crate::query_graph::QueryGraphEdgeTransition;
use crate::query_graph::QueryGraphNodeType;
use crate::query_graph::condition_resolver::ConditionResolution;
use crate::query_graph::graph_path::transition::TransitionGraphPath;
use crate::query_graph::graph_path::transition::TransitionPathWithLazyIndirectPaths;
use crate::schema::position::SchemaRootDefinitionKind;

struct ValidationState {
    /// Path in the supergraph (i.e. the API schema query graph) corresponding to the current state.
    supergraph_path: TransitionGraphPath,
    /// All the possible paths we could be in the subgraphs (excluding @provides paths).
    subgraph_paths: Vec<SubgraphPathInfo>,
    /// When we encounter a supergraph field with a progressive override (i.e. an @override with a
    /// label condition), we consider both possibilities for the label value (T/F) as we traverse
    /// the graph, and record that here. This allows us to exclude paths that can never be taken by
    /// the query planner (i.e. a path where the condition is T in one case and F in another).
    #[allow(dead_code)]
    selected_override_conditions: IndexMap<String, bool>,
}

struct SubgraphPathInfo {
    path: TransitionPathWithLazyIndirectPaths,
    contexts: SubgraphPathContexts,
}

/// A map from context names to information about their match in the subgraph path, if it exists.
/// This is a `BTreeMap` to support `Hash`, as this is used in keys in maps.
type SubgraphPathContexts = Arc<BTreeMap<String, SubgraphPathContextInfo>>;

#[derive(PartialEq, Eq, Hash)]
struct SubgraphPathContextInfo {
    subgraph_name: Arc<str>,
    type_name: Name,
}

#[derive(PartialEq, Eq, Hash)]
struct SubgraphContextKey {
    tail_subgraph_name: Arc<str>,
    contexts: SubgraphPathContexts,
}

impl ValidationState {
    // PORT_NOTE: Named `initial()` in the JS codebase, but conventionally in Rust this kind of
    // constructor is named `new()`.
    #[allow(dead_code)]
    fn new(
        api_schema_query_graph: Arc<QueryGraph>,
        federated_query_graph: Arc<QueryGraph>,
        root_kind: SchemaRootDefinitionKind,
    ) -> Result<Self, FederationError> {
        let Some(federated_root_node) =
            federated_query_graph.root_kinds_to_nodes()?.get(&root_kind)
        else {
            bail!(
                "The supergraph shouldn't have a {} root if no subgraphs have one",
                root_kind
            );
        };
        let federated_root_node_weight = federated_query_graph.node_weight(*federated_root_node)?;
        ensure!(
            federated_root_node_weight.type_ == QueryGraphNodeType::FederatedRootType(root_kind),
            "Unexpected node type {} for federated query graph root (expected {})",
            federated_root_node_weight.type_,
            QueryGraphNodeType::FederatedRootType(root_kind),
        );
        let initial_subgraph_path =
            TransitionGraphPath::from_graph_root(federated_query_graph.clone(), root_kind)?;
        Ok(Self {
            supergraph_path: TransitionGraphPath::from_graph_root(
                api_schema_query_graph,
                root_kind,
            )?,
            subgraph_paths: federated_query_graph
                .out_edges(*federated_root_node)
                .into_iter()
                .map(|edge_ref| {
                    let path = initial_subgraph_path.add(
                        QueryGraphEdgeTransition::SubgraphEnteringTransition,
                        edge_ref.id(),
                        ConditionResolution::no_conditions(),
                        None,
                    )?;
                    Ok::<_, FederationError>(SubgraphPathInfo {
                        path: TransitionPathWithLazyIndirectPaths::new(Arc::new(path)),
                        contexts: Default::default(),
                    })
                })
                .process_results(|iter| iter.collect())?,
            selected_override_conditions: Default::default(),
        })
    }

    #[allow(dead_code)]
    fn current_subgraph_names(&self) -> Result<IndexSet<Arc<str>>, FederationError> {
        self.subgraph_paths
            .iter()
            .map(|path_info| {
                Ok(path_info
                    .path
                    .path
                    .graph()
                    .node_weight(path_info.path.path.tail())?
                    .source
                    .clone())
            })
            .process_results(|iter| iter.collect())
    }

    #[allow(dead_code)]
    fn current_subgraph_context_keys(
        &self,
    ) -> Result<IndexSet<SubgraphContextKey>, FederationError> {
        self.subgraph_paths
            .iter()
            .map(|path_info| {
                Ok(SubgraphContextKey {
                    tail_subgraph_name: path_info
                        .path
                        .path
                        .graph()
                        .node_weight(path_info.path.path.tail())?
                        .source
                        .clone(),
                    contexts: path_info.contexts.clone(),
                })
            })
            .process_results(|iter| iter.collect())
    }
}

impl Display for ValidationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.supergraph_path.fmt(f)?;
        write!(f, " <=> ")?;
        let mut iter = self.subgraph_paths.iter();
        if let Some(first_path_info) = iter.next() {
            first_path_info.path.fmt(f)?;
            for path_info in iter {
                write!(f, ", ")?;
                path_info.path.fmt(f)?;
            }
        }
        Ok(())
    }
}
