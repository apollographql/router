//! Parsing and validation for `@connect(http:)`

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use shape::Shape;

use crate::sources::connect::JSONSelection;
use crate::sources::connect::URLTemplate;
use crate::sources::connect::spec::schema::CONNECT_BODY_ARGUMENT_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME;
use crate::sources::connect::spec::schema::HTTP_ARGUMENT_NAME;
use crate::sources::connect::string_template::Expression;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::connect::CONNECT_SOURCE_ARGUMENT_NAME;
use crate::sources::connect::validation::coordinates::ConnectDirectiveCoordinate;
use crate::sources::connect::validation::coordinates::ConnectHTTPCoordinate;
use crate::sources::connect::validation::coordinates::HttpHeadersCoordinate;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::coordinates::connect_directive_http_body_coordinate;
use crate::sources::connect::validation::expression;
use crate::sources::connect::validation::expression::Context;
use crate::sources::connect::validation::graphql::GraphQLString;
use crate::sources::connect::validation::graphql::SchemaInfo;
use crate::sources::connect::validation::http::headers;
use crate::sources::connect::validation::http::url::validate_template;
use crate::sources::connect::validation::source::SourceName;

pub(super) fn validate(
    coordinate: ConnectDirectiveCoordinate,
    source_name: Option<&SourceName>,
    schema: &SchemaInfo,
) -> Vec<Message> {
    let Some((http_arg, http_arg_node)) = coordinate
        .directive
        .specified_argument_by_name(&HTTP_ARGUMENT_NAME)
        .and_then(|arg| Some((arg.as_object()?, arg)))
    else {
        return vec![Message {
            code: Code::GraphQLError,
            message: format!("{coordinate} must have a `{HTTP_ARGUMENT_NAME}` argument."),
            locations: coordinate
                .directive
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        }];
    };

    let mut errors = Vec::new();

    if let Some((_, body)) = http_arg
        .iter()
        .find(|(name, _)| name == &CONNECT_BODY_ARGUMENT_NAME)
    {
        errors.extend(validate_body(coordinate, schema, body));
    }

    errors.extend(headers::validate_arg(
        http_arg,
        HttpHeadersCoordinate::Connect {
            connect: coordinate,
        },
        schema,
    ));

    errors.extend(
        validate_method(
            http_arg,
            ConnectHTTPCoordinate::from(coordinate),
            http_arg_node,
            source_name,
            schema,
        )
        .err()
        .into_iter()
        .flatten(),
    );

    errors
}

pub(super) fn validate_body(
    connect_coordinate: ConnectDirectiveCoordinate,
    schema: &SchemaInfo,
    selection_node: &Node<Value>,
) -> Vec<Message> {
    let coordinate = connect_directive_http_body_coordinate(&connect_coordinate);

    // Ensure that the body selection is a valid JSON selection string
    let selection_str = match GraphQLString::new(selection_node, &schema.sources) {
        Ok(selection_str) => selection_str,
        Err(_) => {
            return vec![Message {
                code: Code::GraphQLError,
                message: format!("{coordinate} must be a string."),
                locations: selection_node
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }];
        }
    };
    let selection = match JSONSelection::parse(selection_str.as_str()) {
        Ok(selection) => selection,
        Err(err) => {
            return vec![Message {
                code: Code::InvalidBody,
                message: format!("{coordinate} is not valid: {err}"),
                locations: selection_node
                    .line_column_range(&schema.sources)
                    .into_iter()
                    .collect(),
            }];
        }
    };
    if selection.is_empty() {
        return vec![Message {
            code: Code::InvalidBody,
            message: format!("{coordinate} is empty"),
            locations: selection_node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        }];
    }

    // Validate the selection shape
    if let Err(mut message) = expression::validate(
        &Expression {
            expression: selection,
            location: 0..selection_str.as_str().len(),
        },
        &Context::for_connect_request(
            schema,
            connect_coordinate,
            &selection_str,
            Code::InvalidBody,
        ),
        &Shape::unknown([]),
    ) {
        message.message = format!("In {coordinate}: {message}", message = message.message);
        return vec![message];
    }

    Vec::new()
}

fn validate_method<'schema>(
    http_arg: &'schema [(Name, Node<Value>)],
    coordinate: ConnectHTTPCoordinate<'schema>,
    http_arg_node: &Node<Value>,
    source_name: Option<&SourceName<'schema>>,
    schema: &SchemaInfo<'schema>,
) -> Result<URLTemplate, Vec<Message>> {
    let source_map = &schema.sources;
    let mut methods = http_arg
        .iter()
        .filter(|(method, _)| {
            [
                CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME,
                CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME,
            ]
            .contains(method)
        })
        .peekable();

    let Some((method_name, method_value)) = methods.next() else {
        return Err(vec![Message {
            code: Code::MissingHttpMethod,
            message: format!("{coordinate} must specify an HTTP method."),
            locations: http_arg_node
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        }]);
    };

    if methods.peek().is_some() {
        let locations = method_value
            .line_column_range(source_map)
            .into_iter()
            .chain(methods.filter_map(|(_, node)| node.line_column_range(source_map)))
            .collect();
        return Err(vec![Message {
            code: Code::MultipleHttpMethods,
            message: format!("{coordinate} cannot specify more than one HTTP method."),
            locations,
        }]);
    }

    let coordinate = HttpMethodCoordinate {
        connect: coordinate.connect_directive_coordinate,
        http_method: method_name,
        node: method_value,
    };

    let template = validate_template(coordinate, schema)?;

    if source_name.is_some() && template.base.is_some() {
        return Err(vec![Message {
            code: Code::AbsoluteConnectUrlWithSource,
            message: format!(
                "{coordinate} contains the absolute URL {raw_value} while also specifying a `{CONNECT_SOURCE_ARGUMENT_NAME}`. Either remove the `{CONNECT_SOURCE_ARGUMENT_NAME}` argument or change the URL to a path.",
                raw_value = coordinate.node
            ),
            locations: coordinate
                .node
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        }]);
    }
    if source_name.is_none() && template.base.is_none() {
        return Err(vec![Message {
            code: Code::RelativeConnectUrlWithoutSource,
            message: format!(
                "{coordinate} specifies the relative URL {raw_value}, but no `{CONNECT_SOURCE_ARGUMENT_NAME}` is defined. Either use an absolute URL including scheme (e.g. https://), or add a `@{source_directive_name}`.",
                raw_value = coordinate.node,
                source_directive_name = schema.source_directive_name(),
            ),
            locations: coordinate
                .node
                .line_column_range(source_map)
                .into_iter()
                .collect(),
        }]);
    }
    Ok(template)
}
