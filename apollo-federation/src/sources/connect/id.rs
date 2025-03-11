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
    #[allow(unused)]
    Type(ObjectTypeDefinitionDirectivePosition),
}

/// Reifies the connector position into schema definitions
#[derive(Debug)]
pub(crate) enum ConnectedElement<'schema> {
    Field {
        parent_type: &'schema ExtendedType,
        field_def: &'schema Component<FieldDefinition>,
    },
    Type {
        #[allow(unused)]
        type_def: &'schema ExtendedType,
    },
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

    pub(super) fn on_query(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .query
            .as_ref()
            .map(|query| match self {
                ConnectorPosition::Field(pos) => *pos.field.type_name() == query.name,
                ConnectorPosition::Type(_) => false,
            })
            .unwrap_or_default()
    }

    pub(super) fn on_mutation(&self, schema: &Schema) -> bool {
        schema
            .schema_definition
            .mutation
            .as_ref()
            .map(|mutation| match self {
                ConnectorPosition::Field(pos) => *pos.field.type_name() == mutation.name,
                ConnectorPosition::Type(_) => false,
            })
            .unwrap_or_default()
    }
}
