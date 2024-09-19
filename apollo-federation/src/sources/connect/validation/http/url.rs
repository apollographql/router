use std::fmt::Display;
use std::str::FromStr;

use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value;
use apollo_compiler::schema::Component;
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use url::Url;

use crate::sources::connect::url_template;
use crate::sources::connect::url_template::VariableType;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::URLTemplate;
use crate::sources::connect::Variable;

pub(crate) fn validate_template(
    coordinate: HttpMethodCoordinate,
    schema: &SchemaInfo,
) -> Result<URLTemplate, Vec<Message>> {
    let (template, str_value) = match parse_template(coordinate, schema) {
        Ok(tuple) => tuple,
        Err(message) => return Err(vec![message]),
    };
    let mut messages = Vec::new();
    if let Some(base) = template.base.as_ref() {
        messages
            .extend(validate_base_url(base, coordinate, coordinate.node, str_value, schema).err());
    }

    for variable in template.path_variables() {
        match validate_variable(variable, str_value, coordinate, schema) {
            Err(err) => messages.push(err),
            Ok(Some(ty)) if !ty.is_non_null() => {
                messages.push(Message {
                    code: Code::NullablePathVariable,
                    message: format!(
                        "Variables in path parameters should be non-null, but {coordinate} contains `{{{variable}}}` which is nullable. \
                         If a null value is provided at runtime, the request will fail.",
                    ),
                    locations: str_value.line_col_for_subslice(
                        variable.location.clone(),
                        &schema.lookup,
                    ).into_iter().collect(),
                });
            }
            Ok(_) => {} // Type is non-null, or unknowable
        }
    }

    for variable in template.query_variables() {
        if let Err(err) = validate_variable(variable, str_value, coordinate, schema) {
            messages.push(err);
        }
    }

    if messages.is_empty() {
        Ok(template)
    } else {
        Err(messages)
    }
}

fn parse_template<'schema>(
    coordinate: HttpMethodCoordinate<'schema>,
    schema: &'schema SchemaInfo,
) -> Result<(URLTemplate, GraphQLString<'schema>), Message> {
    let str_value =
        GraphQLString::new(coordinate.node, &schema.sources).ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!("The value for {coordinate} must be a string."),
            locations: coordinate
                .node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        })?;
    let template = URLTemplate::from_str(str_value.as_str()).map_err(
        |url_template::Error { message, location }| Message {
            code: Code::InvalidUrl,
            message: format!("{coordinate} must be a valid URL template. {message}"),
            locations: location
                .and_then(|location| str_value.line_col_for_subslice(location, &schema.lookup))
                .into_iter()
                .collect(),
        },
    )?;
    Ok((template, str_value))
}

pub(crate) fn validate_base_url(
    url: &Url,
    coordinate: impl Display,
    value: &Node<Value>,
    str_value: GraphQLString,
    schema: &SchemaInfo,
) -> Result<(), Message> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        let scheme_location = 0..scheme.len();
        Err(Message {
            code: Code::InvalidUrlScheme,
            message: format!(
                "The value {value} for {coordinate} must be http or https, got {scheme}.",
            ),
            locations: str_value
                .line_col_for_subslice(scheme_location, &schema.lookup)
                .into_iter()
                .collect(),
        })
    } else {
        Ok(())
    }
}

fn validate_variable<'schema>(
    variable: &'schema Variable,
    url_value: GraphQLString,
    coordinate: HttpMethodCoordinate<'schema>,
    schema: &'schema SchemaInfo,
) -> Result<Option<Type>, Message> {
    let field_coordinate = coordinate.connect.field_coordinate;
    let field = field_coordinate.field;
    let mut path = variable.path.split('.');
    let path_root = path.next().unwrap_or(&variable.path);
    let mut path_component_start = variable.location.start + variable.var_type.as_str().len() + 1;
    let mut path_component_end = path_component_start + path_root.len();
    let mut variable_type = match variable.var_type {
        VariableType::Config => {
            return Ok(None); // We don't validate Router config yet
        }
        VariableType::Args => {
            field.arguments.iter().find(|arg| arg.name == path_root).ok_or_else( || {
                Message {
                    code: Code::UndefinedArgument,
                    message: format!(
                        "{coordinate} contains `{{{variable}}}`, but {field_coordinate} does not have an argument named `{path_root}`.",
                    ),
                    locations: url_value.line_col_for_subslice(
                        path_component_start..path_component_end,
                        &schema.lookup,
                    ).into_iter().collect()
                }
            }).map(|arg| arg.ty.as_ref().clone())?
        }
        VariableType::This => {
            field_coordinate.object.fields.get(path_root).ok_or_else(||Message {
                    code: Code::UndefinedField,
                    message: format!(
                        "{coordinate} contains `{{{variable}}}`, but {object} does not have a field named `{path_root}`.",
                        object = field_coordinate.object.name,
                    ),
                    locations: url_value.line_col_for_subslice(
                        path_component_start..path_component_end,
                        &schema.lookup,
                    ).into_iter().collect()
                }).map(|field| field.ty.clone())?
        }
    };

    for nested_field_name in path {
        path_component_start = path_component_end + 1; // Past the last component and its dot
        path_component_end = path_component_start + nested_field_name.len();
        let parent_is_nullable = !variable_type.is_non_null();
        variable_type = resolve_type(schema, &variable_type, field_coordinate.field)
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
                                "The type {variable_type} is a union, which is not supported in variables yet.",
                            ),
                            locations: field_coordinate
                                .field
                                .line_column_range(&schema.sources)
                                .into_iter()
                                .collect(),
                        })
                    },
                }
                    .ok_or_else(|| Message {
                        code: Code::UndefinedField,
                        message: format!(
                            "{coordinate} contains `{{{variable}}}`, but `{variable_type}` does not have a field named `{nested_field_name}`.",
                        ),
                        locations: url_value.line_col_for_subslice(
                            path_component_start..path_component_end,
                            &schema.lookup,
                        ).into_iter().collect(),
                    })
            })?.clone();
        if parent_is_nullable && variable_type.is_non_null() {
            variable_type = variable_type.nullable();
        }
    }

    Ok(Some(variable_type))
}

fn resolve_type<'schema>(
    schema: &'schema Schema,
    ty: &Type,
    definition: &Component<FieldDefinition>,
) -> Result<&'schema ExtendedType, Message> {
    schema
        .types
        .get(ty.inner_named_type())
        .ok_or_else(|| Message {
            code: Code::GraphQLError,
            message: format!("The type {ty} is referenced but not defined in the schema.",),
            locations: definition
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        })
}
