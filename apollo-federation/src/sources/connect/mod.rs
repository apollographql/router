use std::fmt::Display;
use std::hash::Hash;
use std::hash::Hasher;
use std::sync::Arc;

use apollo_compiler::NodeStr;
use indexmap::IndexMap;

mod json_selection;
mod models;
pub(crate) mod spec;
mod url_path_template;

pub use json_selection::ApplyTo;
pub use json_selection::ApplyToError;
pub use json_selection::JSONSelection;
pub use json_selection::Key;
pub use json_selection::PathSelection;
pub use json_selection::SubSelection;
pub use models::validate;
pub use models::Location;
pub use models::ValidationError;
pub use models::ValidationErrorCode;
pub(crate) use spec::ConnectSpecDefinition;
pub use url_path_template::URLPathTemplate;

pub use self::models::Connector;
pub use self::models::HTTPMethod;
pub use self::models::HttpJsonTransport;
pub use self::models::Transport;
use super::to_remove::SourceId;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

pub type Connectors = Arc<IndexMap<SourceId, Connector>>;

#[derive(Debug, Clone)]
pub struct ConnectId {
    pub label: String,
    pub subgraph_name: NodeStr,
    pub directive: ObjectOrInterfaceFieldDirectivePosition,
}

impl PartialEq for ConnectId {
    fn eq(&self, other: &Self) -> bool {
        self.subgraph_name == other.subgraph_name && self.directive == other.directive
    }
}

impl Eq for ConnectId {}

impl Hash for ConnectId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.subgraph_name.hash(state);
        self.directive.hash(state);
    }
}

impl Display for ConnectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}
