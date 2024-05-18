use std::sync::Arc;

use apollo_compiler::ast::Name;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use indexmap::IndexMap;

use crate::error::FederationError;
use crate::query_plan::query_planner::QueryPlannerConfig;
use crate::schema::ValidFederationSchema;
use crate::source_aware::federated_query_graph::builder::build_federated_query_graph;
use crate::source_aware::federated_query_graph::FederatedQueryGraph;
use crate::source_aware::query_plan::QueryPlan;
use crate::sources::source;
use crate::sources::source::SourceId;
use crate::sources::source::SourceKind;
use crate::ApiSchemaOptions;
use crate::Supergraph;

pub struct QueryPlanner {
    config: QueryPlannerConfig,
    federated_query_graph: Arc<FederatedQueryGraph>,
    supergraph_schema: ValidFederationSchema,
    api_schema: ValidFederationSchema,
}

impl QueryPlanner {
    pub fn new(
        supergraph: &Supergraph,
        config: QueryPlannerConfig,
    ) -> Result<Self, FederationError> {
        config.assert_valid();

        let supergraph_schema = supergraph.schema.clone();
        let api_schema = supergraph.to_api_schema(ApiSchemaOptions {
            include_defer: config.incremental_delivery.enable_defer,
            ..Default::default()
        })?;
        let federated_query_graph = Arc::new(build_federated_query_graph(
            supergraph_schema.clone(),
            api_schema.clone(),
            Some(true),
            Some(true),
        )?);
        Ok(QueryPlanner {
            config,
            federated_query_graph,
            supergraph_schema,
            api_schema,
        })
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
