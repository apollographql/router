//! Variable validation.

use std::collections::HashMap;

use apollo_compiler::Node;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use itertools::Itertools;

use crate::connectors::id::ConnectedElement;
use crate::connectors::json_selection::SelectionTrie;
use crate::connectors::schema_type_ref::SchemaTypeRef;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::graphql::subslice_location;
use crate::connectors::variable::Namespace;
use crate::connectors::variable::VariableContext;
use crate::connectors::variable::VariableReference;

pub(crate) struct VariableResolver<'a> {
    context: VariableContext<'a>,
    schema: &'a SchemaInfo<'a>,
    resolvers: HashMap<Namespace, Box<dyn NamespaceResolver + 'a>>,
}

impl<'a> VariableResolver<'a> {
    pub(super) fn new(context: VariableContext<'a>, schema: &'a SchemaInfo<'a>) -> Self {
        let mut resolvers = HashMap::<Namespace, Box<dyn NamespaceResolver + 'a>>::new();

        match context.element {
            ConnectedElement::Field {
                parent_type,
                field_def,
                ..
            } => {
                resolvers.insert(
                    Namespace::This,
                    Box::new(ThisResolver::new(*parent_type, field_def)),
                );
                resolvers.insert(Namespace::Args, Box::new(ArgsResolver::new(field_def)));
            }
            ConnectedElement::Type { .. } => {} // TODO: $batch
        }

        Self {
            context,
            schema,
            resolvers,
        }
    }

    pub(super) fn resolve(
        &self,
        reference: &VariableReference<Namespace>,
        node: &Node<Value>,
    ) -> Result<(), Message> {
        if !self
            .context
            .available_namespaces()
            .contains(&reference.namespace.namespace)
        {
            return Err(Message {
                code: self.context.error_code(),
                message: format!(
                    "variable `{namespace}` is not valid at this location, must be one of {available}",
                    namespace = reference.namespace.namespace.as_str(),
                    available = self.context.namespaces_joined(),
                ),
                locations: reference
                    .namespace
                    .location
                    .iter()
                    .flat_map(|range| subslice_location(node, range.clone(), self.schema))
                    .collect(),
            });
        }
        if let Some(resolver) = self.resolvers.get(&reference.namespace.namespace) {
            resolver.check(reference, node, self.schema)?;
        }
        Ok(())
    }
}

/// Checks that the variables are valid within a specific namespace
pub(crate) trait NamespaceResolver {
    fn check(
        &self,
        reference: &VariableReference<Namespace>,
        node: &Node<Value>,
        schema: &SchemaInfo,
    ) -> Result<(), Message>;
}

pub(super) fn resolve_type<'schema>(
    schema: &'schema Schema,
    ty: &Type,
    field: &Component<FieldDefinition>,
) -> Result<&'schema ExtendedType, Message> {
    schema
        .types
        .get(ty.inner_named_type())
        .ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!("The type {ty} is referenced but not defined in the schema.",),
            locations: field
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        })
}

