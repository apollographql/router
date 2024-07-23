use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::InputObjectType;
use apollo_compiler::Node;
use indexmap::IndexMap;

use super::filter_directives;
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
        let (_, type_) = self.type_stack.last_mut().unwrap();

        // Extract the node info
        let field_def = field.get(self.original_schema.schema())?;

        // Add it to the currently processing object
        type_.fields.insert(
            field.field_name,
            Component::new(InputValueDefinition {
                description: field_def.description.clone(),
                name: field_def.name.clone(),
                default_value: field_def.default_value.clone(),
                ty: field_def.ty.clone(),
                directives: filter_directives(self.directive_deny_list, &field_def.directives),
            }),
        );

        Ok(())
    }
}

impl GroupVisitor<InputObjectTypeDefinitionPosition, InputObjectFieldDefinitionPosition>
    for SchemaVisitor<'_, InputObjectTypeDefinitionPosition, InputObjectType>
{
    fn get_group_fields(
        &self,
        group: InputObjectTypeDefinitionPosition,
    ) -> Result<
        Vec<InputObjectFieldDefinitionPosition>,
        <Self as FieldVisitor<InputObjectFieldDefinitionPosition>>::Error,
    > {
        let def = group.get(self.original_schema.schema())?;
        Ok(def.fields.keys().cloned().map(|f| group.field(f)).collect())
    }

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
        group: InputObjectTypeDefinitionPosition,
    ) -> Result<Vec<InputObjectFieldDefinitionPosition>, FederationError> {
        group.pre_insert(self.to_schema)?;

        let group_def = group.get(self.original_schema.schema())?;
        let output_type = InputObjectType {
            description: group_def.description.clone(),
            name: group_def.name.clone(),
            directives: filter_directives(self.directive_deny_list, &group_def.directives),
            fields: IndexMap::with_hasher(Default::default()), // Filled in by the rest of the visitor
        };

        self.type_stack.push((group.clone(), output_type));
        self.get_group_fields(group)
    }

    fn exit_group(&mut self) -> Result<(), FederationError> {
        let (definition, type_) = self.type_stack.pop().unwrap();

        // Now actually consolidate the object into our schema
        definition.insert(self.to_schema, Node::new(type_))
    }
}
