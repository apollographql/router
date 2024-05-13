use apollo_compiler::ast::OperationType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::NodeStr;

use crate::sources::graphql::GraphqlId;

pub mod query_planner;

#[derive(Debug, Clone, PartialEq)]
pub struct FetchNode {
    pub source_id: GraphqlId,
    pub operation_document: Valid<ExecutableDocument>,
    pub operation_name: Option<NodeStr>,
    pub operation_kind: OperationType,
}
