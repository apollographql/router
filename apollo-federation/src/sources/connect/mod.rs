use std::fmt::Display;

use apollo_compiler::NodeStr;

use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
mod json_selection;
mod models;
pub mod query_plan;
pub(crate) mod spec;
mod url_path_template;

pub use json_selection::ApplyTo;
pub use json_selection::ApplyToError;
pub use json_selection::JSONSelection;
pub use json_selection::PathSelection;
pub use json_selection::Property;
pub use json_selection::SubSelection;
pub(crate) use spec::ConnectSpecDefinition;
pub use url_path_template::URLPathTemplate;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ConnectId {
    pub label: String,
    pub subgraph_name: NodeStr,
    pub directive: ObjectOrInterfaceFieldDirectivePosition,
}

impl Display for ConnectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}
