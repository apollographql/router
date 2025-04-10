use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::parser::SourceMap;
use itertools::Itertools;

use crate::sources::connect::JSONSelection;
use crate::sources::connect::validation::Code;
use crate::sources::connect::validation::Message;
use crate::sources::connect::validation::SchemaInfo;

pub(crate) struct UrlProperties {
    method: Option<JSONSelection>,
    scheme: Option<JSONSelection>,
    host: Option<JSONSelection>,
    port: Option<JSONSelection>,
    user: Option<JSONSelection>,
    password: Option<JSONSelection>,
    path: Option<JSONSelection>,
    query: Option<JSONSelection>,
}

impl<'schema> UrlProperties {
    pub(crate) fn parse(
        coordinate: String, // source or connect
        schema: &'schema SchemaInfo<'schema>,
        http_arg: &'schema [(Name, Node<Value>)],
    ) -> Result<Self, Message> {
        let mut method = None;
        let mut scheme = None;
        let mut host = None;
        let mut port = None;
        let mut user = None;
        let mut password = None;
        let mut path = None;
        let mut query = None;

        fn parse_http_arg(
            node: &Node<Value>,
            name: &str,
            errors: &mut Vec<Message>,
            source_map: &SourceMap,
        ) -> Option<JSONSelection> {
            let Some(value) = node.as_str() else {
                errors.push(Message {
                    code: Code::InvalidUrlProperty,
                    message: format!("The `{name}` argument must be a string."),
                    locations: node.line_column_range(&source_map).into_iter().collect(),
                });
                return None;
            };
            match JSONSelection::parse(value) {
                Ok(selection) => Some(selection),
                Err(e) => {
                    errors.push(Message {
                        code: Code::InvalidUrlProperty,
                        message: format!("The `{name}` argument is invalid: {e}"),
                        locations: node.line_column_range(&source_map).into_iter().collect(),
                    });
                    None
                }
            }
        }

        let mut errors = Vec::new();
        let source_map = &schema.sources;

        for (name, value) in http_arg {
            match name.as_str() {
                "method" => method = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "scheme" => scheme = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "host" => host = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "port" => port = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "user" => user = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "password" => {
                    password = parse_http_arg(value, name.as_str(), &mut errors, source_map)
                }
                "path" => path = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                "query" => query = parse_http_arg(value, name.as_str(), &mut errors, source_map),
                _ => {}
            }
        }

        if !errors.is_empty() {
            // TODO return all error individually
            return Err(Message {
                code: Code::InvalidUrlProperty,
                message: format!(
                    "{coordinate} has invalid syntax in URL properties.\n{}",
                    errors.iter().map(|e| e.message.clone()).join("\n")
                ),
                locations: errors.into_iter().flat_map(|err| err.locations).collect(),
            });
        }

        Ok(Self {
            method,
            scheme,
            host,
            port,
            user,
            password,
            path,
            query,
        })
    }

    pub(crate) fn type_check(&self) -> Vec<Message> {
        vec![]
    }
}
