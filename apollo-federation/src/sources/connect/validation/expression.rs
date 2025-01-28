//! This module is all about validating [`Expression`]s for a given context. This isn't done at
//! runtime, _only_ during composition because it could be expensive.

use std::ops::Range;
use std::str::FromStr;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::LineColumn;
use itertools::Itertools;
use shape::graphql::shape_for_arguments;
use shape::location::Location;
use shape::location::SourceId;
use shape::NamedShapePathKey;
use shape::Shape;
use shape::ShapeCase;

use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::coordinates::ConnectDirectiveCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::Namespace;

/// Details about the available variables and shapes for the current expression.
/// These should be consistent for all pieces of a connector in the request phase.
pub(super) struct Context<'schema> {
    pub(crate) schema: &'schema SchemaInfo<'schema>,
    var_lookup: IndexMap<Namespace, Shape>,
    source: &'schema GraphQLString<'schema>,
    /// The code that all resulting messages will use
    /// TODO: make code dynamic based on coordinate so new validations can be warnings
    code: Code,
}

impl<'schema> Context<'schema> {
    /// Create a context valid for expressions within the URI or headers of a `@connect` directive
    pub(super) fn for_connect_request(
        schema: &'schema SchemaInfo,
        coordinate: ConnectDirectiveCoordinate,
        source: &'schema GraphQLString,
        code: Code,
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
                shape_for_arguments(&coordinate.field_coordinate.field),
            ),
            (Namespace::Config, Shape::unknown(Vec::new())),
            (Namespace::Context, Shape::unknown(Vec::new())),
        ]
        .into_iter()
        .collect();
        if !is_root_type {
            var_lookup.insert(Namespace::This, Shape::from(object_type));
        }

        Self {
            schema,
            var_lookup,
            source,
            code,
        }
    }

    /// Create a context valid for expressions within the `@source` directive
    pub(super) fn for_source(
        schema: &'schema SchemaInfo,
        source: &'schema GraphQLString,
        code: Code,
    ) -> Self {
        let var_lookup: IndexMap<Namespace, Shape> = [
            (Namespace::Config, Shape::unknown(Vec::new())),
            (Namespace::Context, Shape::unknown(Vec::new())),
        ]
        .into_iter()
        .collect();
        Self {
            schema,
            var_lookup,
            source,
            code,
        }
    }
}

/// Take a single expression and check that it's valid for the given context. This checks that
/// the expression can be executed given the known args and that the output shape is as expected.
///
/// TODO: this is only useful for URIs and headers right now, because it assumes objects/arrays are invalid.
pub(crate) fn validate(expression: &Expression, context: &Context) -> Result<(), Message> {
    let Expression {
        expression,
        location, // TODO: use this to get the location in the whole schema document
    } = expression;
    let shape = expression.shape();

    validate_shape(&shape, context, location.start)
}

