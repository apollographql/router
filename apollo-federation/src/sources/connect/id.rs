use std::fmt;
use std::fmt::Display;
use std::fmt::Formatter;
use std::hash::Hash;

use apollo_compiler::Name;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;

use crate::error::FederationError;
use crate::schema::position::ObjectOrInterfaceFieldDirectivePosition;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) struct ObjectTypeDefinitionDirectivePosition {
    pub(super) type_name: Name,
    pub(super) directive_name: Name,
    pub(super) directive_index: usize,
}

/// Stores information about the position of the @connect directive, either
/// on a field or on a type.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum ConnectorPosition {
    Field(ObjectOrInterfaceFieldDirectivePosition),
    Type(ObjectTypeDefinitionDirectivePosition),
}

impl ConnectorPosition {
    pub(crate) fn element<'s>(
        &self,
        schema: &'s Schema,
    ) -> Result<ConnectedElement<'s>, FederationError> {
        match self {
            Self::Field(pos) => Ok(ConnectedElement::Field {
                parent_type: schema.types.get(pos.field.parent().type_name()).ok_or(
                    FederationError::internal("Parent type for connector not found"),
                )?,
                field_def: pos.field.get(schema).map_err(|_| {
                    FederationError::internal("Field definition for connector not found")
                })?,
            }),
            Self::Type(pos) => Ok(ConnectedElement::Type {
                type_def: schema
                    .types
                    .get(&pos.type_name)
                    .ok_or(FederationError::internal("Type for connector not found"))?,
            }),
        }
    }

    // Only connectors on fields have a parent type (a root type or an entity type)
    pub(crate) fn parent_type_name(&self) -> Option<Name> {
        match self {
            ConnectorPosition::Field(pos) => Some(pos.field.type_name().clone()),
            ConnectorPosition::Type(_) => None,
        }
    }

    // The "base" type is the type returned by the connector. For connectors
    // on fields, this is the field return type. For connectors on types, this
    // is the type itself.
    pub(crate) fn base_type_name(&self, schema: &Schema) -> Option<NamedType> {
        match self {
            ConnectorPosition::Field(_) => self
                .field_definition(schema)
                .map(|field| field.ty.inner_named_type().clone()),
            ConnectorPosition::Type(pos) => Some(pos.type_name.clone()),
        }
    }

    pub(crate) fn field_definition<'s>(
        &self,
        schema: &'s Schema,
    ) -> Option<&'s Component<FieldDefinition>> {
        match self {
            ConnectorPosition::Field(pos) => pos.field.get(schema).ok(),
            ConnectorPosition::Type(_) => None,
        }
    }

    pub(crate) fn coordinate(&self) -> String {
        match self {
            ConnectorPosition::Field(pos) => format!(
                "{}.{}@{}[{}]",
                pos.field.type_name(),
                pos.field.field_name(),
                pos.directive_name,
                pos.directive_index,
            ),
            ConnectorPosition::Type(pos) => format!(
                "{}@{}[{}]",
                pos.type_name, pos.directive_name, pos.directive_index,
            ),
        }
    }

    pub(crate) fn synthetic_name(&self) -> String {
        match self {
            ConnectorPosition::Field(pos) => format!(
                "{}_{}_{}",
                pos.field.type_name(),
                pos.field.field_name(),
                pos.directive_index,
            ),
            ConnectorPosition::Type(pos) => format!("{}_{}", pos.type_name, pos.directive_index),
        }
    }

    pub(super) fn on_root_type(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| match self {
                ConnectorPosition::Field(pos) => *pos.field.type_name() == query.name,
                ConnectorPosition::Type(_) => false,
            })
            || schema
                .schema_definition
                .mutation
                .as_ref()
                .is_some_and(|mutation| match self {
                    ConnectorPosition::Field(pos) => *pos.field.type_name() == mutation.name,
                    ConnectorPosition::Type(_) => false,
                })
    }
}

/// Reifies the connector position into schema definitions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectedElement<'schema> {
    Field {
        parent_type: &'schema ExtendedType,
        field_def: &'schema Component<FieldDefinition>,
    },
    Type {
        type_def: &'schema ExtendedType,
    },
}

impl Display for ConnectedElement<'_> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match self {
            Self::Field {
                parent_type,
                field_def,
            } => write!(f, "{}.{}", parent_type.name(), field_def.name),
            Self::Type { type_def } => write!(f, "{}", type_def.name()),
        }
    }
}

impl ConnectedElement<'_> {
    pub(super) fn on_root_type(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| match self {
                ConnectedElement::Field { parent_type, .. } => *parent_type.name() == query.name,
                ConnectedElement::Type { .. } => false,
            })
            || schema
                .schema_definition
                .mutation
                .as_ref()
                .is_some_and(|mutation| match self {
                    ConnectedElement::Field { parent_type, .. } => {
                        *parent_type.name() == mutation.name
                    }
                    ConnectedElement::Type { .. } => false,
                })
    }
}
