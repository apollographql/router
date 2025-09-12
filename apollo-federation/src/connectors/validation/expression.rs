//! This module is all about validating [`Expression`]s for a given context. This isn't done at
//! runtime, _only_ during composition because it could be expensive.

use std::fmt::Display;
use std::ops::Range;
use std::str::FromStr;
use std::sync::LazyLock;

use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::parser::LineColumn;
use itertools::Itertools;
use shape::NamedShapePathKey;
use shape::Shape;
use shape::ShapeCase;
use shape::graphql::shape_for_arguments;
use shape::location::Location;
use shape::location::SourceId;

use crate::connectors::JSONSelection;
use crate::connectors::Namespace;
use crate::connectors::id::ConnectedElement;
use crate::connectors::id::ObjectCategory;
use crate::connectors::string_template::Expression;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::coordinates::ConnectDirectiveCoordinate;
use crate::connectors::validation::graphql::SchemaInfo;
use crate::connectors::validation::graphql::subslice_location;
use crate::connectors::variable::VariableReference;

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

static RESPONSE_SHAPE: LazyLock<Shape> = LazyLock::new(|| {
    Shape::record(
        [(
            "headers".to_string(),
            Shape::dict(Shape::list(Shape::string([]), []), []),
        )]
        .into(),
        [],
    )
});

fn env_shape() -> Shape {
    Shape::dict(Shape::string([]), [])
}

/// Details about the available variables and shapes for the current expression.
/// These should be consistent for all pieces of a connector in the request phase.
pub(super) struct Context<'schema> {
    pub(crate) schema: &'schema SchemaInfo<'schema>,
    var_lookup: IndexMap<Namespace, Shape>,
    node: &'schema Node<Value>,
    /// The code that all resulting messages will use
    /// TODO: make code dynamic based on coordinate so new validations can be warnings
    code: Code,
    /// Used to determine if `$root` is available (aka: we're mapping a response, not a request)
    has_response_body: bool,
}

impl<'schema> Context<'schema> {
    /// Create a context valid for expressions within the URI or headers of a `@connect` directive
    pub(super) fn for_connect_request(
        schema: &'schema SchemaInfo,
        coordinate: ConnectDirectiveCoordinate,
        node: &'schema Node<Value>,
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
                    // TODO Should these be Dict<Unknown> instead of Unknown?
                    (Namespace::Config, Shape::unknown([])),
                    (Namespace::Context, Shape::unknown([])),
                    (Namespace::Request, REQUEST_SHAPE.clone()),
                    (Namespace::Env, env_shape()),
                ]
                .into_iter()
                .collect();

                if matches!(parent_category, ObjectCategory::Other) {
                    var_lookup.insert(Namespace::This, Shape::from(parent_type));
                }

