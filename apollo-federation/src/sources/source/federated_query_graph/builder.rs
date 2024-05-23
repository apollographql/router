use enum_dispatch::enum_dispatch;

use crate::error::FederationError;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilder;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::sources::connect;
use crate::sources::connect::ConnectSpecDefinition;
use crate::sources::graphql;
use crate::sources::source::SourceKind;
use crate::ValidFederationSubgraph;
use crate::ValidFederationSubgraphs;

#[enum_dispatch(FederatedQueryGraphBuilderApi)]
pub(crate) enum FederatedQueryGraphBuilder {
    Graphql(graphql::federated_query_graph::builder::FederatedQueryGraphBuilder),
    Connect(connect::federated_query_graph::builder::FederatedQueryGraphBuilder),
}

#[enum_dispatch]
pub(crate) trait FederatedQueryGraphBuilderApi {
    fn process_subgraph_schema(
        &self,
        subgraph: ValidFederationSubgraph,
        builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError>;
}

pub(crate) struct FederatedQueryGraphBuilders {
    graphql: FederatedQueryGraphBuilder, // Always the GraphQL variant
    connect: FederatedQueryGraphBuilder, // Always the Connect variant
}

impl FederatedQueryGraphBuilders {
    pub(crate) fn process_subgraph_schemas(
        &self,
        subgraphs: ValidFederationSubgraphs,
        builder: &mut IntraSourceQueryGraphBuilder,
    ) -> Result<(), FederationError> {
        subgraphs.into_iter().try_for_each(|(_name, graph)| {
            let src_kind = extract_source_kind(&graph);
            builder.set_source_kind(src_kind);
            self.get_builder(src_kind)
                .process_subgraph_schema(graph, builder)
        })
    }

    fn get_builder(&self, src_kind: SourceKind) -> &FederatedQueryGraphBuilder {
        match src_kind {
            SourceKind::Graphql => &self.graphql,
            SourceKind::Connect => &self.connect,
        }
    }
}

fn extract_source_kind(graph: &ValidFederationSubgraph) -> SourceKind {
    if graph
        .schema
        .metadata()
        .and_then(|metadata| metadata.for_identity(&ConnectSpecDefinition::identity()))
        .is_some()
    {
        SourceKind::Connect
    } else {
        SourceKind::Graphql
    }
}

impl Default for FederatedQueryGraphBuilders {
    fn default() -> Self {
        Self {
            graphql: FederatedQueryGraphBuilder::Graphql(Default::default()),
            connect: FederatedQueryGraphBuilder::Connect(Default::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sources::source::federated_query_graph::builder::FederatedQueryGraphBuilder;
    use crate::sources::source::federated_query_graph::builder::FederatedQueryGraphBuilders;
    use crate::sources::source::SourceKind;

    #[test]
    fn federated_query_graph_builder_field_invariant() {
        let builder = FederatedQueryGraphBuilders::default();
        assert!(matches!(
            builder.get_builder(SourceKind::Graphql),
            FederatedQueryGraphBuilder::Graphql(_)
        ));
        assert!(matches!(
            builder.get_builder(SourceKind::Connect),
            FederatedQueryGraphBuilder::Connect(_)
        ));
    }
}
