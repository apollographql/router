// No panics allowed from connectors code.
// Crashing the language server is a bad user experience, and panicking in the router is even worse.
#![cfg_attr(
    not(test),
    deny(
        clippy::exit,
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::unimplemented,
        clippy::todo
    )
)]
#![deny(nonstandard_style)]
#![deny(clippy::redundant_clone)]
#![deny(clippy::manual_while_let_some)]
#![deny(clippy::needless_borrow)]
#![deny(clippy::manual_ok_or)]
#![deny(clippy::needless_collect)]
#![deny(clippy::or_fun_call)]

use std::fmt::Display;
use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::Name;

pub mod expand;
mod header;
mod id;
mod json_selection;
mod models;
pub(crate) mod spec;
mod string_template;
pub mod validation;
pub(crate) mod variable;

use apollo_compiler::name;
use id::ConnectorPosition;
use id::ObjectTypeDefinitionDirectivePosition;
pub use json_selection::ApplyToError;
pub use json_selection::JSONSelection;
pub use json_selection::Key;
pub use json_selection::PathSelection;
pub use json_selection::SubSelection;
pub use models::CustomConfiguration;
pub use models::Header;
pub use spec::ConnectHTTPArguments;
pub use spec::ConnectSpec;
pub use spec::SourceHTTPArguments;
pub use string_template::Error;
pub use string_template::StringTemplate;
pub use variable::Namespace;

pub use self::models::Connector;
pub use self::models::EntityResolver;
pub use self::models::HTTPMethod;
pub use self::models::HeaderSource;
pub use self::models::HttpJsonTransport;
pub use self::models::MakeUriError;
pub use self::models::SourceName;
pub use self::spec::connect::ConnectBatchArguments;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

#[derive(Debug, Clone)]
pub struct ConnectId {
    pub label: String,
    pub subgraph_name: String,
    pub source_name: Option<SourceName>,
    pub(crate) directive: ConnectorPosition,
}

impl ConnectId {
    /// Create a synthetic name for this connect ID
    ///
    /// Until we have a source-aware query planner, we'll need to split up connectors into
    /// their own subgraphs when doing planning. Each subgraph will need a name, so we
    /// synthesize one using metadata present on the directive.
    pub(crate) fn synthetic_name(&self) -> String {
        format!("{}_{}", self.subgraph_name, self.directive.synthetic_name())
    }

    pub fn subgraph_source(&self) -> String {
        let source = self
            .source_name
            .as_ref()
            .map(SourceName::as_str)
            .unwrap_or_default();
        format!("{}.{}", self.subgraph_name, source)
    }

    pub fn coordinate(&self) -> String {
        format!("{}:{}", self.subgraph_name, self.directive.coordinate())
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
    /// Intended for tests in apollo-router
    pub fn new(
        subgraph_name: String,
        source_name: Option<SourceName>,
        type_name: Name,
        field_name: Name,
        index: usize,
        label: &str,
    ) -> Self {
        Self {
            label: label.to_string(),
            subgraph_name,
            source_name,
            directive: ConnectorPosition::Field(ObjectOrInterfaceFieldDirectivePosition {
                field: ObjectOrInterfaceFieldDefinitionPosition::Object(
                    ObjectFieldDefinitionPosition {
                        type_name,
                        field_name,
                    },
                ),
                directive_name: name!(connect),
                directive_index: index,
            }),
        }
    }

    /// Intended for tests in apollo-router
    pub fn new_on_object(
        subgraph_name: String,
        source_name: Option<SourceName>,
        type_name: Name,
        index: usize,
        label: &str,
    ) -> Self {
        Self {
            label: label.to_string(),
            subgraph_name,
            source_name,
            directive: ConnectorPosition::Type(ObjectTypeDefinitionDirectivePosition {
                type_name,
                directive_name: name!(connect),
                directive_index: index,
            }),
        }
    }
}
