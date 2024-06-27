use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Name;
use apollo_compiler::name;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::DirectiveList;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use apollo_compiler::Node;
use indexmap::IndexMap;
use indexmap::IndexSet;

use super::filter_directives;
use crate::error::FederationError;
use crate::schema::position::TypeDefinitionPosition;
use crate::schema::FederationSchema;
use crate::schema::ValidFederationSchema;
use crate::sources::connect::json_selection::JSONSelectionVisitor;
use crate::sources::connect::Connector;

/// A JSONSelection visitor for schema building.
///
/// This implementation of the JSONSelection visitor walks a JSONSelection,
/// copying over all output types (and respective fields / sub types) as it goes
/// from a reference schema.
pub(super) struct ToSchemaVisitor<'a> {
    /// List of directives to not copy over into the target schema.
    directive_deny_list: &'a IndexSet<&'a Name>,

    /// The original schema used for sourcing all types / fields / directives / etc.
    original_schema: &'a ValidFederationSchema,

    /// The target schema for adding all types.
    to_schema: &'a mut FederationSchema,

    /// A stack of parent types used for fetching subtypes
    ///
    /// Each entry corresponds to a nested subselect in the JSONSelection.
    type_stack: Vec<(TypeDefinitionPosition, ExtendedType)>,
}

impl<'a> ToSchemaVisitor<'a> {
    pub fn new(
        connector: &'a Connector,
        original_schema: &'a ValidFederationSchema,
        to_schema: &'a mut FederationSchema,
        initial_position: TypeDefinitionPosition,
        initial_type: &ExtendedType,
        directive_deny_list: &'a IndexSet<&'a Name>,
    ) -> ToSchemaVisitor<'a> {
        let mut output_type_directive_deny_list = directive_deny_list.clone();

        // never put a key on root connectors that don't provide entity resolvers
        let federation_key = name!(federation__key);
        if connector.on_root_type && !connector.entity {
            output_type_directive_deny_list.insert(&federation_key);
        }

        // TODO: construct the correct key for connector.entity == true using input parameters/selections

        // Get the type information for the initial position, making sure to strip
        // off any unwanted directives.
        let initial_type = match initial_type {
            ExtendedType::Object(object) => ExtendedType::Object(Node::new(ObjectType {
                description: object.description.clone(),
                name: object.name.clone(),
                implements_interfaces: object.implements_interfaces.clone(),
                directives: filter_directives(&output_type_directive_deny_list, &object.directives),
                fields: IndexMap::new(), // Will be filled in by subsequent visits
            })),
            ExtendedType::Scalar(_) => todo!(),
            ExtendedType::Interface(_) => todo!(),
            ExtendedType::Union(_) => todo!(),
            ExtendedType::Enum(_) => todo!(),
            ExtendedType::InputObject(_) => todo!(),
        };

        ToSchemaVisitor {
            directive_deny_list,
            original_schema,
            to_schema,
            type_stack: vec![(initial_position, initial_type)],
        }
    }
}