                Self {
                    schema,
                    var_lookup,
                    node,
                    code,
                    has_response_body: false,
                }
            }
            ConnectedElement::Type { type_def } => {
                let var_lookup: IndexMap<Namespace, Shape> = [
                    (Namespace::This, Shape::from(type_def)),
                    (Namespace::Batch, Shape::list(Shape::from(type_def), [])),
                    (Namespace::Config, Shape::unknown([])),
                    (Namespace::Context, Shape::unknown([])),
                    (Namespace::Request, REQUEST_SHAPE.clone()),
                    (Namespace::Env, env_shape()),
                ]
                .into_iter()
                .collect();

                Self {
                    schema,
                    var_lookup,
                    node,
                    code,
                    has_response_body: false,
                }
            }
        }
    }

    /// Create a context valid for expressions within the errors.message or errors.extension of the `@connect` directive
    /// TODO: We might be able to re-use this for the "selection" field later down the road
    pub(super) fn for_connect_response(
        schema: &'schema SchemaInfo,
        coordinate: ConnectDirectiveCoordinate,
        node: &'schema Node<Value>,
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
                    (Namespace::Status, Shape::int([])),
                    (Namespace::Request, REQUEST_SHAPE.clone()),
                    (Namespace::Response, RESPONSE_SHAPE.clone()),
                    (Namespace::Env, env_shape()),
                ]
                .into_iter()
                .collect();

                if matches!(parent_category, ObjectCategory::Other) {
                    var_lookup.insert(Namespace::This, Shape::from(parent_type));
                }

                Self {
                    schema,
                    var_lookup,
                    node,
                    code,
                    has_response_body: true,
                }
            }
            ConnectedElement::Type { type_def } => {
                let var_lookup: IndexMap<Namespace, Shape> = [
                    (Namespace::This, Shape::from(type_def)),
                    (Namespace::Batch, Shape::list(Shape::from(type_def), [])),
                    (Namespace::Config, Shape::unknown([])),
                    (Namespace::Context, Shape::unknown([])),
                    (Namespace::Status, Shape::int([])),
                    (Namespace::Request, REQUEST_SHAPE.clone()),
                    (Namespace::Response, RESPONSE_SHAPE.clone()),
                    (Namespace::Env, env_shape()),
                ]
                .into_iter()
                .collect();

                Self {
                    schema,
                    var_lookup,
                    node,
                    code,
                    has_response_body: true,
                }
            }
        }
    }

    /// Create a context valid for expressions within the `@source` directive
    pub(super) fn for_source(
        schema: &'schema SchemaInfo,
        node: &'schema Node<Value>,
        code: Code,
    ) -> Self {
        let var_lookup: IndexMap<Namespace, Shape> = [
            (Namespace::Config, Shape::unknown([])),
            (Namespace::Context, Shape::unknown([])),
            (Namespace::Request, REQUEST_SHAPE.clone()),
            (Namespace::Env, env_shape()),
        ]
        .into_iter()
        .collect();
        Self {
            schema,
            var_lookup,
            node,
            code,
            has_response_body: false,
        }
    }

    /// Create a context valid for expressions within the errors.message or errors.extension of the `@source` directive
    /// Note that we can't use stuff like "this" here cause we have no idea what the "type" is when on a @source block
    pub(super) fn for_source_response(
        schema: &'schema SchemaInfo,
        node: &'schema Node<Value>,
        code: Code,
    ) -> Self {
        let var_lookup: IndexMap<Namespace, Shape> = [
            (Namespace::Config, Shape::unknown([])),
            (Namespace::Context, Shape::unknown([])),
            (Namespace::Status, Shape::int([])),
            (Namespace::Request, REQUEST_SHAPE.clone()),
            (Namespace::Response, RESPONSE_SHAPE.clone()),
            (Namespace::Env, env_shape()),
        ]
        .into_iter()
        .collect();

        Self {
            schema,
            var_lookup,
            node,
            code,
            has_response_body: true,
        }
    }

    /// Create a context valid for expressions within the `baseURL` property of the `@source` directive
    pub(super) fn for_source_url(
        schema: &'schema SchemaInfo,
        node: &'schema Node<Value>,
        code: Code,
    ) -> Self {
        let var_lookup: IndexMap<Namespace, Shape> = [
            (Namespace::Config, Shape::unknown([])),
            (Namespace::Env, env_shape()),
        ]
        .into_iter()
        .collect();

        Self {
            schema,
            var_lookup,
            node,
            code,
            has_response_body: false,
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
    // TODO: this check should be done in the shape checking, but currently
    // shape resolution can drop references to inputs if the expressions ends with
    // a method, i.e. `$batch.id->joinNotNull(',')` — this resolves to simply
    // `Unknown`, so variables are dropped and cannot be checked.
    for variable_ref in expression.expression.variable_references() {
        let namespace = variable_ref.namespace.namespace;
        if !context.var_lookup.contains_key(&namespace) {
            let message = if namespace == Namespace::Batch {
                "`$batch` may only be used when `@connect` is applied to a type.".to_string()
            } else {
                format!(
                    "{} is not valid here, must be one of {}",
                    namespace,
                    context.var_lookup.keys().map(|ns| ns.as_str()).join(", ")
                )
            };
            return Err(Message {
                code: context.code,
                message,
                locations: variable_ref
                    .location
                    .iter()
                    .filter_map(|location| {
                        subslice_location(
                            context.node,
                            location.start + expression.location.start
                                ..location.end + expression.location.start,
                            context.schema,
                        )
                    })
                    .collect(),
            });
        }
    }

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
                // For response mapping, $root (aka the response body) is allowed so we will exit out early here
                // However, $root is not allowed for requests so we will error below
                if context.has_response_body {
                    return Ok(Shape::unknown([]));
                }

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
                subslice_location(
                    context.node,
                    location.span.start + expression.location.start
                        ..location.span.end + expression.location.start,
                    context.schema,
                )
            }
        })
        .collect();
    if locations.is_empty() {
        // Highlight the whole expression
        locations.extend(subslice_location(
            context.node,
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

pub(crate) struct MappingArgument {
    pub(crate) expression: Expression,
    pub(crate) node: Node<Value>,
}

impl MappingArgument {
    pub(crate) fn variable_references(&self) -> impl Iterator<Item = VariableReference<Namespace>> {
        self.expression.expression.variable_references()
    }
}

pub(crate) fn parse_mapping_argument(
    node: &Node<Value>,
    coordinate: impl Display,
    code: Code,
    schema: &SchemaInfo,
) -> Result<MappingArgument, Message> {
    let Some(string) = node.as_str() else {
        return Err(Message {
            code: Code::GraphQLError,
            message: format!("{coordinate} must be a string."),
            locations: node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    };

    let selection = match JSONSelection::parse_with_spec(string, schema.connect_link.spec) {
        Ok(selection) => selection,
        Err(e) => {
            return Err(Message {
                code,
                message: format!("{coordinate} is not valid: {e}"),
                locations: subslice_location(node, e.offset..e.offset + 1, schema)
                    .into_iter()
                    .collect(),
            });
        }
    };

    if selection.is_empty() {
        return Err(Message {
            code,
            message: format!("{coordinate} is empty"),
            locations: node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        });
    }

    Ok(MappingArgument {
        expression: Expression {
            expression: selection,
            location: 0..string.len(),
        },
        node: node.clone(),
    })
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use line_col::LineColLookup;
    use rstest::rstest;

    use super::*;
    use crate::connectors::ConnectSpec;
    use crate::connectors::JSONSelection;
    use crate::connectors::validation::ConnectLink;

    fn expression(selection: &str, spec: ConnectSpec) -> Expression {
        Expression {
            expression: JSONSelection::parse_with_spec(selection, spec).unwrap(),
            location: 0..0,
        }
    }

    const SCHEMA: &str = r#"
        extend schema
          @link(
            url: "https://specs.apollo.dev/connect/CONNECT_SPEC_VERSION",
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

    fn schema_for(selection: &str, spec: ConnectSpec) -> String {
        let version_string = format!("v{}", spec.as_str());
        SCHEMA
            .replace("CONNECT_SPEC_VERSION", version_string.as_str())
            .replace("EXPRESSION", selection)
    }

    fn validate_with_context(
        selection: &str,
        expected: Shape,
        spec: ConnectSpec,
    ) -> Result<(), Message> {
        let schema_str = schema_for(selection, spec);
        let schema = Schema::parse(&schema_str, "schema").unwrap();
        let object = schema.get_object("Query").unwrap();
        let field = &object.fields["aField"];
        let directive = field.directives.get("connect").unwrap();
        let schema_info =
            SchemaInfo::new(&schema, &schema_str, ConnectLink::new(&schema).unwrap()?);
        debug_assert_eq!(schema_info.connect_link.spec, spec);
        let expr_string = directive
            .argument_by_name("http", &schema)
            .unwrap()
            .as_object()
            .unwrap()
            .first()
            .unwrap()
            .1
            .clone();
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
        validate(&expression(selection, spec), &context, &expected)
    }

    /// Given a full expression replaced in `{EXPRESSION}` above, find the line/col of a substring.
    fn location_of_expression(
        part: &str,
        full_expression: &str,
        spec: ConnectSpec,
    ) -> Range<LineColumn> {
        let schema = schema_for(full_expression, spec);
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
    #[case::entries_when_type_unknown("$config.something->entries->first.value")]
    #[case::methods_with_unknown_input(r#"$config->get("something")->slice(0, 1)"#)]
    fn valid_expressions(#[case] selection: &str) {
        // If this fails, another ConnectSpec version has probably been added,
        // and should be accounted for in the loop below.
        assert_eq!(ConnectSpec::next(), ConnectSpec::V0_3);

        for spec in [ConnectSpec::V0_1, ConnectSpec::V0_2, ConnectSpec::V0_3] {
            validate_with_context(selection, scalars(), spec).unwrap();
        }
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
    fn common_invalid_expressions(#[case] selection: &str) {
        // If this fails, another ConnectSpec version has probably been added,
        // and should be accounted for in the loop below.
        assert_eq!(ConnectSpec::next(), ConnectSpec::V0_3);

        for spec in [ConnectSpec::V0_1, ConnectSpec::V0_2, ConnectSpec::V0_3] {
            let err = validate_with_context(selection, scalars(), spec);
            assert!(err.is_err());
            assert!(
                !err.err().unwrap().locations.is_empty(),
                "Every error should have at least one location"
            );
        }
    }

    #[rstest]
    // These cases require method shape checking, which was enabled in v0.3:
    #[case::echo_invalid_constants("$->echo([])")]
    #[case::map_scalar("$(1)->map(@)")]
    #[case::map_array("$([])->map(@)")]
    #[case::match_some_invalid_values("$config->match([1, 1], [2, {}])")]
    #[case::slice_of_array("$([])->slice(0, 2)")]
    #[case::entries("$config.something->entries")]
    #[case::map_array("$args.array->map(@)")]
    #[case::slice_array("$args.array->slice(0, 2)")]
    #[case::entries_scalar("$args.int->entries")]
    #[case::first("$args.array->first")]
    #[case::last("$args.array->last")]
    fn invalid_expressions_with_method_shape_checking(#[case] selection: &str) {
        // If this fails, another ConnectSpec version has probably been added,
        // and should probably be tested here in addition to v0.3.
        assert_eq!(ConnectSpec::next(), ConnectSpec::V0_3);

        let spec = ConnectSpec::V0_3;
        let err = validate_with_context(selection, scalars(), spec);
        assert!(err.is_err());
        assert!(
            !err.err().unwrap().locations.is_empty(),
            "Every error should have at least one location"
        );
    }

    #[rstest]
    #[case::args_object_as_echo_bool("$args.object->as($o)->echo($o.bool)")]
    #[case::args_object_as_echo_bool("$args.object->as($o)->echo(@.bool->eq($o.bool))")]
    #[case::args_object_as_echo_bool(
        "$->as($true, false->not)->as($false, true->not)->echo($true->or($false))"
    )]
    #[case::method_math("$([1, 2, 3])->as($arr)->first->add($arr->last)")]
    #[case::redundant_as("$args.object->as($o)->as($o)->echo($o.bool)")]
    #[case::unnecessary_as("$args.object->as($obj)->as($o)->echo($o.bool)")]
    #[case::as_int_addition("$args.int->as($i)->add(1, $i, $i)")]
    #[case::as_string_concat(
        "$args.string->as($s, @->slice(0, 100))->echo({ full: @, first100: $s })->jsonStringify"
    )]
    fn valid_as_var_bindings(#[case] selection: &str) {
        let spec = ConnectSpec::V0_3;
        validate_with_context(selection, scalars(), spec).unwrap();
    }

    #[rstest]
    #[case::args_object_as_echo_bool_var_mismatch("$args.object->as($obj)->echo($o.bool)")]
    #[case::args_object_as_echo_missing_string("$args.object->as($o)->echo($o.string)")]
    #[case::args_object_as_echo_missing_int("$args.object->as($o)->echo($o.int)")]
    #[case::as_without_args("$args.object->as")]
    #[case::as_with_no_args("$args.object->as()")]
    #[case::as_with_non_variable("$args.object->as(true)")]
    #[case::as_with_wrong_args("$args.object->as(1, 2, 3)")]
    #[case::as_with_reused_var("$([1, 2, 3])->as($o, $o)->echo($o)")]
    fn invalid_expressions_with_as_var_binding(#[case] selection: &str) {
        let spec = ConnectSpec::V0_3;
        let err = validate_with_context(selection, scalars(), spec);
        assert!(err.is_err());
        assert!(
            !err.err().unwrap().locations.is_empty(),
            "Every error should have at least one location"
        );
    }

    #[test]
    fn coalescing() {
        let spec = ConnectSpec::V0_3;
        validate_with_context(
            r#"$($args.string ?? "unknown error")"#,
            Shape::string([]),
            spec,
        )
        .expect("coalescing type checks in expressions");
    }

    #[test]
    fn bare_field_with_path() {
        let selection = "something.blah";
        let err = validate_with_context(selection, scalars(), ConnectSpec::latest())
            .expect_err("missing property is unknown");
        let expected_location =
            location_of_expression("something", selection, ConnectSpec::latest());
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
        let err = validate_with_context(selection, scalars(), ConnectSpec::latest())
            .expect_err("objects are not allowed");
        let expected_location = location_of_expression("object", selection, ConnectSpec::latest());
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
        let err = validate_with_context(selection, scalars(), ConnectSpec::latest())
            .expect_err("missing property is unknown");
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
            err.locations.contains(&location_of_expression(
                "unknown",
                selection,
                ConnectSpec::latest()
            )),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }

    #[test]
    fn unknown_var_in_scalar() {
        let selection = r#"$({"something": $blahblahblah})"#;
        let err = validate_with_context(selection, Shape::unknown([]), ConnectSpec::latest())
            .expect_err("unknown variable is unknown");
        assert!(
            err.message.contains("`$blahblahblah`"),
            "{} didn't reference variable",
            err.message
        );
        assert!(
            err.locations.contains(&location_of_expression(
                "$blahblahblah",
                selection,
                ConnectSpec::latest()
            )),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }

    #[test]
    fn subselection_of_literal_with_missing_field() {
        let selection = r#"$({"a": 1}) { b }"#;
        let err = validate_with_context(selection, Shape::unknown([]), ConnectSpec::latest())
            .expect_err("invalid property is an error");
        assert!(
            err.message.contains("`b`"),
            "{} didn't reference variable",
            err.message
        );
        assert!(
            err.locations.contains(&location_of_expression(
                "b",
                selection,
                ConnectSpec::latest()
            )),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }

    #[test]
    fn subselection_of_literal_in_array_with_missing_field() {
        let selection = r#"$([{"a": 1}]) { b }"#;
        let err = validate_with_context(selection, Shape::unknown([]), ConnectSpec::latest())
            .expect_err("invalid property is an error");
        assert!(
            err.message.contains("`b`"),
            "{} didn't reference variable",
            err.message
        );
        assert!(
            err.locations.contains(&location_of_expression(
                "b",
                selection,
                ConnectSpec::latest()
            )),
            "The relevant piece of the expression wasn't included in {:?}",
            err.locations
        );
    }
}
