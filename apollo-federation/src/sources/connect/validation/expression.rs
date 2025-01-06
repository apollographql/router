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
    var_lookup: IndexMap<Namespace, Option<Shape>>,
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
        let mut var_lookup: IndexMap<Namespace, Option<Shape>> = [
            (
                Namespace::Args,
                Some(Shape::record(
                    coordinate
                        .field_coordinate
                        .field
                        .arguments
                        .iter()
                        .map(|arg| (arg.name.to_string(), Shape::from(arg.ty.as_ref())))
                        .collect(),
                )),
            ),
            (Namespace::Config, None),
            (Namespace::Context, None),
        ]
        .into_iter()
        .collect();
        if !is_root_type {
            var_lookup.insert(Namespace::This, Some(Shape::from(object_type.as_ref())));
        }

        Self { schema, var_lookup }
    }

    /// Create a context valid for expressions within the `@source` directive
    pub(super) fn for_source(schema: &'schema SchemaInfo) -> Self {
        let var_lookup: IndexMap<Namespace, Option<Shape>> =
            [(Namespace::Config, None), (Namespace::Context, None)]
                .into_iter()
                .collect();
        Self { schema, var_lookup }
    }
}

/// Take a single expression and check that it's valid for the given context. This checks that
/// the expression can be executed given the known args and that the output shape is as expected.
///
/// TODO: this is only useful for URIs and headers right now, because it assumes objects/arrays are invalid.
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
                let Some(var_shape) = context
                    .var_lookup
                    .get(&namespace)
                    .ok_or_else(|| {
                        format!(
                            "{namespace} is not valid here, must be one of {namespaces}",
                            namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", "),
                        )
                    })?
                    .clone()
                else {
                    return Ok(()); // We don't know the shape of this var, so we can't validate it
                };
                var_shape
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

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::Schema;
    use rstest::rstest;

    use super::*;
    use crate::sources::connect::validation::coordinates::FieldCoordinate;
    use crate::sources::connect::JSONSelection;

    fn expression(selection: &str) -> Expression {
        Expression {
            expression: JSONSelection::parse(selection).unwrap(),
            location: 0..0,
        }
    }

    const SCHEMA: &str = r#"
        extend schema
          @link(
            url: "https://specs.apollo.dev/connect/v0.1"
            import: ["@connect", "@source"]
          )
          @source(name: "v2", http: { baseURL: "http://127.0.0.1" })
        
        type Query {
          aField(
            int: Int
            string: String
            customScalar: CustomScalar
            object: InputObject
            array: [InputObject]
          ): AnObject  @connect(source: "v2")
          something: String
        }
        
        scalar CustomScalar
        
        input InputObject {
          bool: Boolean
        }
        
        type AnObject {
          bool: Boolean
        }
    "#;

    #[rstest]
    #[case::int("$(1)")]
    #[case::float("$(1.0)")]
    #[case::bool("$(true)")]
    #[case::string("$(\"hello\")")]
    #[case::null("$(null)")]
    fn allowed_literals(#[case] selection: &str) {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let context = Context::for_source(&schema_info);
        validate(&expression(selection), &context).unwrap();
    }

    #[rstest]
    #[case::array("$([])")]
    #[case::object("$({\"a\": 1})")]
    fn disallowed_literals(#[case] selection: &str) {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let context = Context::for_source(&schema_info);
        assert!(validate(&expression(selection), &context).is_err());
    }

    #[rstest]
    #[case::echo_valid_constants("$->echo(1)")]
    #[case::map_unknown("$config->map(@)")]
    #[case::map_scalar("$(1)->map(@)")]
    #[case::match_only_valid_values("$config->match([1, 1], [2, true])")]
    #[case::first("$([1, 2])->first")]
    #[case::first_type_unknown("$config.something->first")]
    #[case::last("$([1, 2])->last")]
    #[case::last_type_unknown("$config.something->last")]
    #[case::slice_of_string("$(\"hello\")->slice(0, 2)")]
    #[case::slice_when_type_unknown("$config.something->slice(0, 2)")]
    #[case::size_when_type_unknown("$config.something->size")]
    #[case::size_of_array("$([])->size")]
    #[case::size_of_entries("$config->entries->size")]
    #[case::size_of_slice("$([1, 2, 3])->slice(0, 2)->size")]
    #[case::slice_after_match("$config->match([1, \"something\"], [2, \"another\"])->slice(0, 2)")]
    fn valid_methods(#[case] selection: &str) {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let context = Context::for_source(&schema_info);
        validate(&expression(selection), &context).unwrap();
    }

    #[rstest]
    #[case::echo_invalid_constants("$->echo([])")]
    #[case::map_array("$([])->map(@)")]
    #[case::match_some_invalid_values("$config->match([1, 1], [2, {}])")]
    #[case::slice_of_array("$([])->slice(0, 2)")]
    #[case::entries("$config.something->entries")]
    fn invalid_methods(#[case] selection: &str) {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let context = Context::for_source(&schema_info);
        assert!(validate(&expression(selection), &context).is_err());
    }

    #[rstest]
    #[case("$args.int")]
    #[case("$args.string")]
    #[case("$args.customScalar")]
    #[case("$args.object.bool")]
    #[case("$args.array->echo(1)")]
    #[case("$args.int->map(@)")]
    #[case::chained_methods("$args.array->map(@)->slice(0,2)->first.bool")]
    #[case::match_scalars("$args.string->match([\"hello\", \"world\"], [@, null])")]
    #[case::slice("$args.string->slice(0, 2)")]
    #[case::size("$args.array->size")]
    fn valid_args(#[case] selection: &str) {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let object = schema.get_object("Query").unwrap();
        let field = object.fields.get("aField").unwrap();
        let directive = field.directives.get("connect").unwrap();
        let coordinate = ConnectDirectiveCoordinate {
            field_coordinate: FieldCoordinate { field, object },
            directive,
        };
        let context = Context::for_connect_request(&schema_info, coordinate);
        validate(&expression(selection), &context).unwrap();
    }

    #[rstest]
    #[case::unknown_var("$args.unknown")]
    #[case::arg_is_array("$args.array")]
    #[case::arg_is_object("$args.object")]
    #[case::unknown_field_on_object("$args.object.unknown")]
    #[case::map_array("$args.array->map(@)")]
    #[case::slice_array("$args.array->slice(0, 2)")]
    #[case::entries_scalar("$args.int->entries")]
    fn invalid_args(#[case] selection: &str) {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let object = schema.get_object("Query").unwrap();
        let field = object.fields.get("aField").unwrap();
        let directive = field.directives.get("connect").unwrap();
        let coordinate = ConnectDirectiveCoordinate {
            field_coordinate: FieldCoordinate { field, object },
            directive,
        };
        let context = Context::for_connect_request(&schema_info, coordinate);
        assert!(validate(&expression(selection), &context).is_err());
    }

    #[test]
    fn this_on_query() {
        let schema = Schema::parse(SCHEMA, "schema").unwrap();
        let connect = name!("connect");
        let source = name!("source");
        let schema_info = SchemaInfo::new(&schema, "", &connect, &source);
        let object = schema.get_object("Query").unwrap();
        let field = object.fields.get("aField").unwrap();
        let directive = field.directives.get("connect").unwrap();
        let coordinate = ConnectDirectiveCoordinate {
            field_coordinate: FieldCoordinate { field, object },
            directive,
        };
        let context = Context::for_connect_request(&schema_info, coordinate);
        assert!(validate(&expression("$this.something"), &context).is_err());
    }
}
