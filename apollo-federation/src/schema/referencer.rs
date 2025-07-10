use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;

use super::FederationSchema;
use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::position::DirectiveArgumentDefinitionPosition;
use crate::schema::position::DirectiveTargetPosition;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::EnumValueDefinitionPosition;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceFieldArgumentDefinitionPosition;
use crate::schema::position::InterfaceFieldDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectFieldArgumentDefinitionPosition;
use crate::schema::position::ObjectFieldDefinitionPosition;
use crate::schema::position::ObjectOrInterfaceFieldDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::SchemaDefinitionPosition;
use crate::schema::position::SchemaRootDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;
use crate::schema::position::UnionTypenameFieldDefinitionPosition;

#[derive(Debug, Clone, Default)]
pub(crate) struct Referencers {
    pub(crate) scalar_types: IndexMap<Name, ScalarTypeReferencers>,
    pub(crate) object_types: IndexMap<Name, ObjectTypeReferencers>,
    pub(crate) interface_types: IndexMap<Name, InterfaceTypeReferencers>,
    pub(crate) union_types: IndexMap<Name, UnionTypeReferencers>,
    pub(crate) enum_types: IndexMap<Name, EnumTypeReferencers>,
    pub(crate) input_object_types: IndexMap<Name, InputObjectTypeReferencers>,
    pub(crate) directives: IndexMap<Name, DirectiveReferencers>,
}

impl Referencers {
    pub(crate) fn contains_type_name(&self, name: &str) -> bool {
        self.scalar_types.contains_key(name)
            || self.object_types.contains_key(name)
            || self.interface_types.contains_key(name)
            || self.union_types.contains_key(name)
            || self.enum_types.contains_key(name)
            || self.input_object_types.contains_key(name)
    }

    pub(crate) fn get_interface_type(
        &self,
        name: &str,
    ) -> Result<&InterfaceTypeReferencers, FederationError> {
        self.interface_types.get(name).ok_or_else(|| {
            SingleFederationError::Internal {
                message: "Interface type referencers unexpectedly missing type".to_owned(),
            }
            .into()
        })
    }

    pub(crate) fn get_directive(
        &self,
        name: &str,
    ) -> Result<&DirectiveReferencers, FederationError> {
        self.directives.get(name).ok_or_else(|| {
            internal_error!("Directive referencers unexpectedly missing directive `{name}`")
        })
    }

