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

use std::hash::Hash;
use std::hash::Hasher;

use apollo_compiler::Name;

pub mod expand;
pub mod header;
mod id;
mod json_selection;
mod models;
pub use models::ProblemLocation;
pub mod runtime;
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
pub(crate) use json_selection::SelectionTrie;
pub use json_selection::SubSelection;
pub use models::CustomConfiguration;
pub use models::Header;
use serde::Serialize;
pub use spec::ConnectHTTPArguments;
pub use spec::ConnectSpec;
pub use spec::SourceHTTPArguments;
pub use string_template::Error as StringTemplateError;
pub use string_template::StringTemplate;
pub(crate) use validation::field_set_is_subset;
pub use variable::Namespace;

pub use self::models::Connector;
pub use self::models::ConnectorErrorsSettings;
pub use self::models::EntityResolver;
pub use self::models::HTTPMethod;
pub use self::models::HeaderSource;
pub use self::models::HttpJsonTransport;
pub use self::models::Label;
pub use self::models::MakeUriError;
pub use self::models::OriginatingDirective;
pub use self::models::SourceName;
pub use self::spec::connect::ConnectBatchArguments;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

#[derive(Debug, Clone)]
pub struct ConnectId {
    pub subgraph_name: String,
    pub source_name: Option<SourceName>,
    pub named: Option<Name>,
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

    /// Connector ID Name
    pub fn name(&self) -> String {
        self.named
            .as_ref()
            .map_or_else(|| self.directive.coordinate(), |name| name.to_string())
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

    /// Intended for tests in apollo-router
    pub fn new(
        subgraph_name: String,
        source_name: Option<SourceName>,
        type_name: Name,
        field_name: Name,
        named: Option<Name>,
        index: usize,
    ) -> Self {
        Self {
            subgraph_name,
            source_name,
            named,
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
        named: Option<Name>,
        index: usize,
    ) -> Self {
        Self {
            subgraph_name,
            source_name,
            named,
            directive: ConnectorPosition::Type(ObjectTypeDefinitionDirectivePosition {
                type_name,
                directive_name: name!(connect),
                directive_index: index,
            }),
        }
    }
}

impl PartialEq<&str> for ConnectId {
    fn eq(&self, other: &&str) -> bool {
        let coordinate = self.directive.coordinate();
        let coordinate_non_indexed = coordinate.strip_suffix("[0]").unwrap_or(&coordinate);
        &coordinate == other
            || &coordinate_non_indexed == other
            || self
                .named
                .as_ref()
                .is_some_and(|name| &name.as_str() == other)
    }
}

impl PartialEq<String> for ConnectId {
    fn eq(&self, other: &String) -> bool {
        self == &other.as_str()
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

impl Serialize for ConnectId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_eq_str_with_index_0() {
        let id = ConnectId::new(
            "subgraph".to_string(),
            None,
            name!("type"),
            name!("field"),
            Some(name!("my_id")),
            0,
        );

        assert_eq!(id, "type.field[0]");
        assert_eq!(id, "type.field[0]".to_string());
        assert_eq!(id, "type.field");
        assert_eq!(id, "type.field".to_string());
        assert_eq!(id, "my_id");
        assert_eq!(id, "my_id".to_string());
    }

    #[test]
    fn id_eq_str_with_index_non_zero() {
        let id = ConnectId::new(
            "subgraph".to_string(),
            None,
            name!("type"),
            name!("field"),
            Some(name!("my_id")),
            10,
        );

        assert_eq!(id, "type.field[10]");
        assert_eq!(id, "type.field[10]".to_string());
        assert!(id != "type.field");
        assert_eq!(id, "my_id");
    }
}
