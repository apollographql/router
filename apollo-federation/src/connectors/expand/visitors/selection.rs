use std::ops::Deref;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use indexmap::IndexMap;
use shape::Shape;
use shape::ShapeCase;

use super::filter_directives;
use super::try_insert;
use super::try_pre_insert;
use crate::error::FederationError;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::schema::position::EnumTypeDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::InterfaceTypeDefinitionPosition;
use crate::schema::position::ObjectTypeDefinitionPosition;
use crate::schema::position::ScalarTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::position::UnionTypeDefinitionPosition;

#[derive(Debug)]
struct TypeShapeWalker<'a> {
    original_schema: &'a ValidFederationSchema,
    to_schema: &'a mut FederationSchema,
    directive_deny_list: &'a IndexSet<Name>,
}

pub(crate) fn walk_type_with_shape(
    type_def_pos: &TypeDefinitionPosition,
    shape: &Shape,
    // These three parameters become a TypeShapeWalker that will be passed as
    // &self to walk_type.
    original_schema: &ValidFederationSchema,
    to_schema: &mut FederationSchema,
    directive_deny_list: &IndexSet<Name>,
) -> Result<(), FederationError> {
    TypeShapeWalker {
        original_schema,
        to_schema,
        directive_deny_list,
    }
    .walk_type(type_def_pos, shape)
}

impl<'a> TypeShapeWalker<'a> {
    fn walk_type(
        &mut self,
        type_def_pos: &TypeDefinitionPosition,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        match type_def_pos {
            TypeDefinitionPosition::Enum(enum_type_pos) => self.walk_enum(enum_type_pos, shape),
            TypeDefinitionPosition::InputObject(input_type_pos) => {
                self.walk_input_object(input_type_pos, shape)
            }
            TypeDefinitionPosition::Interface(interface_type_pos) => {
                self.walk_interface(interface_type_pos, shape)
            }
            TypeDefinitionPosition::Object(object_type_pos) => {
                self.walk_object(object_type_pos, shape)
            }
            TypeDefinitionPosition::Scalar(scalar_type_pos) => {
                self.walk_scalar(scalar_type_pos, shape)
            }
            TypeDefinitionPosition::Union(union_type_pos) => self.walk_union(union_type_pos, shape),
        }
    }

    fn walk_object(
        &mut self,
        object: &ObjectTypeDefinitionPosition,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        try_pre_insert!(self.to_schema, object)?;
        let def = object.get(self.original_schema.schema())?;
        let mut sub_type = ObjectType {
            description: def.description.clone(),
            name: def.name.clone(),
            implements_interfaces: def.implements_interfaces.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
            fields: IndexMap::with_hasher(Default::default()), // Will be filled in by the `visit` method for each field
        };

        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                for field_name_string in fields.keys() {
                    let field_name = Name::new(field_name_string)?;
                    let field = object
                        .field(field_name.clone())
                        .get(self.original_schema.schema())?;
                    let field_type = self
                        .original_schema
                        .get_type(field.ty.inner_named_type().clone())?;
                    let extended_field_type = field_type.get(self.original_schema.schema())?;

                    // We only need to care about the type of the field if it isn't built-in
                    if !extended_field_type.is_built_in() {
                        let nested_field_shape = shape.field(field_name_string, []);
                        self.walk_type(&field_type, &nested_field_shape)?;
                    }

                    // Add the field to the currently processing object, making sure
                    // to not overwrite if it already exists (and verify that we
                    // didn't change the type)
                    let new_field = FieldDefinition {
                        description: field.description.clone(),
                        name: field.name.clone(),
                        arguments: field.arguments.clone(),
                        ty: field.ty.clone(),
                        directives: filter_directives(self.directive_deny_list, &field.directives),
                    };
                    if let Some(old_field) = sub_type.fields.get(&field_name) {
                        if *old_field.deref().deref() != new_field {
                            return Err(FederationError::internal(format!(
                                "tried to write field to existing type, but field type was different. expected {new_field:?} found {old_field:?}"
                            )));
                        }
                    } else {
                        sub_type
                            .fields
                            .insert(field_name, Component::new(new_field));
                    }
                }
            }

