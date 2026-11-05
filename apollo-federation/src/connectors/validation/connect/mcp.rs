//! Parsing and validation for `@connect(mcp:)`

use std::fmt::Display;

use shape::Shape;

use crate::connectors::ConnectSpec;
use crate::connectors::Namespace;
use crate::connectors::SourceName;
use crate::connectors::spec::http::HTTP_ARGUMENT_NAME;
use crate::connectors::spec::mcp::MCP_ARGUMENT_NAME;
use crate::connectors::spec::mcp::MCP_ARGUMENTS_ARGUMENT_NAME;
use crate::connectors::spec::mcp::MCP_TOOL_ARGUMENT_NAME;
use crate::connectors::validation::Code;
use crate::connectors::validation::Message;
use crate::connectors::validation::coordinates::ConnectDirectiveCoordinate;
use crate::connectors::validation::expression;
use crate::connectors::validation::expression::Context;
use crate::connectors::validation::expression::MappingArgument;
use crate::connectors::validation::expression::parse_mapping_argument;
use crate::connectors::validation::graphql::SchemaInfo;

/// A valid, parsed (but not type-checked) `@connect(mcp:)`.
pub(super) struct Mcp<'schema> {
    arguments: Option<Arguments<'schema>>,
}

impl<'schema> Mcp<'schema> {
    /// Parse `@connect(mcp:)`, if the connector has one.
    ///
    /// `source_name` is the connector's validated source, and `mcp_sources` are the sources that
    /// declare `@source(mcp:)`. The tool call goes to that source's MCP endpoint, so a connector
    /// must name one.
    pub(super) fn parse(
        connect: ConnectDirectiveCoordinate<'schema>,
        source_name: Option<&SourceName>,
        mcp_sources: &[SourceName],
        schema: &'schema SchemaInfo,
    ) -> Result<Option<Self>, Vec<Message>> {
        let Some(node) = connect
            .directive
            .specified_argument_by_name(&MCP_ARGUMENT_NAME)
        else {
            return Ok(None);
        };
        let coordinate = McpCoordinate { connect };
        let error = |message: String| Message {
            code: Code::InvalidMcp,
            message: format!("{coordinate} {message}"),
            locations: node
                .line_column_range(&schema.sources)
                .into_iter()
                .collect(),
        };

        // `@connect` has no `mcp` argument before v0.5, so GraphQL validation rejects it too, but
        // that message doesn't say which version to use.
        if schema.connect_link.spec < ConnectSpec::V0_5 {
            return Err(vec![error(format!(
                "requires connect spec v0.5 or later. Use `@link(url: \"{url}\")`.",
                url = ConnectSpec::V0_5.url(),
            ))]);
        }

        let mut messages = Vec::new();
        if connect
            .directive
            .specified_argument_by_name(&HTTP_ARGUMENT_NAME)
            .is_some()
        {
            messages.push(error(format!(
                "can't be combined with `{HTTP_ARGUMENT_NAME}:`. A connector calls an MCP tool or makes an HTTP request, not both."
            )));
        }
        match source_name {
            None => messages.push(error(format!(
                "needs a `source` whose `@{source}` declares `{MCP_ARGUMENT_NAME}:`, to say which MCP server to call.",
                source = schema.source_directive_name(),
            ))),
            Some(source_name) if !mcp_sources.contains(source_name) => {
                messages.push(error(format!(
                    "uses source \"{source_name}\", which isn't an MCP server. Add `{MCP_ARGUMENT_NAME}: {{}}` to its `@{source}`.",
                    source = schema.source_directive_name(),
                )))
            }
            Some(_) => {}
        }

        let Some(mcp_arg) = node.as_object() else {
            messages.push(error("must be an object.".to_string()));
            return Err(messages);
        };

        match mcp_arg
            .iter()
            .find(|(name, _)| name == &MCP_TOOL_ARGUMENT_NAME)
            .and_then(|(_, value)| value.as_str())
        {
            Some(tool) if !tool.trim().is_empty() => {}
            _ => messages.push(error(format!(
                "must name a tool with a non-empty `{MCP_TOOL_ARGUMENT_NAME}`."
            ))),
        }

        let arguments = match mcp_arg
            .iter()
            .find(|(name, _)| name == &MCP_ARGUMENTS_ARGUMENT_NAME)
        {
            None => None,
            Some((_, value)) => {
                let arguments_coordinate = ArgumentsCoordinate { connect };
                match parse_mapping_argument(value, arguments_coordinate, Code::InvalidMcp, schema)
                {
                    Ok(mapping) => Some(Arguments {
                        mapping,
                        coordinate: arguments_coordinate,
                    }),
                    Err(message) => {
                        messages.push(message);
                        None
                    }
                }
            }
        };

        if messages.is_empty() {
            Ok(Some(Self { arguments }))
        } else {
            Err(messages)
        }
    }

    /// Check that the `arguments` mapping only uses variables that exist here.
    pub(super) fn type_check(self, schema: &SchemaInfo) -> Result<(), Message> {
        let Some(Arguments {
            mapping,
            coordinate,
        }) = self.arguments
        else {
            return Ok(());
        };
        expression::validate(
            &mapping.expression,
            &Context::for_connect_request(
                schema,
                coordinate.connect,
                &mapping.node,
                Code::InvalidMcp,
            ),
            &Shape::unknown([]),
        )
        .map(|_| ())
        .map_err(|mut message| {
            message.message = format!("In {coordinate}: {message}", message = message.message);
            message
        })
    }

    pub(super) fn variables(&self) -> impl Iterator<Item = Namespace> + '_ {
        self.arguments.iter().flat_map(|arguments| {
            arguments
                .mapping
                .variable_references()
                .map(|var_ref| var_ref.namespace.namespace)
        })
    }
}

struct Arguments<'schema> {
    mapping: MappingArgument,
    coordinate: ArgumentsCoordinate<'schema>,
}

#[derive(Clone, Copy)]
struct McpCoordinate<'schema> {
    connect: ConnectDirectiveCoordinate<'schema>,
}

impl Display for McpCoordinate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`@{connect_directive_name}({MCP_ARGUMENT_NAME}:)` on `{element}`",
            connect_directive_name = self.connect.directive.name,
            element = self.connect.element
        )
    }
}

#[derive(Clone, Copy)]
struct ArgumentsCoordinate<'schema> {
    connect: ConnectDirectiveCoordinate<'schema>,
}

impl Display for ArgumentsCoordinate<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`@{connect_directive_name}({MCP_ARGUMENT_NAME}: {{{MCP_ARGUMENTS_ARGUMENT_NAME}:}})` on `{element}`",
            connect_directive_name = self.connect.directive.name,
            element = self.connect.element
        )
    }
}
