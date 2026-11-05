//! `@source(mcp:)` and `@connect(mcp:)`, added in connect/v0.5.
//!
//! A source with `mcp:` is a Model Context Protocol server reached over Streamable HTTP. A
//! connector with `mcp:` calls one of its tools: the router writes the JSON-RPC `tools/call`
//! request, folds the answer whether it arrives as JSON or as Server-Sent Events, and hands the
//! selection the tool's result.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::name;

use crate::connectors::ConnectSpec;
use crate::connectors::json_selection::JSONSelection;
use crate::error::FederationError;

/// `@source(mcp:)` and `@connect(mcp:)`
pub(crate) const MCP_ARGUMENT_NAME: Name = name!("mcp");
/// The input type of `@connect(mcp:)`
pub(crate) const CONNECT_MCP_NAME_IN_SPEC: Name = name!("ConnectMCP");
/// The input type of `@source(mcp:)`
pub(crate) const SOURCE_MCP_NAME_IN_SPEC: Name = name!("SourceMCP");
/// `@connect(mcp: {tool:})`, the name of the tool to call
pub(crate) const MCP_TOOL_ARGUMENT_NAME: Name = name!("tool");
/// `@connect(mcp: {arguments:})`, a mapping that builds the tool's arguments
pub(crate) const MCP_ARGUMENTS_ARGUMENT_NAME: Name = name!("arguments");
/// `@source(mcp: {path:})`, the path of the MCP endpoint, appended to `http.baseURL`
pub(crate) const MCP_PATH_ARGUMENT_NAME: Name = name!("path");

type ObjectNode = [(Name, Node<Value>)];

/// Parsed `@connect(mcp:)`
#[derive(Clone, Debug)]
pub struct ConnectMCPArguments {
    /// The tool to call, as `tools/list` names it
    pub(crate) tool: String,
    /// The tool's arguments, built from `$args`, `$this` and the other request variables.
    pub(crate) arguments: Option<JSONSelection>,
}

impl TryFrom<(&ObjectNode, &Name, ConnectSpec)> for ConnectMCPArguments {
    type Error = FederationError;

    fn try_from(
        (values, directive_name, connect_spec): (&ObjectNode, &Name, ConnectSpec),
    ) -> Result<Self, FederationError> {
        let mut tool = None;
        let mut arguments = None;
        for (name, value) in values {
            if *name == MCP_TOOL_ARGUMENT_NAME {
                tool = Some(
                    value
                        .as_str()
                        .ok_or_else(|| {
                            FederationError::internal(format!(
                                "`{MCP_TOOL_ARGUMENT_NAME}` in `@{directive_name}({MCP_ARGUMENT_NAME}:)` is not a string"
                            ))
                        })?
                        .to_string(),
                );
            } else if *name == MCP_ARGUMENTS_ARGUMENT_NAME {
                let value = value.as_str().ok_or_else(|| {
                    FederationError::internal(format!(
                        "`{MCP_ARGUMENTS_ARGUMENT_NAME}` in `@{directive_name}({MCP_ARGUMENT_NAME}:)` is not a string"
                    ))
                })?;
                arguments = Some(
                    JSONSelection::parse_with_spec(value, connect_spec)
                        .map_err(|e| FederationError::internal(e.message))?,
                );
            }
        }
        Ok(Self {
            tool: tool.ok_or_else(|| {
                FederationError::internal(format!(
                    "`@{directive_name}({MCP_ARGUMENT_NAME}:)` is missing `{MCP_TOOL_ARGUMENT_NAME}`"
                ))
            })?,
            arguments,
        })
    }
}

/// Parsed `@source(mcp:)`
#[derive(Clone, Debug, Default)]
pub struct SourceMCPArguments {
    /// The path of the MCP endpoint, appended to the source's `http.baseURL`. Without one, the
    /// base URL is the endpoint.
    pub(crate) path: Option<String>,
}

impl TryFrom<(&ObjectNode, &Name)> for SourceMCPArguments {
    type Error = FederationError;

    fn try_from((values, directive_name): (&ObjectNode, &Name)) -> Result<Self, FederationError> {
        let mut path = None;
        for (name, value) in values {
            if *name == MCP_PATH_ARGUMENT_NAME {
                path = Some(
                    value
                        .as_str()
                        .ok_or_else(|| {
                            FederationError::internal(format!(
                                "`{MCP_PATH_ARGUMENT_NAME}` in `@{directive_name}({MCP_ARGUMENT_NAME}:)` is not a string"
                            ))
                        })?
                        .to_string(),
                );
            }
        }
        Ok(Self { path })
    }
}
