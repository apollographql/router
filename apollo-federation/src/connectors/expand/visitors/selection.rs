use std::ops::Deref;

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::EnumType;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::InterfaceType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::schema::ScalarType;
use apollo_compiler::schema::UnionType;
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
        let mut new_object_type = ObjectType {
            description: def.description.clone(),
            name: def.name.clone(),
            implements_interfaces: def.implements_interfaces.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
            fields: IndexMap::with_hasher(Default::default()), // Will be filled in by the `visit` method for each field
        };

        self.walk_object_helper(object, &mut new_object_type, shape)?;

        try_insert!(self.to_schema, object, Node::new(new_object_type))?;

        Ok(())
    }

    fn walk_object_helper(
        &mut self,
        object: &ObjectTypeDefinitionPosition,
        new_object_type: &mut ObjectType,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        let object_type_name = object.type_name.to_string();

        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                for (field_name_string, field_shape) in fields.iter() {
                    if field_name_string == "__typename" {
                        match field_shape.case() {
                            ShapeCase::String(Some(literal_string)) => {
                                if literal_string != &object_type_name {
                                    return Err(FederationError::internal(format!(
                                        "expected __typename to be {object_type_name}, found: {literal_string}"
                                    )));
                                }
                            }
                            _ => {
                                return Err(FederationError::internal(format!(
                                    "expected __typename to be a string literal, found: {}",
                                    field_shape.pretty_print(),
                                )));
                            }
                        };

                        continue;
                    }

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
                    if let Some(old_field) = new_object_type.fields.get(&field_name) {
                        if *old_field.deref().deref() != new_field {
                            return Err(FederationError::internal(format!(
                                "tried to write field to existing type, but field type was different. expected {new_field:?} found {old_field:?}"
                            )));
                        }
                    } else {
                        new_object_type
                            .fields
                            .insert(field_name, Component::new(new_field));
                    }
                }
            }

            ShapeCase::Array { prefix, tail } => {
                for shape in prefix {
                    self.walk_object_helper(object, new_object_type, shape)?;
                }
                self.walk_object_helper(object, new_object_type, tail)?;
            }

            ShapeCase::One(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_object_helper(object, new_object_type, member_shape)?;
                }
            }

            ShapeCase::All(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_object_helper(object, new_object_type, member_shape)?;
                }
            }

            ShapeCase::None | ShapeCase::Null | ShapeCase::Unknown => {
                // None or Null might be fine if the object is nullable where
                // it's used, but we can't tell that from
                // ObjectTypeDefinitionPosition alone.
                //
                // Unknown is included here because it might represent a valid
                // value at runtime, and there's nothing to be validated about
                // it right now.
            }

            ShapeCase::Name(name, weak) => {
                if let Some(named_shape) = weak.upgrade(name) {
                    self.walk_object_helper(object, new_object_type, &named_shape)?;
                } else {
                    // Named shapes are like placeholders for future shapes that
                    // may be defined later, and since they might be valid
                    // later, we have nothing to warn about now.
                }
            }

            ShapeCase::Bool(_) | ShapeCase::String(_) | ShapeCase::Int(_) | ShapeCase::Float => {
                return Err(FederationError::internal(format!(
                    "Unexpected primitive {} provided for object type {}",
                    shape.pretty_print(),
                    object.type_name.as_str(),
                )));
            }

            ShapeCase::Error(shape::Error { partial, .. }) => {
                if let Some(partial) = partial {
                    // Errors with partial shapes still mostly behave like those
                    // shapes (except for simplification), so we need to
                    // validate the object against the partial shape.
                    self.walk_object_helper(object, new_object_type, partial)?;
                }
            }
        };

        Ok(())
    }

    fn walk_interface(
        &mut self,
        interface: &InterfaceTypeDefinitionPosition,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        try_pre_insert!(self.to_schema, interface)?;
        let def = interface.get(self.original_schema.schema())?;
        let sub_type = InterfaceType {
            description: def.description.clone(),
            name: def.name.clone(),
            implements_interfaces: def.implements_interfaces.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
            // The interface type gets its fields from the original interface,
            // since the shape is likely to be either just an object or a union
            // of objects representing concrete types, not the interface itself.
            fields: def.fields.clone(),
        };

        self.walk_interface_helper(interface, shape)?;

        try_insert!(self.to_schema, interface, Node::new(sub_type))?;

        Ok(())
    }

    fn get_concrete_type_names_for_interface(&self, interface_name: &str) -> IndexSet<&str> {
        self.original_schema
            .schema()
            .types
            .values()
            .filter_map(|extended_type| {
                if let ExtendedType::Object(obj) = extended_type {
                    if obj.implements_interfaces.contains(interface_name) {
                        Some(obj.name.as_str())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .collect()
    }

    fn walk_interface_helper(
        &mut self,
        interface: &InterfaceTypeDefinitionPosition,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        let concrete_name_set =
            self.get_concrete_type_names_for_interface(interface.type_name.as_str());

        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                if let Some(type_name) = fields.get("__typename") {
                    if let ShapeCase::String(Some(literal_value)) = type_name.case() {
                        if !concrete_name_set.contains(literal_value.as_str()) {
                            return Err(FederationError::internal(format!(
                                "Type '{}' is not a valid concrete type for interface '{}'",
                                literal_value, interface.type_name
                            )));
                        }

                        let type_def_pos =
                            self.original_schema.get_type(Name::new(literal_value)?)?;

                        self.walk_type(&type_def_pos, shape)?;
                    } else {
                        return Err(FederationError::internal(format!(
                            "expected __typename to be a string literal, found: {type_name:?}"
                        )));
                    }
                } else {
                    return Err(FederationError::internal(
                        "interface types require a valid __typename".to_string(),
                    ));
                }
            }

            ShapeCase::Array { prefix, tail } => {
                for shape in prefix {
                    self.walk_interface_helper(interface, shape)?;
                }
                self.walk_interface_helper(interface, tail)?;
            }

            ShapeCase::One(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_interface_helper(interface, member_shape)?;
                }
            }

            ShapeCase::All(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_interface_helper(interface, member_shape)?;
                }
            }

            ShapeCase::None | ShapeCase::Null | ShapeCase::Unknown => {
                // None or Null might be fine if the interface is nullable where
                // it's used, but we can't tell that from
                // InterfaceTypeDefinitionPosition alone.
                //
                // We include Unknown here because we are generally tolerant of
                // Unknown values in validation.
            }

            ShapeCase::Name(_, _) => {
                // Named shapes are like placeholders for future shapes that
                // may be defined later, and since they might be valid
                // later, we have nothing to warn about now.
            }

            ShapeCase::Bool(_) | ShapeCase::String(_) | ShapeCase::Int(_) | ShapeCase::Float => {
                return Err(FederationError::internal(format!(
                    "Unexpected primitive shape provided for interface type: {}",
                    shape.pretty_print()
                )));
            }

            ShapeCase::Error(shape::Error { partial, .. }) => {
                if let Some(partial) = partial {
                    // Errors with partial shapes still mostly behave like those
                    // shapes (except for simplification), so we need to
                    // validate the interface against the partial shape.
                    self.walk_interface_helper(interface, partial)?;
                }
            }
        };

        Ok(())
    }

    fn walk_union(
        &mut self,
        union_type_pos: &UnionTypeDefinitionPosition,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        // Similar to walk_interface, except there are only member object types,
        // no parent supertype.
        try_pre_insert!(self.to_schema, union_type_pos)?;

        let def = union_type_pos.get(self.original_schema.schema())?;
        let sub_type = UnionType {
            description: def.description.clone(),
            name: def.name.clone(),
            directives: filter_directives(self.directive_deny_list, &def.directives),
            members: def.members.clone(),
        };

        for member_name in def.members.iter() {
            if let TypeDefinitionPosition::Object(object_type_pos) =
                self.original_schema.get_type(member_name.name.clone())?
            {
                try_pre_insert!(self.to_schema, object_type_pos)?;
            }
        }

        self.walk_union_helper(def, shape)?;

        try_insert!(self.to_schema, union_type_pos, Node::new(sub_type))?;

        Ok(())
    }

    fn walk_union_helper(
        &mut self,
        union_type: &Node<UnionType>,
        shape: &Shape,
    ) -> Result<(), FederationError> {
        match shape.case() {
            ShapeCase::Object { fields, .. } => {
                if let Some(type_name) = fields.get("__typename") {
                    if let ShapeCase::String(Some(literal_value)) = type_name.case() {
                        let member_name = Name::new(literal_value)?;
                        if union_type.members.contains(&member_name) {
                            let type_def_pos = self.original_schema.get_type(member_name)?;
                            self.walk_type(&type_def_pos, shape)?;
                        } else {
                            return Err(FederationError::internal(format!(
                                "expected __typename to be one of the union members ({}), found: {literal_value}",
                                union_type
                                    .members
                                    .iter()
                                    .map(|n| {
                                        // We want each typename to be quoted like a
                                        // JSON string. It's unlikely that any
                                        // GraphQL __typename string will need
                                        // character escaping, but we're all hedged
                                        // up if such a possibility comes to pass.
                                        serde_json_bytes::Value::String(n.name.to_string().into())
                                            .to_string()
                                    })
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )));
                        }
                    } else {
                        return Err(FederationError::internal(format!(
                            "expected __typename to be a string literal, found: {}",
                            type_name.pretty_print()
                        )));
                    }
                }
            }

            ShapeCase::Array { prefix, tail } => {
                for shape in prefix {
                    self.walk_union_helper(union_type, shape)?;
                }
                self.walk_union_helper(union_type, tail)?;
            }

            ShapeCase::One(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_union_helper(union_type, member_shape)?;
                }
            }

            ShapeCase::All(shapes) => {
                for member_shape in shapes.iter() {
                    self.walk_union_helper(union_type, member_shape)?;
                }
            }

            ShapeCase::None | ShapeCase::Null | ShapeCase::Unknown => {
                // None or Null might be fine if the union is nullable where
                // it's used, but we can't tell that from
                // UnionTypeDefinitionPosition alone.
                //
                // We include Unknown here because it could always turn out to
                // be something that works at runtime.
            }

            ShapeCase::Name(name, weak) => {
                if let Some(named_shape) = weak.upgrade(name) {
                    self.walk_union_helper(union_type, &named_shape)?;
                } else {
                    // Named shapes are like placeholders for future shapes that
                    // may be defined later, and since they might be valid
                    // later, we have nothing to warn about now.
                }
            }

            ShapeCase::Bool(_) | ShapeCase::String(_) | ShapeCase::Int(_) | ShapeCase::Float => {
                return Err(FederationError::internal(format!(
                    "Unexpected primitive shape provided for union type: {}",
                    shape.pretty_print()
                )));
            }

            ShapeCase::Error(shape::Error { partial, .. }) => {
                if let Some(partial) = partial {
                    // Errors with partial shapes still mostly behave like those
                    // shapes (except for simplification), so we need to
                    // validate the union against the partial shape.
                    self.walk_union_helper(union_type, partial)?;
                }
            }
        }

        Ok(())
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
