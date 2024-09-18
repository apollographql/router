use std::fmt::Display;
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::Name;

pub mod expand;
mod header;
mod json_selection;
mod models;
pub(crate) mod spec;
mod url_template;
pub mod validation;

use apollo_compiler::name;
pub use json_selection::ApplyToError;
pub use json_selection::JSONSelection;
pub use json_selection::Key;
pub use json_selection::PathSelection;
pub use json_selection::SubSelection;
pub use models::CustomConfiguration;
pub(crate) use spec::ConnectSpecDefinition;
pub use url_template::URLTemplate;
pub use url_template::Variable;

pub use self::models::Connector;
pub use self::models::EntityResolver;
pub use self::models::HTTPMethod;
pub use self::models::HeaderSource;
pub use self::models::HttpJsonTransport;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

#[derive(Debug, Clone)]
pub struct ConnectId {
    pub label: String,
    pub subgraph_name: String,
    pub source_name: Option<String>,
    pub(crate) directive: ObjectOrInterfaceFieldDirectivePosition,
}

impl ConnectId {
    /// Create a synthetic name for this connect ID
    ///
    /// Until we have a source-aware query planner, we'll need to split up connectors into
    /// their own subgraphs when doing planning. Each subgraph will need a name, so we
    /// synthesize one using metadata present on the directive.
    pub(crate) fn synthetic_name(&self) -> String {
        format!(
            "{}_{}_{}_{}",
            self.subgraph_name,
            self.directive.field.type_name(),
            self.directive.field.field_name(),
            self.directive.directive_index
        )
    }
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

impl ConnectId {
    /// Mostly intended for tests in apollo-router
    pub fn new(
        subgraph_name: String,
        source_name: Option<String>,
        type_name: Name,
        field_name: Name,
        index: usize,
        label: &str,
    ) -> Self {
        Self {
            label: label.to_string(),
            subgraph_name,
            source_name,
            directive: ObjectOrInterfaceFieldDirectivePosition {
                field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                    ObjectFieldDefinitionPosition {
                        type_name,
                        field_name,
                    },
                ),
                directive_name: name!(connect),
                directive_index: index,
            },
        }
    }
}
