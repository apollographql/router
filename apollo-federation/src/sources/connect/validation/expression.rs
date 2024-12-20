use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use itertools::Itertools;
use shape::Shape;
use shape::ShapeCase;

use crate::sources::connect::string_template::Error;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::coordinates::ConnectDirectiveCoordinate;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::Namespace;

/// Details about the available variables and shapes for the current expression.
/// These should be consistent for all pieces of a connector in the request phase.
pub(super) struct Context<'schema> {
    pub(crate) schema: &'schema SchemaInfo<'schema>,
    var_lookup: IndexMap<Namespace, Shape>,
}

impl<'schema> Context<'schema> {
    pub(super) fn for_connect_request(
        schema: &'schema SchemaInfo,
        coordinate: ConnectDirectiveCoordinate,
    ) -> Self {
        let object_type = coordinate.field_coordinate.object;
        let is_root_type = schema
            .schema_definition
            .query
            .as_ref()
            .is_some_and(|query| query.name == object_type.name)
            || schema
                .schema_definition
                .mutation
                .as_ref()
                .is_some_and(|mutation| mutation.name == object_type.name);
        let mut var_lookup: IndexMap<Namespace, Shape> = [
            (
                Namespace::Args,
                Shape::record(
                    coordinate
                        .field_coordinate
                        .field
                        .arguments
                        .iter()
                        .map(|arg| (arg.name.to_string(), Shape::from(arg.ty.as_ref())))
                        .collect(),
                ),
            ),
            (Namespace::Config, Shape::none()),
            (Namespace::Context, Shape::none()),
        ]
        .into_iter()
        .collect();
        if !is_root_type {
            var_lookup.insert(Namespace::This, Shape::from(object_type.as_ref()));
        }

        Self { schema, var_lookup }
    }

    /// Create a context valid for expressions within the `@source` directive
    pub(super) fn for_source(schema: &'schema SchemaInfo) -> Self {
        let var_lookup: IndexMap<Namespace, Shape> = [
            (Namespace::Config, Shape::none()),
            (Namespace::Context, Shape::none()),
        ]
        .into_iter()
        .collect();
        Self { schema, var_lookup }
    }
}

/// Take a single expression and check that it's valid for the given context. This checks that
/// the expression can be executed given the known args and that the output shape is as expected.
///
/// TODO: this is only useful for URIs and headers right now, because of assumptions it makes.
pub(crate) fn validate(expression: &Expression, context: &Context) -> Result<(), Vec<Error>> {
    let Expression {
        expression,
        location,
    } = expression;
    let shape = expression.shape();
    let errors: Vec<Error> = shape
        .errors()
        .map(|err| Error {
            message: err.message.clone(),
            location: err
                .range
                .as_ref()
                .map(|range| range.start + location.start..range.end + location.start)
                .unwrap_or_else(|| location.clone()),
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors);
    }

    validate_shape(&shape, context).map_err(|message| {
        vec![Error {
            message,
            location: location.clone(),
        }]
    })
}

/// Validate that the shape is an acceptable output shape for an Expression.
///
/// TODO: Some day, whether objects or arrays are allowed will be dependent on &self (i.e., is the * modifier used)
fn validate_shape(shape: &Shape, context: &Context) -> Result<(), String> {
    match shape.case() {
        ShapeCase::Array { .. } => Err("array values aren't valid here".to_string()),
        ShapeCase::Object { .. } => Err("object values aren't valid here".to_string()),
        ShapeCase::One(shapes) | ShapeCase::All(shapes) => {
            for shape in shapes {
                validate_shape(shape, context)?;
            }
            Ok(())
        }
        ShapeCase::Name(name, key) => {
            let mut shape = if name == "$root" {
                return Err(format!(
                    "`{key}` must start with an argument name, like `$this` or `$args`",
                    key = key.iter().map(|key| key.to_string()).join(".")
                ));
            } else if name.starts_with('$') {
                let namespace = Namespace::from_str(name).map_err(|_| {
                    format!(
                        "unknown variable `{name}`, must be one of {namespaces}",
                        namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", ")
                    )
                })?;
                context
                    .var_lookup
                    .get(&namespace)
                    .ok_or_else(|| {
                        format!(
                            "{namespace} is not valid here, must be one of {namespaces}",
                            namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", "),
                        )
                    })?
                    .clone()
            } else {
                context
                    .schema
                    .shape_lookup
                    .get(name.as_str())
                    .cloned()
                    .ok_or_else(|| format!("unknown type `{name}`"))?
            };
            let mut path = name.clone();
            for key in key {
                let child = shape.child(key);
                if child.is_none() {
                    return Err(format!("`{path}` doesn't have a field named `{key}`"));
                }
                shape = child;
                path = format!("{path}.{key}");
            }
            validate_shape(&shape, context)
        }
        ShapeCase::Error(shape::Error { message, .. }) => Err(message.clone()),
        ShapeCase::None
        | ShapeCase::Bool(_)
        | ShapeCase::String(_)
        | ShapeCase::Int(_)
        | ShapeCase::Float
        | ShapeCase::Null => Ok(()), // We use null as any/unknown right now, so don't say anything about it
    }
}
