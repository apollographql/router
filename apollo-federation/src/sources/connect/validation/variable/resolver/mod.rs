use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Schema;

use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::variable::Namespace;
use crate::sources::connect::variable::VariablePathPart;
use crate::sources::connect::variable::VariableReference;

pub(super) mod args;
pub(super) mod this;

/// Resolves variables with a specific namespace
pub(crate) trait NamespaceResolver {
    fn resolve(
        &self,
        reference: &VariableReference<Namespace>,
        expression: GraphQLString,
        schema: &SchemaInfo,
    ) -> Result<Option<Type>, Message>;
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
) -> Result<Option<Type>, Message> {
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
                        locations: expression.line_col_for_subslice(path_component_range.start..path_component_range.end, schema).into_iter().collect(),
                    })
            })?.clone();
        if parent_is_nullable && variable_type.is_non_null() {
            variable_type = variable_type.nullable();
        }
    }
    Ok(Some(variable_type))
}

/// Require a variable reference to have a path
fn get_root<'a>(
    reference: &'a VariableReference<'a, Namespace>,
    expression: GraphQLString<'a>,
    schema: &'a SchemaInfo<'a>,
) -> Result<VariablePathPart<'a>, Message> {
    reference.path.first().cloned().ok_or(Message {
        code: Code::GraphQLError,
        message: format!(
            "The variable `{}` must be followed by a path",
            reference.namespace.namespace
        ),
        locations: expression
            .line_col_for_subslice(reference.location.clone(), schema)
            .into_iter()
            .collect(),
    })
}
