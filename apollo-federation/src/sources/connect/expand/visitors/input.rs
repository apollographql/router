use std::ops::Deref;

use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::Node;
use indexmap::IndexMap;

use super::filter_directives;
use super::try_insert;
use super::try_pre_insert;
use super::FieldVisitor;
use super::GroupVisitor;
use super::SchemaVisitor;
use crate::error::FederationError;
use crate::schema::position::InputObjectFieldDefinitionPosition;
use crate::schema::position::InputObjectTypeDefinitionPosition;
use crate::schema::position::TypeDefinitionPosition;

impl FieldVisitor<InputObjectFieldDefinitionPosition>
    for SchemaVisitor<'_, InputObjectTypeDefinitionPosition, InputObjectType>
{
    type Error = FederationError;

    fn visit<'a>(&mut self, field: InputObjectFieldDefinitionPosition) -> Result<(), Self::Error> {
        let (_, r#type) = self.type_stack.last_mut().ok_or(FederationError::internal(
            "tried to visit a field in a group not yet visited",
        ))?;

        // Extract the node info
        let field_def = field.get(self.original_schema.schema())?;

        // Add the input to the currently processing object, making sure to not overwrite if it already
        // exists (and verify that we didn't change the type)
        let new_field = InputValueDefinition {
            description: field_def.description.clone(),
            name: field_def.name.clone(),
            default_value: field_def.default_value.clone(),
            ty: field_def.ty.clone(),
            directives: filter_directives(self.directive_deny_list, &field_def.directives),
        };

        let input_type = self
            .original_schema
            .get_type(field_def.ty.inner_named_type().clone())?;
        match input_type {
            TypeDefinitionPosition::Scalar(pos) => {
                try_pre_insert!(self.to_schema, pos)?;
                try_insert!(
                    self.to_schema,
                    pos,
                    pos.get(self.original_schema.schema())?.clone()
                )?;
            }
            TypeDefinitionPosition::Enum(pos) => {
                try_pre_insert!(self.to_schema, pos)?;
                try_insert!(
                    self.to_schema,
                    pos,
                    pos.get(self.original_schema.schema())?.clone()
                )?;
            }
            _ => {}
        }

        if let Some(old_field) = r#type.fields.get(&field.field_name) {
            if *old_field.deref().deref() != new_field {
                return Err(FederationError::internal(
                   format!( "tried to write field to existing type, but field type was different. expected {new_field:?} found {old_field:?}"),
                ));
            }
        } else {
            r#type
                .fields
                .insert(field.field_name, Component::new(new_field));
        }

        Ok(())
    }
}

impl GroupVisitor<InputObjectTypeDefinitionPosition, InputObjectFieldDefinitionPosition>
    for SchemaVisitor<'_, InputObjectTypeDefinitionPosition, InputObjectType>
{
    fn try_get_group_for_field(
        &self,
        field: &InputObjectFieldDefinitionPosition,
    ) -> Result<Option<InputObjectTypeDefinitionPosition>, FederationError> {
        // Return the next group, if found
        let field_type = field.get(self.original_schema.schema())?;
        let inner_type = self
            .original_schema
            .get_type(field_type.ty.inner_named_type().clone())?;
        match inner_type {
            TypeDefinitionPosition::InputObject(input) => Ok(Some(input)),
            TypeDefinitionPosition::Scalar(_) | TypeDefinitionPosition::Enum(_) => Ok(None),

            other => Err(FederationError::internal(format!(
                "input objects cannot include fields of type: {}",
                other.type_name()
            ))),
        }
    }

    fn enter_group<'a>(
        &mut self,
        group: &InputObjectTypeDefinitionPosition,
    ) -> Result<Vec<InputObjectFieldDefinitionPosition>, FederationError> {
        try_pre_insert!(self.to_schema, group)?;

        let group_def = group.get(self.original_schema.schema())?;
        let output_type = InputObjectType {
            description: group_def.description.clone(),
            name: group_def.name.clone(),
            directives: filter_directives(self.directive_deny_list, &group_def.directives),
            fields: IndexMap::with_hasher(Default::default()), // Filled in by the rest of the visitor
        };

        self.type_stack.push((group.clone(), output_type));
        let def = group.get(self.original_schema.schema())?;
        Ok(def.fields.keys().cloned().map(|f| group.field(f)).collect())
    }

    fn exit_group(&mut self) -> Result<(), FederationError> {
        let (definition, r#type) = self.type_stack.pop().ok_or(FederationError::internal(
            "tried to exit a group not yet visited",
        ))?;

        // Now actually consolidate the object into our schema
        try_insert!(self.to_schema, definition, Node::new(r#type))
    }
}
