use apollo_compiler::Name;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;

use crate::error::FederationError;
use crate::error::SingleFederationError;
use crate::internal_error;
use crate::schema::position::DirectiveArgumentDefinitionPosition;
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

impl InterfaceTypeReferencers {
    pub(crate) fn len(&self) -> usize {
        self.object_types.len()
            + self.object_fields.len()
            + self.interface_types.len()
            + self.interface_fields.len()
    }
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
}