impl JSONSelectionVisitor for ToSchemaVisitor<'_> {
    fn visit(&mut self, name: &str) -> Result<(), FederationError> {
        let (definition, type_) = self.type_stack.last_mut().unwrap();

        // Extract the node info
        match (definition, type_) {
            // Objects have fields
            (TypeDefinitionPosition::Object(object), ExtendedType::Object(object_type)) => {
                let field_name = Name::new(name)?;
                let field = object
                    .field(field_name.clone())
                    .get(self.original_schema.schema())?;

                // Add it to the currently processing object
                object_type.make_mut().fields.insert(
                    field_name,
                    Component::new(FieldDefinition {
                        description: field.description.clone(),
                        name: field.name.clone(),
                        arguments: field.arguments.clone(),
                        ty: field.ty.clone(),
                        directives: filter_directives(self.directive_deny_list, &field.directives),
                    }),
                );
            }

            (TypeDefinitionPosition::Scalar(_), ExtendedType::Scalar(_)) => todo!(),
            (TypeDefinitionPosition::Interface(_), ExtendedType::Interface(_)) => todo!(),
            (TypeDefinitionPosition::Union(_), ExtendedType::Union(_)) => todo!(),
            (TypeDefinitionPosition::Enum(_), ExtendedType::Enum(_)) => todo!(),
            (TypeDefinitionPosition::InputObject(_), ExtendedType::InputObject(_)) => todo!(),

            (_, _) => unreachable!("type definition did not match type"),
        };

        Ok(())
    }

    fn enter_group(&mut self, group: &str) -> Result<(), FederationError> {
        let (definition, _) = self.type_stack.last().unwrap();

        // Helper for making sure that the selected group is of a type that allows sub selections
        let mut extract_sub_type = |type_: &TypeDefinitionPosition| {
            match type_ {
                TypeDefinitionPosition::Object(object) => {
                    object.pre_insert(self.to_schema)?;
                    let def = object.get(self.original_schema.schema())?;

                    Ok(ExtendedType::Object(Node::new(ObjectType {
                        description: def.description.clone(),
                        name: def.name.clone(),
                        implements_interfaces: def.implements_interfaces.clone(),
                        directives: DirectiveList::new(), // TODO: Whitelist
                        fields: IndexMap::new(), // Will be filled in by the `visit` method for each field
                    })))
                }
                TypeDefinitionPosition::Interface(_) => todo!(),
                TypeDefinitionPosition::Union(_) => todo!(),
                TypeDefinitionPosition::InputObject(_) => todo!(),

                TypeDefinitionPosition::Enum(_) => {
                    Err(FederationError::internal("enums cannot have subselections"))
                }
                TypeDefinitionPosition::Scalar(_) => Err(FederationError::internal(
                    "scalars cannot have subselections",
                )),
            }
        };

        // Attempt to get the sub type by the field name specified
        let (next, sub_type) = match definition {
            // Objects have fields
            TypeDefinitionPosition::Object(object) => {
                let field = object
                    .field(Name::new(group)?)
                    .get(self.original_schema.schema())?;
                let next = self
                    .original_schema
                    .get_type(field.ty.inner_named_type().clone())?;

                // Extract the extended type info for the output type
                let output_type = extract_sub_type(&next)?;
                (next, output_type)
            }

            TypeDefinitionPosition::Interface(_) => todo!(),
            TypeDefinitionPosition::Union(_) => todo!(),
            TypeDefinitionPosition::InputObject(_) => todo!(),

            TypeDefinitionPosition::Enum(_) => {
                return Err(FederationError::internal("cannot enter an enum"))
            }
            TypeDefinitionPosition::Scalar(_) => {
                return Err(FederationError::internal("cannot enter a scalar"))
            }
        };

        self.type_stack.push((next, sub_type));

        Ok(())
    }

    fn exit_group(&mut self) -> Result<(), FederationError> {
        let (definition, type_) = self.type_stack.pop().unwrap();

        // Now actually consolidate the object into our schema
        match (definition, type_) {
            (TypeDefinitionPosition::Object(object), ExtendedType::Object(object_type)) => {
                object.insert(self.to_schema, object_type)
            }
            (TypeDefinitionPosition::Interface(_), ExtendedType::Interface(_)) => todo!(),
            (TypeDefinitionPosition::Union(_), ExtendedType::Union(_)) => todo!(),
            (TypeDefinitionPosition::InputObject(_), ExtendedType::InputObject(_)) => todo!(),

            (TypeDefinitionPosition::Enum(_), ExtendedType::Enum(_)) => {
                unreachable!("enums should not have been entered")
            }
            (TypeDefinitionPosition::Scalar(_), ExtendedType::Scalar(_)) => {
                unreachable!("scalars should not have been entered")
            }
            (_, _) => unreachable!("type definition did not match type"),
        }
    }

    fn finish(mut self) -> Result<(), FederationError> {
        // Make sure to create the final object that we started with
        let (definition, type_) = self.type_stack.pop().unwrap();

        // Now actually consolidate the object into our schema
        match (definition, type_) {
            (TypeDefinitionPosition::Object(object), ExtendedType::Object(object_type)) => {
                object.pre_insert(self.to_schema)?;
                object.insert(self.to_schema, object_type)
            }
            (TypeDefinitionPosition::Interface(_), ExtendedType::Interface(_)) => todo!(),
            (TypeDefinitionPosition::Union(_), ExtendedType::Union(_)) => todo!(),
            (TypeDefinitionPosition::InputObject(_), ExtendedType::InputObject(_)) => todo!(),

            (TypeDefinitionPosition::Enum(_), ExtendedType::Enum(_)) => {
                unreachable!("enums should not have been entered")
            }
            (TypeDefinitionPosition::Scalar(_), ExtendedType::Scalar(_)) => {
                unreachable!("scalars should not have been entered")
            }
            (_, _) => unreachable!("type definition did not match type"),
        }
    }
}
