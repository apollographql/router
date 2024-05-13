use crate::error::FederationError;
use crate::source_aware::federated_query_graph::builder::IntraSourceQueryGraphBuilderApi;
use crate::sources::source::federated_query_graph::builder::FederatedQueryGraphBuilderApi;
use crate::ValidFederationSubgraph;

pub(crate) struct FederatedQueryGraphBuilder;

impl FederatedQueryGraphBuilderApi for FederatedQueryGraphBuilder {
    fn process_subgraph_schema(
        &self,
        _subgraph: ValidFederationSubgraph,
        _builder: &mut impl IntraSourceQueryGraphBuilderApi,
    ) -> Result<(), FederationError> {
        todo!()
    }
}