    pub(crate) fn get_directive_applications<'schema>(
        &self,
        schema: &'schema FederationSchema,
        name: &Name,
    ) -> Result<
        impl Iterator<Item = (DirectiveTargetPosition, &'schema Node<ast::Directive>)>,
        FederationError,
    > {
        let directive_referencers = self.get_directive(name)?;
        Ok(directive_referencers.iter().flat_map(|pos| {
            pos.get_applied_directives(schema, name)
                .into_iter()
                .map(move |directive_application| (pos.clone(), directive_application))
        }))
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ScalarTypeReferencers {
    pub(crate) object_fields: IndexSet<ObjectFieldDefinitionPosition>,
    pub(crate) object_field_arguments: IndexSet<ObjectFieldArgumentDefinitionPosition>,
    pub(crate) interface_fields: IndexSet<InterfaceFieldDefinitionPosition>,
    pub(crate) interface_field_arguments: IndexSet<InterfaceFieldArgumentDefinitionPosition>,
    pub(crate) union_fields: IndexSet<UnionTypenameFieldDefinitionPosition>,
    pub(crate) input_object_fields: IndexSet<InputObjectFieldDefinitionPosition>,
    pub(crate) directive_arguments: IndexSet<DirectiveArgumentDefinitionPosition>,
}

impl ScalarTypeReferencers {
    pub(crate) fn len(&self) -> usize {
        self.object_fields.len()
            + self.object_field_arguments.len()
            + self.interface_fields.len()
            + self.interface_field_arguments.len()
            + self.union_fields.len()
            + self.input_object_fields.len()
            + self.directive_arguments.len()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ObjectTypeReferencers {
    pub(crate) schema_roots: IndexSet<SchemaRootDefinitionPosition>,
    pub(crate) object_fields: IndexSet<ObjectFieldDefinitionPosition>,
    pub(crate) interface_fields: IndexSet<InterfaceFieldDefinitionPosition>,
    pub(crate) union_types: IndexSet<UnionTypeDefinitionPosition>,
}

impl ObjectTypeReferencers {
    pub(crate) fn len(&self) -> usize {
        self.schema_roots.len()
            + self.object_fields.len()
            + self.interface_fields.len()
            + self.union_types.len()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InterfaceTypeReferencers {
    pub(crate) object_types: IndexSet<ObjectTypeDefinitionPosition>,
    pub(crate) object_fields: IndexSet<ObjectFieldDefinitionPosition>,
    pub(crate) interface_types: IndexSet<InterfaceTypeDefinitionPosition>,
    pub(crate) interface_fields: IndexSet<InterfaceFieldDefinitionPosition>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct UnionTypeReferencers {
    pub(crate) object_fields: IndexSet<ObjectFieldDefinitionPosition>,
    pub(crate) interface_fields: IndexSet<InterfaceFieldDefinitionPosition>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct EnumTypeReferencers {
    pub(crate) object_fields: IndexSet<ObjectFieldDefinitionPosition>,
    pub(crate) object_field_arguments: IndexSet<ObjectFieldArgumentDefinitionPosition>,
    pub(crate) interface_fields: IndexSet<InterfaceFieldDefinitionPosition>,
    pub(crate) interface_field_arguments: IndexSet<InterfaceFieldArgumentDefinitionPosition>,
    pub(crate) input_object_fields: IndexSet<InputObjectFieldDefinitionPosition>,
    pub(crate) directive_arguments: IndexSet<DirectiveArgumentDefinitionPosition>,
}

impl EnumTypeReferencers {
    pub(crate) fn len(&self) -> usize {
        self.object_fields.len()
            + self.object_field_arguments.len()
            + self.interface_fields.len()
            + self.interface_field_arguments.len()
            + self.input_object_fields.len()
            + self.directive_arguments.len()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct InputObjectTypeReferencers {
    pub(crate) object_field_arguments: IndexSet<ObjectFieldArgumentDefinitionPosition>,
    pub(crate) interface_field_arguments: IndexSet<InterfaceFieldArgumentDefinitionPosition>,
    pub(crate) input_object_fields: IndexSet<InputObjectFieldDefinitionPosition>,
    pub(crate) directive_arguments: IndexSet<DirectiveArgumentDefinitionPosition>,
}

impl InputObjectTypeReferencers {
    pub(crate) fn len(&self) -> usize {
        self.object_field_arguments.len()
            + self.interface_field_arguments.len()
            + self.input_object_fields.len()
            + self.directive_arguments.len()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DirectiveReferencers {
    pub(crate) schema: Option<SchemaDefinitionPosition>,
    pub(crate) scalar_types: IndexSet<ScalarTypeDefinitionPosition>,
    pub(crate) object_types: IndexSet<ObjectTypeDefinitionPosition>,
    pub(crate) object_fields: IndexSet<ObjectFieldDefinitionPosition>,
    pub(crate) object_field_arguments: IndexSet<ObjectFieldArgumentDefinitionPosition>,
    pub(crate) interface_types: IndexSet<InterfaceTypeDefinitionPosition>,
    pub(crate) interface_fields: IndexSet<InterfaceFieldDefinitionPosition>,
    pub(crate) interface_field_arguments: IndexSet<InterfaceFieldArgumentDefinitionPosition>,
    pub(crate) union_types: IndexSet<UnionTypeDefinitionPosition>,
    pub(crate) enum_types: IndexSet<EnumTypeDefinitionPosition>,
    pub(crate) enum_values: IndexSet<EnumValueDefinitionPosition>,
    pub(crate) input_object_types: IndexSet<InputObjectTypeDefinitionPosition>,
    pub(crate) input_object_fields: IndexSet<InputObjectFieldDefinitionPosition>,
    pub(crate) directive_arguments: IndexSet<DirectiveArgumentDefinitionPosition>,
}

impl DirectiveReferencers {
    pub(crate) fn object_or_interface_fields(
        &self,
    ) -> impl Iterator<Item = ObjectOrInterfaceFieldDefinitionPosition> {
        self.object_fields
            .iter()
            .map(|pos| ObjectOrInterfaceFieldDefinitionPosition::Object(pos.clone()))
            .chain(
                self.interface_fields
                    .iter()
                    .map(|pos| ObjectOrInterfaceFieldDefinitionPosition::Interface(pos.clone())),
            )
    }

    pub(crate) fn composite_type_positions(
        &self,
    ) -> impl Iterator<Item = CompositeTypeDefinitionPosition> {
        self.object_types
            .iter()
            .map(|t| CompositeTypeDefinitionPosition::from(t.clone()))
            .chain(self.interface_types.iter().map(|t| t.clone().into()))
            .chain(self.union_types.iter().map(|t| t.clone().into()))
    }

    pub(crate) fn extend(&mut self, other: &Self) {
        if let Some(schema) = &other.schema {
            self.schema = Some(schema.clone());
        }
        self.scalar_types.extend(other.scalar_types.iter().cloned());
        self.object_types.extend(other.object_types.iter().cloned());
        self.object_fields
            .extend(other.object_fields.iter().cloned());
        self.object_field_arguments
            .extend(other.object_field_arguments.iter().cloned());
        self.interface_types
            .extend(other.interface_types.iter().cloned());
        self.interface_fields
            .extend(other.interface_fields.iter().cloned());
        self.interface_field_arguments
            .extend(other.interface_field_arguments.iter().cloned());
        self.union_types.extend(other.union_types.iter().cloned());
        self.enum_types.extend(other.enum_types.iter().cloned());
        self.enum_values.extend(other.enum_values.iter().cloned());
        self.input_object_types
            .extend(other.input_object_types.iter().cloned());
        self.input_object_fields
            .extend(other.input_object_fields.iter().cloned());
        self.directive_arguments
            .extend(other.directive_arguments.iter().cloned());
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = DirectiveTargetPosition> {
        let schema = self
            .schema
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::Schema);
        let scalar_types = self
            .scalar_types
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::ScalarType);
        let object_types = self
            .object_types
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::ObjectType);
        let object_fields = self
            .object_fields
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::ObjectField);
        let object_field_arguments = self
            .object_field_arguments
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::ObjectFieldArgument);
        let interface_types = self
            .interface_types
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::InterfaceType);
        let interface_fields = self
            .interface_fields
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::InterfaceField);
        let interface_field_arguments = self
            .interface_field_arguments
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::InterfaceFieldArgument);
        let union_types = self
            .union_types
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::UnionType);
        let enum_types = self
            .enum_types
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::EnumType);
        let enum_values = self
            .enum_values
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::EnumValue);
        let input_object_types = self
            .input_object_types
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::InputObjectType);
        let input_object_fields = self
            .input_object_fields
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::InputObjectField);
        let directive_arguments = self
            .directive_arguments
            .iter()
            .cloned()
            .map(DirectiveTargetPosition::DirectiveArgument);

        schema
            .chain(scalar_types)
            .chain(object_types)
            .chain(object_fields)
            .chain(object_field_arguments)
            .chain(interface_types)
            .chain(interface_fields)
            .chain(interface_field_arguments)
            .chain(union_types)
            .chain(enum_types)
            .chain(enum_values)
            .chain(input_object_types)
            .chain(input_object_fields)
            .chain(directive_arguments)
    }
}
