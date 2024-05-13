use std::fmt::Display;
use std::sync::Arc;

use apollo_compiler::NodeStr;
use indexmap::IndexMap;

use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

pub(crate) mod federated_query_graph;
pub(crate) mod fetch_dependency_graph;
mod models;
pub mod query_plan;
mod selection_parser;
pub(crate) mod spec;
mod url_path_template;

pub use selection_parser::ApplyTo;
pub use selection_parser::ApplyToError;
pub use selection_parser::Selection;
pub use selection_parser::SubSelection;
pub(crate) use spec::ConnectSpecDefinition;
pub use url_path_template::URLPathTemplate;

pub use self::models::HTTPMethod;
pub use self::models::HttpJsonTransport;
pub use self::models::Transport;

pub type ConnectorTransports = Arc<IndexMap<ConnectId, Transport>>;

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