/// Validate that the shape is an acceptable output shape for an Expression.
///
/// TODO: Some day, whether objects or arrays are allowed will be dependent on &self (i.e., is the * modifier used)
fn validate_shape(
    shape: &Shape,
    context: &Context,
    expression_offset: usize,
) -> Result<(), Message> {
    match shape.case() {
        ShapeCase::Array { .. } => Err(Message {
            code: context.code,
            locations: transform_locations(&shape.locations, context, expression_offset),
            message: format!(
                "{} is an array, which isn't valid here",
                expression_label(shape)
            ),
        }),
        ShapeCase::Object { .. } => Err(Message {
            code: context.code,
            locations: transform_locations(&shape.locations, context, expression_offset),
            message: format!(
                "{} is an object, which isn't valid here",
                expression_label(shape)
            ),
        }),
        ShapeCase::One(shapes) => {
            for inner in shapes {
                validate_shape(
                    &inner.with_locations(shape.locations.clone()),
                    context,
                    expression_offset,
                )?;
            }
            Ok(())
        }
        ShapeCase::All(shapes) => {
            for inner in shapes {
                validate_shape(
                    &inner.with_locations(shape.locations.clone()),
                    context,
                    expression_offset,
                )?;
            }
            Ok(())
        }
        ShapeCase::Name(name, key) => {
            let mut resolved = if name.value == "$root" {
                return Err(Message {
                    code: context.code,
                    message: format!(
                        "`{key}` must start with one of {namespaces}",
                        key = key.iter().map(|key| key.to_string()).join("."),
                        namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", "),
                    ),
                    locations: transform_locations(
                        key.first().iter().flat_map(|key| &key.locations),
                        context,
                        expression_offset,
                    ),
                });
            } else if name.value.starts_with('$') {
                let namespace = Namespace::from_str(&name.value).map_err(|_| Message {
                    code: context.code,
                    message: format!(
                        "unknown variable `{name}`, must be one of {namespaces}",
                        namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", ")
                    ),
                    locations: transform_locations(&shape.locations, context, expression_offset),
                })?;
                context
                    .var_lookup
                    .get(&namespace)
                    .ok_or_else(|| Message {
                        code: context.code,
                        message: format!(
                            "{namespace} is not valid here, must be one of {namespaces}",
                            namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", "),
                        ),
                        locations: transform_locations(
                            &shape.locations,
                            context,
                            expression_offset,
                        ),
                    })?
                    .clone()
            } else {
                context
                    .schema
                    .shape_lookup
                    .get(name.value.as_str())
                    .cloned()
                    .ok_or_else(|| Message {
                        code: context.code,
                        message: format!("unknown type `{name}`"),
                        locations: transform_locations(&name.locations, context, expression_offset),
                    })?
            };
            resolved.locations.extend(shape.locations.iter().cloned());
            for key in key {
                let child = resolved.child(key.clone());
                if child.is_none() {
                    let path = expression_label(&resolved);
                    let message = match key.value {
                        NamedShapePathKey::AnyIndex | NamedShapePathKey::Index(_) => {
                            format!("`{path}` is not an array or string")
                        }

                        NamedShapePathKey::AnyField | NamedShapePathKey::Field(_) => {
                            format!("`{path}` doesn't have a field named `{key}`")
                        }
                    };
                    return Err(Message {
                        code: context.code,
                        message,
                        locations: transform_locations(&key.locations, context, expression_offset),
                    });
                }
                resolved = child;
            }
            validate_shape(&resolved, context, expression_offset)
        }
        ShapeCase::Error(shape::Error { message, .. }) => Err(Message {
            code: context.code,
            message: message.clone(),
            locations: transform_locations(&shape.locations, context, expression_offset),
        }),
        ShapeCase::None
        | ShapeCase::Bool(_)
        | ShapeCase::String(_)
        | ShapeCase::Int(_)
        | ShapeCase::Float
        | ShapeCase::Null
        | ShapeCase::Unknown => Ok(()),
    }
}

fn expression_label(resolved: &Shape) -> String {
    resolved
        .locations
        .iter()
        .find(|location| matches!(location.source_id, SourceId::Other(_)))
        .map(|location| location.label.clone())
        .unwrap_or_default()
}

