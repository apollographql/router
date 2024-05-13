use std::sync::Arc;

use apollo_compiler::ast::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use indexmap::IndexMap;

use crate::error::FederationError;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::query_plan::QueryPlan;
use crate::sources::source;
use crate::sources::source::SourceId;
use crate::sources::source::SourceKind;
use crate::Supergraph;

pub struct QueryPlanner {
    config: QueryPlannerConfig,
    federated_query_graph: Arc<FederatedQueryGraph>,
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
}

impl QueryPlanner {
    pub fn new(
        _supergraph: &Supergraph,
        _config: QueryPlannerConfig,
    ) -> Result<Self, FederationError> {
        todo!()
    }

    pub fn build_query_plan(
        &self,
        _document: &Valid<ExecutableDocument>,
        _operation_name: Option<Name>,
    ) -> Result<QueryPlan, FederationError> {
        todo!()
    }

    pub fn execution_metadata(
        &self,
        _source_kind: SourceKind,
    ) -> IndexMap<SourceId, source::query_plan::query_planner::ExecutionMetadata> {
        todo!()
    }
}
