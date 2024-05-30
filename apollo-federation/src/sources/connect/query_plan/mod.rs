use apollo_compiler::ast::Name;
use apollo_compiler::ast::Value;
use indexmap::IndexMap;

use crate::sources::connect::ConnectId;
use crate::sources::connect::JSONSelection;

pub mod query_planner;

#[derive(Debug, Clone, PartialEq)]
pub struct FetchNode {
    pub source_id: ConnectId,
    pub field_response_name: Name,              // aliasing
    pub field_arguments: IndexMap<Name, Value>, // req
    pub selection: JSONSelection,               // res
}
