//! This module is all about validating [`Expression`]s for a given context. This isn't done at
//! runtime, _only_ during composition because it could be expensive.

use std::ops::Range;
use std::str::FromStr;
use std::sync::LazyLock;

use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::LineColumn;
use itertools::Itertools;
use shape::NamedShapePathKey;
use shape::Shape;
use shape::ShapeCase;
use shape::graphql::shape_for_arguments;
use shape::location::Location;
use shape::location::SourceId;

use crate::sources::connect::Namespace;
use crate::sources::connect::id::ConnectedElement;
use crate::sources::connect::id::ObjectCategory;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::coordinates::ConnectDirectiveCoordinate;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;

static REQUEST_SHAPE: LazyLock<Shape> = LazyLock::new(|| {
    Shape::record(
        [(
            "headers".to_string(),
            Shape::dict(Shape::list(Shape::string([]), []), []),
        )]
        .into(),
        [],
    )
});

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
        match coordinate.element {
            ConnectedElement::Field {
                parent_type,
                field_def,
                parent_category,
            } => {
                let mut var_lookup: IndexMap<Namespace, Shape> = [
                    (Namespace::Args, shape_for_arguments(field_def)),
                    (Namespace::Config, Shape::unknown([])),
                    (Namespace::Context, Shape::unknown([])),
                    (Namespace::Request, REQUEST_SHAPE.clone()),
                ]
                .into_iter()
                .collect();

                if matches!(parent_category, ObjectCategory::Other) {
                    var_lookup.insert(Namespace::This, Shape::from(parent_type));
                }

                Self {
                    schema,
                    var_lookup,
                    source,
                    code,
                }
            }
            ConnectedElement::Type { type_def } => {
                let var_lookup: IndexMap<Namespace, Shape> = [
                    (Namespace::This, Shape::from(type_def)),
                    (Namespace::Batch, Shape::list(Shape::from(type_def), [])),
                    (Namespace::Config, Shape::unknown([])),
                    (Namespace::Context, Shape::unknown([])),
                    (Namespace::Request, REQUEST_SHAPE.clone()),
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
    }

    /// Create a context valid for expressions within the `@source` directive
    pub(super) fn for_source(
        schema: &'schema SchemaInfo,
        source: &'schema GraphQLString,
        code: Code,
    ) -> Self {
        let var_lookup: IndexMap<Namespace, Shape> = [
            (Namespace::Config, Shape::unknown([])),
            (Namespace::Context, Shape::unknown([])),
            (Namespace::Request, REQUEST_SHAPE.clone()),
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

pub(crate) fn scalars() -> Shape {
    Shape::one(
        vec![
            Shape::int([]),
            Shape::float([]),
            Shape::bool(None),
            Shape::string(None),
            Shape::null([]),
            Shape::none(),
        ],
        [],
    )
}

/// Take a single expression and check that it's valid for the given context. This checks that
/// the expression can be executed given the known args and that the output shape is as expected.
pub(crate) fn validate(
    expression: &Expression,
    context: &Context,
    expected_shape: &Shape,
) -> Result<(), Message> {
    let shape = expression.expression.shape();

    let actual_shape = resolve_shape(&shape, context, expression)?;
    if let Some(mismatch) = expected_shape
        .validate(&actual_shape)
        .into_iter()
        // Unknown satisfies nothing, but we have to allow it for things like `$config`
        .find(|mismatch| !mismatch.received.is_unknown())
    {
        Err(Message {
            code: context.code,
            message: format!(
                "{} values aren't valid here",
                shape_name(&mismatch.received)
            ),
            locations: transform_locations(&mismatch.received.locations, context, expression),
        })
    } else {
        Ok(())
    }
}

/// Validate that the shape is an acceptable output shape for an Expression.
fn resolve_shape(
    shape: &Shape,
    context: &Context,
    expression: &Expression,
) -> Result<Shape, Message> {
    match shape.case() {
        ShapeCase::One(shapes) => {
            let mut inners = Vec::new();
            for inner in shapes {
                inners.push(resolve_shape(
                    &inner.with_locations(shape.locations.clone()),
                    context,
                    expression,
                )?);
            }
            Ok(Shape::one(inners, []))
        }
        ShapeCase::All(shapes) => {
            let mut inners = Vec::new();
            for inner in shapes {
                inners.push(resolve_shape(
                    &inner.with_locations(shape.locations.clone()),
                    context,
                    expression,
                )?);
            }
            Ok(Shape::all(inners, []))
        }
        ShapeCase::Name(name, key) => {
            let mut resolved = if name.value == "$root" {
                let mut key_str = key.iter().map(|key| key.to_string()).join(".");
                if !key_str.is_empty() {
                    key_str = format!("`{key_str}` ");
                }
                return Err(Message {
                    code: context.code,
                    message: format!(
                        "{key_str}must start with one of {namespaces}",
                        namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", "),
                    ),
                    locations: transform_locations(
                        key.first()
                            .map(|key| &key.locations)
                            .unwrap_or(&shape.locations),
                        context,
                        expression,
                    ),
                });
            } else if name.value.starts_with('$') {
                let namespace = Namespace::from_str(&name.value).map_err(|_| Message {
                    code: context.code,
                    message: format!(
                        "unknown variable `{name}`, must be one of {namespaces}",
                        namespaces = context.var_lookup.keys().map(|ns| ns.as_str()).join(", ")
                    ),
                    locations: transform_locations(&shape.locations, context, expression),
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
                        locations: transform_locations(&shape.locations, context, expression),
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
                        locations: transform_locations(&name.locations, context, expression),
                    })?
            };
            resolved.locations.extend(shape.locations.iter().cloned());
            let mut path = name.value.clone();
            for key in key {
                let child = resolved.child(key.clone());
                if child.is_none() {
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
                        locations: transform_locations(&key.locations, context, expression),
                    });
                }
                resolved = child;
                path = format!("{path}.{key}");
            }
            resolve_shape(&resolved, context, expression)
        }
        ShapeCase::Error(shape::Error { message, .. }) => Err(Message {
            code: context.code,
            message: message.clone(),
            locations: transform_locations(&shape.locations, context, expression),
        }),
        ShapeCase::Array { prefix, tail } => {
            let prefix = prefix
                .iter()
                .map(|shape| resolve_shape(shape, context, expression))
                .collect::<Result<Vec<_>, _>>()?;
            let tail = resolve_shape(tail, context, expression)?;
            Ok(Shape::array(prefix, tail, shape.locations.clone()))
        }
        ShapeCase::Object { fields, rest } => {
            let mut resolved_fields = Shape::empty_map();
            for (key, value) in fields {
                resolved_fields.insert(key.clone(), resolve_shape(value, context, expression)?);
            }
            let resolved_rest = resolve_shape(rest, context, expression)?;
            Ok(Shape::object(
                resolved_fields,
                resolved_rest,
                shape.locations.clone(),
            ))
        }
        ShapeCase::None
        | ShapeCase::Bool(_)
        | ShapeCase::String(_)
        | ShapeCase::Int(_)
        | ShapeCase::Float
        | ShapeCase::Null
        | ShapeCase::Unknown => Ok(shape.clone()),
    }
}

fn transform_locations<'a>(
    locations: impl IntoIterator<Item = &'a Location>,
    context: &Context,
    expression: &Expression,
) -> Vec<Range<LineColumn>> {
    let mut locations: Vec<_> = locations
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
                    location.span.start + expression.location.start
                        ..location.span.end + expression.location.start,
                    context.schema,
                )
            }
        })
        .collect();
    if locations.is_empty() {
        // Highlight the whole expression
        locations.extend(context.source.line_col_for_subslice(
            expression.location.start..expression.location.end,
            context.schema,
        ))
    }
    locations
}

