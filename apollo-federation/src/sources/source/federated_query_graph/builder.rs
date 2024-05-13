use apollo_compiler::NodeStr;
use enum_dispatch::enum_dispatch;
use indexmap::IndexMap;

use crate::error::FederationError;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::sources::connect;
use crate::sources::graphql;
use crate::sources::source::SourceKind;
use crate::ValidFederationSubgraph;

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
    fn new() -> Self {
        todo!()
    }

    fn process_subgraph_schemas(
        &self,
        _subgraphs_by_name: IndexMap<NodeStr, ValidFederationSubgraph>,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError> {
        todo!()
    }
}
