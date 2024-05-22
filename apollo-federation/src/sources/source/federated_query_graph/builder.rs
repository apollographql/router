#![allow(unused, unconditional_panic, clippy::diverging_sub_expression)]
use apollo_compiler::name;
use enum_dispatch::enum_dispatch;
use indexmap::IndexMap;

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
    builders: IndexMap<SourceKind, FederatedQueryGraphBuilder>,
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
        self.builders.get(&src_kind).unwrap()
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
            builders: IndexMap::from([
                (
                    SourceKind::Graphql,
                    FederatedQueryGraphBuilder::Graphql(Default::default()),
                ),
                (
                    SourceKind::Connect,
                    FederatedQueryGraphBuilder::Connect(Default::default()),
                ),
            ]),
        }
    }
}