/// A simplified shape name for error messages
fn shape_name(shape: &Shape) -> &'static str {
    match shape.case() {
        ShapeCase::Bool(_) => "boolean",
        ShapeCase::String(_) => "string",
        ShapeCase::Int(_) => "number",
        ShapeCase::Float => "number",
        ShapeCase::Null => "null",
        ShapeCase::Array { .. } => "array",
        ShapeCase::Object { .. } => "object",
        ShapeCase::One(_) => "union",
        ShapeCase::All(_) => "intersection",
        ShapeCase::Name(_, _) => "unknown",
        ShapeCase::Unknown => "unknown",
        ShapeCase::None => "none",
        ShapeCase::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use line_col::LineColLookup;
    use rstest::rstest;

    use super::*;
    use crate::sources::connect::JSONSelection;
    use crate::sources::connect::validation::link::ConnectLink;

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

    fn validate_with_context(selection: &str, expected: Shape) -> Result<(), Message> {
        let schema_str = schema_for(selection);
        let schema = Schema::parse(&schema_str, "schema").unwrap();
        let object = schema.get_object("Query").unwrap();
        let field = &object.fields["aField"];
        let directive = field.directives.get("connect").unwrap();
        let schema_info =
            SchemaInfo::new(&schema, &schema_str, ConnectLink::new(&schema).unwrap()?);
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
            element: ConnectedElement::Field {
                parent_type: object,
                field_def: field,
                parent_category: ObjectCategory::Query,
            },
            directive,
        };
        let context =
            Context::for_connect_request(&schema_info, coordinate, &expr_string, Code::InvalidUrl);
        validate(&expression(selection), &context, &expected)
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
    #[case::map_unknown("$config->map(@)->first")]
    #[case::map_scalar("$(1)->map(@)->last")]
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
    #[case("$args.int->map(@)->last")]
    #[case::chained_methods("$args.array->map(@)->slice(0,2)->first.bool")]
    #[case::match_scalars("$args.string->match([\"hello\", \"world\"], [@, null])")]
    #[case::slice("$args.string->slice(0, 2)")]
    #[case::size("$args.array->size")]
    #[case::first("$args.array->first.bool")]
    #[case::last("$args.array->last.bool")]
    #[case::multi_level_input("$args.multiLevel.inner.nested")]
    fn valid_expressions(#[case] selection: &str) {
        validate_with_context(selection, scalars()).unwrap();
    }

    #[rstest]
    #[case::array("$([])")]
    #[case::object("$({\"a\": 1})")]
    #[case::missing_property_of_object("$({\"a\": 1}).b")]
    #[case::missing_property_of_in_array("$([{\"a\": 1}]).b")]
    #[case::last("$([1, 2])")]
    #[case::unknown_var("$args.unknown")]
    #[case::arg_is_array("$args.array")]
    #[case::arg_is_object("$args.object")]
    #[case::unknown_field_on_object("$args.object.unknown")]
    #[case::this_on_query("$this.something")]
    #[case::bare_field_no_var("something")]
    // TODO: Re-enable these tests when method type checking is back
    // #[case::echo_invalid_constants("$->echo([])")]
    // #[case::map_scalar("$(1)->map(@)")]
    // #[case::map_array("$([])->map(@)")]
    // #[case::match_some_invalid_values("$config->match([1, 1], [2, {}])")]
    // #[case::slice_of_array("$([])->slice(0, 2)")]
    // #[case::entries("$config.something->entries")]
    // #[case::map_array("$args.array->map(@)")]
    // #[case::slice_array("$args.array->slice(0, 2)")]
    // #[case::entries_scalar("$args.int->entries")]
    // #[case::first("$args.array->first")]
    // #[case::last("$args.array->last")]
    fn invalid_expressions(#[case] selection: &str) {
        let err = validate_with_context(selection, scalars());
        assert!(err.is_err());
        assert!(
            !err.err().unwrap().locations.is_empty(),
            "Every error should have at least one location"
        );
    }

    #[test]
    fn bare_field_with_path() {
        let selection = "something.blah";
        let err =
            validate_with_context(selection, scalars()).expect_err("missing property is unknown");
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
        let err = validate_with_context(selection, scalars()).expect_err("objects are not allowed");
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
        let err =
            validate_with_context(selection, scalars()).expect_err("missing property is unknown");
        assert!(
            err.message.contains("`MultiLevel`"),
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

    #[test]
    fn unknown_var_in_scalar() {
        let selection = r#"$({"something": $blahblahblah})"#;
        let err = validate_with_context(selection, Shape::unknown([]))
            .expect_err("unknown variable is unknown");
        assert!(
            err.message.contains("`$blahblahblah`"),
            "{} didn't reference variable",
            err.message
        );
        assert!(
            err.locations
                .contains(&location_of_expression("$blahblahblah", selection)),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }

    #[test]
    fn subselection_of_literal_with_missing_field() {
        let selection = r#"$({"a": 1}) { b }"#;
        let err = validate_with_context(selection, Shape::unknown([]))
            .expect_err("invalid property is an error");
        assert!(
            err.message.contains("`b`"),
            "{} didn't reference variable",
            err.message
        );
        assert!(
            err.locations
                .contains(&location_of_expression("b", selection)),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }

    #[test]
    fn subselection_of_literal_in_array_with_missing_field() {
        let selection = r#"$([{"a": 1}]) { b }"#;
        let err = validate_with_context(selection, Shape::unknown([]))
            .expect_err("invalid property is an error");
        assert!(
            err.message.contains("`b`"),
            "{} didn't reference variable",
            err.message
        );
        assert!(
            err.locations
                .contains(&location_of_expression("b", selection)),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }
}
