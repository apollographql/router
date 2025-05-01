//! Variable validation.

use std::collections::HashMap;

use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::schema::ObjectType;
use itertools::Itertools;

use crate::sources::connect::id::ConnectedElement;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::VariableContext;
use crate::sources::connect::variable::VariablePathPart;
use crate::sources::connect::variable::VariableReference;

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
                    Box::new(ThisResolver::new(parent_type, field_def)),
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
        expression: GraphQLString,
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
                locations: expression
                    .line_col_for_subslice(
                        reference.namespace.location.start..reference.namespace.location.end,
                        self.schema,
                    )
                    .into_iter()
                    .collect(),
            });
        }
        if let Some(resolver) = self.resolvers.get(&reference.namespace.namespace) {
            resolver.check(reference, expression, self.schema)?;
        }
        Ok(())
    }
}

/// Checks that the variables are valid within a specific namespace
pub(crate) trait NamespaceResolver {
    fn check(
        &self,
        reference: &VariableReference<Namespace>,
        expression: GraphQLString,
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
    reference: &VariableReference<Namespace>,
    expression: GraphQLString,
    field_type: &Type,
    field: &Component<FieldDefinition>,
) -> Result<(), Message> {
    let mut variable_type = field_type.clone();
    for nested_field_name in reference.path.clone().iter().skip(1) {
        let path_component_range = nested_field_name.location.clone();
        let nested_field_name = nested_field_name.as_str();
        let parent_is_nullable = !field_type.is_non_null();
        variable_type = resolve_type(schema, &variable_type, field)
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
                            "`{variable_type}` does not have a field named `{nested_field_name}`."
                        ),
                        locations: expression.line_col_for_subslice(
                            path_component_range,
                            schema
                        ).into_iter().collect(),
                    })
            })?.clone();
        if parent_is_nullable && variable_type.is_non_null() {
            variable_type = variable_type.nullable();
        }
    }
    Ok(())
}

/// Require a variable reference to have a path
fn get_root<'a>(reference: &'a VariableReference<'a, Namespace>) -> Option<VariablePathPart<'a>> {
    reference.path.first().cloned()
}

/// Resolves variables in the `$this` namespace
pub(crate) struct ThisResolver<'a> {
    object: &'a ObjectType,
    field: &'a Component<FieldDefinition>,
}

impl<'a> ThisResolver<'a> {
    pub(crate) const fn new(object: &'a ObjectType, field: &'a Component<FieldDefinition>) -> Self {
        Self { object, field }
    }
}

impl NamespaceResolver for ThisResolver<'_> {
    fn check(
        &self,
        reference: &VariableReference<Namespace>,
        expression: GraphQLString,
        schema: &SchemaInfo,
    ) -> Result<(), Message> {
        let Some(root) = get_root(reference) else {
            return Ok(()); // Not something we can type check this way
        };

        let fields = &self.object.fields;

        let field_type = fields
            .get(root.as_str())
            .ok_or_else(|| Message {
                code: Code::UndefinedField,
                message: format!(
                    "`{object}` does not have a field named `{root}`",
                    object = self.object.name,
                    root = root.as_str(),
                ),
                locations: expression
                    .line_col_for_subslice(root.location.start..root.location.end, schema)
                    .into_iter()
                    .collect(),
            })
            .map(|field| field.ty.clone())?;

        resolve_path(schema, reference, expression, &field_type, self.field)
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
        expression: GraphQLString,
        schema: &SchemaInfo,
    ) -> Result<(), Message> {
        let Some(root) = get_root(reference) else {
            return Ok(()); // Not something we can type check this way TODO: delete all of this when Shape is available
        };

        let field_type = self
            .field
            .arguments
            .iter()
            .find(|arg| arg.name == root.as_str())
            .ok_or_else(|| Message {
                code: Code::UndefinedArgument,
                message: format!(
                    "`{object}` does not have an argument named `{root}`",
                    object = self.field.name,
                    root = root.as_str(),
                ),
                locations: expression
                    .line_col_for_subslice(root.location.start..root.location.end, schema)
                    .into_iter()
                    .collect(),
            })
            .map(|field| field.ty.clone())?;

        resolve_path(schema, reference, expression, &field_type, self.field)
    }
}