fn transform_locations<'a>(
    locations: impl IntoIterator<Item = &'a Location>,
    context: &Context,
    expression_offset: usize,
) -> Vec<Range<LineColumn>> {
    locations
        .into_iter()
        .filter_map(|location| match &location.source_id {
            SourceId::GraphQL(file_id) => context
                .schema
                .sources
                .get(file_id)
                .and_then(|source| source.get_line_column_range(location.span.clone())),
            SourceId::Other(_) => {
                // Right now, this always refers to the JSONSelection location
                context.source.line_col_for_subslice(
                    location.span.start + expression_offset..location.span.end + expression_offset,
                    context.schema,
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use apollo_compiler::name;
    use apollo_compiler::Schema;
    use line_col::LineColLookup;
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
            multiLevel: MultiLevelInput
          ): AnObject  @connect(source: "v2", http: {GET: """{EXPRESSION}"""})
          something: String
        }
        
        scalar CustomScalar
        
        input InputObject {
          bool: Boolean
        }
        
        type AnObject {
          bool: Boolean
        }
        
        input MultiLevelInput {
            inner: MultiLevel
        }
        
        type MultiLevel {
            nested: String
        }
    "#;

    fn schema_for(selection: &str) -> String {
        SCHEMA.replace("EXPRESSION", selection)
    }

    fn validate_with_context(selection: &str) -> Result<(), Message> {
        let schema_str = schema_for(selection);
        let schema = Schema::parse(&schema_str, "schema").unwrap();
        let object = schema.get_object("Query").unwrap();
        let field = &object.fields["aField"];
        let directive = field.directives.get("connect").unwrap();
        let source_directive = name!("source");
        let schema_info = SchemaInfo::new(&schema, &schema_str, &directive.name, &source_directive);
        let expr_string = GraphQLString::new(
            &directive
                .argument_by_name("http", &schema)
                .unwrap()
                .as_object()
                .unwrap()
                .first()
                .unwrap()
                .1,
            &schema.sources,
        )
        .unwrap();
        let coordinate = ConnectDirectiveCoordinate {
            field_coordinate: FieldCoordinate { field, object },
            directive,
        };
        let context =
            Context::for_connect_request(&schema_info, coordinate, &expr_string, Code::InvalidUrl);
        validate(&expression(selection), &context)
    }

    /// Given a full expression replaced in `{EXPRESSION}` above, find the line/col of a substring.
    fn location_of_expression(part: &str, full_expression: &str) -> Range<LineColumn> {
        let schema = schema_for(full_expression);
        let line_col_lookup = LineColLookup::new(&schema);
        let expression_offset = schema.find(full_expression).unwrap() - 1;
        let start_offset = expression_offset + full_expression.find(part).unwrap();
        let (start_line, start_col) = line_col_lookup.get(start_offset);
        let (end_line, end_col) = line_col_lookup.get(start_offset + part.len());
        LineColumn {
            line: start_line,
            column: start_col,
        }..LineColumn {
            line: end_line,
            column: end_col,
        }
    }

    #[rstest]
    #[case::int("$(1)")]
    #[case::float("$(1.0)")]
    #[case::bool("$(true)")]
    #[case::string("$(\"hello\")")]
    #[case::null("$(null)")]
    #[case::property_of_object("$({\"a\": 1}).a")]
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
    #[case::first("$args.array->first.bool")]
    #[case::last("$args.array->last.bool")]
    #[case::multi_level_input("$args.multiLevel.inner.nested")]
    fn valid_expressions(#[case] selection: &str) {
        validate_with_context(selection).unwrap();
    }

    #[rstest]
    #[case::array("$([])")]
    #[case::object("$({\"a\": 1})")]
    // #[case::missing_property_of_object("$({\"a\": 1}).b")]  // TODO: catch this error
    #[case::echo_invalid_constants("$->echo([])")]
    #[case::map_array("$([])->map(@)")]
    #[case::match_some_invalid_values("$config->match([1, 1], [2, {}])")]
    #[case::slice_of_array("$([])->slice(0, 2)")]
    #[case::entries("$config.something->entries")]
    #[case::unknown_var("$args.unknown")]
    #[case::arg_is_array("$args.array")]
    #[case::arg_is_object("$args.object")]
    #[case::unknown_field_on_object("$args.object.unknown")]
    // #[case::map_array("$args.array->map(@)")]  // TODO: check for this error once we improve ->map type checking
    #[case::slice_array("$args.array->slice(0, 2)")]
    #[case::entries_scalar("$args.int->entries")]
    #[case::first("$args.array->first")]
    #[case::last("$args.array->last")]
    #[case::this_on_query("$this.something")]
    #[case::bare_field_no_var("something")]
    fn invalid_expressions(#[case] selection: &str) {
        let err = validate_with_context(selection);
        assert!(err.is_err());
        assert!(
            !err.err().unwrap().locations.is_empty(),
            "Every error should have at least one location"
        );
    }

    #[test]
    fn bare_field_with_path() {
        let selection = "something.blah";
        let err = validate_with_context(selection)
            .err()
            .expect("missing property is unknown");
        let expected_location = location_of_expression("something", selection);
        assert!(
            err.message.contains("`something.blah`"),
            "{} didn't reference missing arg",
            err.message
        );
        assert!(
            err.message.contains("$args"),
            "{} didn't provide suggested variables",
            err.message
        );
        assert!(
            err.locations.contains(&expected_location),
            "The expected location {:?} wasn't included in {:?}",
            expected_location,
            err.locations
        );
    }

    #[test]
    fn object_in_url() {
        let selection = "$args.object";
        let err = validate_with_context(selection)
            .err()
            .expect("objects are not allowed");
        let expected_location = location_of_expression("object", selection);
        assert!(
            err.locations.contains(&expected_location),
            "The expected location {:?} wasn't included in {:?}",
            expected_location,
            err.locations
        );
    }

    #[test]
    fn nested_unknown_property() {
        let selection = "$args.multiLevel.inner.unknown";
        let err = validate_with_context(selection)
            .err()
            .expect("missing property is unknown");
        assert!(
            err.message.contains("`$args.multiLevel.inner`"),
            "{} didn't reference type",
            err.message
        );
        assert!(
            err.message.contains("`unknown`"),
            "{} didn't reference field name",
            err.message
        );
        assert!(
            err.locations
                .contains(&location_of_expression("unknown", selection)),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }
}