            ShapeCase::One(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_object(object, member_shape)?;
                }
            }

            _ => todo!(),
        };

        try_insert!(self.to_schema, object, Node::new(sub_type))?;

        Ok(())
    }

    fn walk_interface(
        &mut self,
        interface: &InterfaceTypeDefinitionPosition,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        try_pre_insert!(self.to_schema, interface)?;
        let def = interface.get(self.original_schema.schema())?;
        let mut sub_type = InterfaceType {
            description: def.description.clone(),
            name: def.name.clone(),
            implements_interfaces: def.implements_interfaces.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
            fields: IndexMap::with_hasher(Default::default()), // Will be filled in by the `visit` method for each field
        };

        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                if let Some(type_name) = fields.get("__typename") {
                    if let ShapeCase::String(Some(literal_value)) = type_name.case() {
                        // TODO Check that type_name is one of the allowed values.
                        let type_def_pos =
                            self.original_schema.get_type(Name::new(literal_value)?)?;
                        self.walk_type(&type_def_pos, shape)?;
                    } else {
                        return Err(FederationError::internal(format!(
                            "expected __typename to be a string literal, found: {type_name:?}"
                        )));
                    }
                }

                for field_name_string in fields.keys() {
                    if field_name_string == "__typename" {
                        // Already handled above.
                        continue;
                    }

                    let field_name = Name::new(field_name_string)?;
                    let field = interface
                        .field(field_name.clone())
                        .get(self.original_schema.schema())?;
                    let field_type = self
                        .original_schema
                        .get_type(field.ty.inner_named_type().clone())?;
                    let extended_field_type = field_type.get(self.original_schema.schema())?;

                    // We only need to care about the type of the field if it isn't built-in
                    if !extended_field_type.is_built_in() {
                        let nested_field_shape = shape.field(field_name_string, []);
                        self.walk_type(&field_type, &nested_field_shape)?;
                    }

                    // Add the field to the currently processing object, making sure
                    // to not overwrite if it already exists (and verify that we
                    // didn't change the type)
                    let new_field = FieldDefinition {
                        description: field.description.clone(),
                        name: field.name.clone(),
                        arguments: field.arguments.clone(),
                        ty: field.ty.clone(),
                        directives: filter_directives(self.directive_deny_list, &field.directives),
                    };
                    if let Some(old_field) = sub_type.fields.get(&field_name) {
                        if *old_field.deref().deref() != new_field {
                            return Err(FederationError::internal(format!(
                                "tried to write field to existing type, but field type was different. expected {new_field:?} found {old_field:?}"
                            )));
                        }
                    } else {
                        sub_type
                            .fields
                            .insert(field_name, Component::new(new_field));
                    }
                }
            }

            ShapeCase::One(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_interface(interface, member_shape)?;
                }
            }

            _ => todo!(),
        };

        try_insert!(self.to_schema, interface, Node::new(sub_type))?;

        Ok(())
    }

    fn walk_union(
        &mut self,
        _union: &UnionTypeDefinitionPosition,
        _shape: &Shape,
    ) -> Result<(), FederationError> {
        // Unions are not yet handled for expansion
        Err(FederationError::internal(
            "unions are not yet handled for expansion",
        ))
    }

    fn walk_input_object(
        &mut self,
        _input: &InputObjectTypeDefinitionPosition,
        _shape: &Shape,
    ) -> Result<(), FederationError> {
        // Input objects are not yet handled for expansion
        Err(FederationError::internal(
            "input objects are not yet handled for expansion",
        ))
    }

    fn walk_scalar(
        &mut self,
        scalar: &ScalarTypeDefinitionPosition,
        _shape: &Shape, // TODO
    ) -> Result<(), FederationError> {
        let def = scalar.get(self.original_schema.schema())?;
        let def = ScalarType {
            description: def.description.clone(),
            name: def.name.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
        };
        try_pre_insert!(self.to_schema, scalar)?;
        try_insert!(self.to_schema, scalar, Node::new(def))?;
        Ok(())
    }

    fn walk_enum(
        &mut self,
        enum_type_pos: &EnumTypeDefinitionPosition,
        _shape: &Shape, // TODO
    ) -> Result<(), FederationError> {
        let def = enum_type_pos.get(self.original_schema.schema())?;
        let def = EnumType {
            description: def.description.clone(),
            name: def.name.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
            values: def.values.clone(),
        };
        try_pre_insert!(self.to_schema, enum_type_pos)?;
        try_insert!(self.to_schema, enum_type_pos, Node::new(def))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // TODO: Write these tests
}
