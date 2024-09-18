use apollo_compiler::ast::Value;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;

use super::url::validate_template;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_DELETE_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_GET_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PATCH_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_POST_METHOD_NAME;
use crate::sources::connect::spec::schema::CONNECT_HTTP_ARGUMENT_PUT_METHOD_NAME;
use crate::sources::connect::validation::coordinates::ConnectHTTPCoordinate;
use crate::sources::connect::validation::coordinates::HttpMethodCoordinate;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::URLTemplate;

pub(crate) fn validate<'schema>(
    http_arg: &'schema [(Name, Node<Value>)],
    coordinate: ConnectHTTPCoordinate<'schema>,
    http_arg_node: &Node<Value>,
    schema: &Schema,
) -> Result<(URLTemplate, HttpMethodCoordinate<'schema>), Vec<Message>> {
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

    validate_template(coordinate, schema).map(|template| (template, coordinate))
}