/// Resolve a variable reference path relative to a type. Assumes that the first element of the
/// path has already been resolved to the type, and validates any remainder.
fn resolve_path(
    schema: &SchemaInfo,
    path_selection: &SelectionTrie,
    node: &Node<Value>,
    field_type: &Type,
    field: &Component<FieldDefinition>,
) -> Result<(), Message> {
    let parent_is_nullable = !field_type.is_non_null();

    for (nested_field_name, sub_trie) in path_selection.iter() {
        let nested_field_type = resolve_type(schema, field_type, field)
            .and_then(|extended_type| {
                match extended_type {
                    ExtendedType::Enum(_) | ExtendedType::Scalar(_) => None,
                    ExtendedType::Object(object) => object.fields.get(nested_field_name).map(|field| &field.ty),
                    ExtendedType::InputObject(input_object) => input_object.fields.get(nested_field_name).map(|field| field.ty.as_ref()),
                    // TODO: at the time of writing, you can't declare interfaces or unions in connectors schemas at all, so these aren't tested
                    ExtendedType::Interface(interface) => interface.fields.get(nested_field_name).map(|field| &field.ty),
                    ExtendedType::Union(_) => {
                        return Err(Message {
                            code: Code::UnsupportedVariableType,
                            message: format!(
                                "The type {field_type} is a union, which is not supported in variables yet.",
                            ),
                            locations: field
                                .line_column_range(&schema.sources)
                                .into_iter()
                                .collect(),
                        })
                    },
                }
                    .ok_or_else(|| Message {
                        code: Code::UndefinedField,
                        message: format!(
                            "`{field_type}` does not have a field named `{nested_field_name}`."
                        ),
                        locations: path_selection
                            .key_ranges(nested_field_name)
                            .flat_map(|range| subslice_location(node, range, schema))
                            .collect(),
                    })
            })
            .map(|extended_type| {
                if parent_is_nullable && extended_type.is_non_null() {
                    // This .clone() might not be necessary if .nullable() did
                    // not take ownership of extended_type.
                    extended_type.clone().nullable()
                } else {
                    extended_type.clone()
                }
            })?;

        resolve_path(schema, sub_trie, node, &nested_field_type, field)?;
    }

    Ok(())
}

/// Resolves variables in the `$this` namespace
pub(crate) struct ThisResolver<'a> {
    object_type: SchemaTypeRef<'a>,
    field: &'a Component<FieldDefinition>,
}

impl<'a> ThisResolver<'a> {
    pub(crate) const fn new(
        object_type: SchemaTypeRef<'a>,
        field: &'a Component<FieldDefinition>,
    ) -> Self {
        Self { object_type, field }
    }
}

impl NamespaceResolver for ThisResolver<'_> {
    fn check(
        &self,
        reference: &VariableReference<Namespace>,
        node: &Node<Value>,
        schema: &SchemaInfo,
    ) -> Result<(), Message> {
        for (root, sub_trie) in reference.selection.iter() {
            let fields = self.object_type.get_fields(root);

            if fields.is_empty() {
                return Err(Message {
                    code: Code::UndefinedField,
                    message: format!(
                        "`{object}` does not have a field named `{root}`",
                        object = self.object_type.name(),
                    ),
                    locations: reference
                        .selection
                        .key_ranges(root)
                        .flat_map(|range| subslice_location(node, range, schema))
                        .collect(),
                });
            }

            for field_def in fields.values() {
                resolve_path(schema, sub_trie, node, &field_def.ty, self.field)?;
            }
        }

        Ok(())
    }
}

/// Resolves variables in the `$args` namespace
pub(crate) struct ArgsResolver<'a> {
    field: &'a Component<FieldDefinition>,
}

impl<'a> ArgsResolver<'a> {
    pub(crate) const fn new(field: &'a Component<FieldDefinition>) -> Self {
        Self { field }
    }
}

impl NamespaceResolver for ArgsResolver<'_> {
    fn check(
        &self,
        reference: &VariableReference<Namespace>,
        node: &Node<Value>,
        schema: &SchemaInfo,
    ) -> Result<(), Message> {
        for (root, sub_trie) in reference.selection.iter() {
            let field_type = self
                .field
                .arguments
                .iter()
                .find(|arg| arg.name == root)
                .map(|arg| arg.ty.clone())
                .ok_or_else(|| Message {
                    code: Code::UndefinedArgument,
                    message: format!(
                        "`{object}` does not have an argument named `{root}`",
                        object = self.field.name,
                    ),
                    locations: reference
                        .selection
                        .key_ranges(root)
                        .flat_map(|range| subslice_location(node, range, schema))
                        .collect(),
                })?;

            resolve_path(schema, sub_trie, node, &field_type, self.field)?;
        }

        Ok(())
    }
}
