//! Reading the answer to an MCP `tools/call`, for `@connect(mcp:)`.
//!
//! The transport folds the HTTP response into a list of JSON events, whether the server answered
//! with plain JSON or with Server-Sent Events. Among those events is one JSON-RPC reply, possibly
//! after progress notifications and log messages. MCP reports a failure in one of two ways, both
//! with HTTP 200: a JSON-RPC `error`, or a tool result with `isError: true`. This module finds the
//! reply, decides which of those happened, and gives the selection the tool's output.

use serde_json_bytes::Map;
use serde_json_bytes::Value;

/// The error code for a JSON-RPC `error` reply: the server refused the call itself, for example
/// because the tool doesn't exist or its arguments don't fit the tool's input schema. It usually
/// means the schema and the server disagree.
pub(crate) const MCP_PROTOCOL_ERROR_CODE: &str = "MCP_PROTOCOL_ERROR";

/// The error code for a tool result with `isError: true`: the tool ran and reported a failure.
pub(crate) const MCP_TOOL_ERROR_CODE: &str = "MCP_TOOL_ERROR";

/// The error code for a successful HTTP response that holds no JSON-RPC reply.
pub(crate) const MCP_INVALID_RESPONSE_CODE: &str = "MCP_INVALID_RESPONSE";

/// The JSON-RPC reply among the folded events: an object with an `id` and a `result` or an
/// `error`, and no `method`, which would make it a request or notification from the server.
///
/// Each HTTP request carries one `tools/call`, so there is at most one reply to it. The reply's
/// `id` isn't compared with the request's, because a session layer may have renumbered the
/// request on its way out. If a server sends several replies anyway, the last one wins.
pub(crate) fn reply(events: &Value) -> Option<&Value> {
    let events = match events {
        Value::Array(events) => events.as_slice(),
        single => std::slice::from_ref(single),
    };
    events.iter().rev().find(|event| {
        event.as_object().is_some_and(|event| {
            event.contains_key("id")
                && !event.contains_key("method")
                && (event.contains_key("result") || event.contains_key("error"))
        })
    })
}

/// What went wrong with a reply, if anything.
#[derive(Debug, PartialEq)]
pub(crate) enum Failure<'a> {
    /// No JSON-RPC reply came back
    NoReply,
    /// A JSON-RPC `error`
    Protocol {
        message: Option<&'a str>,
        code: Option<&'a Value>,
    },
    /// A tool result with `isError: true`
    Tool { message: Option<String> },
}

/// Whether the reply reports a failure, and which kind.
pub(crate) fn failure(reply: Option<&Value>) -> Option<Failure<'_>> {
    let Some(reply) = reply else {
        return Some(Failure::NoReply);
    };
    if let Some(error) = reply.get("error") {
        return Some(Failure::Protocol {
            message: error.get("message").and_then(Value::as_str),
            code: error.get("code"),
        });
    }
    let result = reply.get("result")?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Some(Failure::Tool {
            message: text_content(result),
        });
    }
    None
}

/// What the selection of an `@connect(mcp:)` connector receives: the tool's `structuredContent`
/// when it has one. Otherwise the text of its content, parsed as JSON if it is JSON and as a
/// string if not, since many tools put JSON in a text block instead of declaring an output schema.
pub(crate) fn tool_output(reply: &Value) -> Value {
    let Some(result) = reply.get("result") else {
        return Value::Null;
    };
    if let Some(structured) = result.get("structuredContent") {
        return structured.clone();
    }
    match text_content(result) {
        Some(text) => {
            serde_json::from_str::<Value>(&text).unwrap_or_else(|_| Value::String(text.into()))
        }
        None => Value::Null,
    }
}

/// The text blocks of a tool result's `content`, joined by newlines, or `None` if it has none.
fn text_content(result: &Value) -> Option<String> {
    let texts = result
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// The `extensions.mcp` of a protocol error: the JSON-RPC error code, which says what kind of
/// refusal it was. The error's `data` is left out, since servers put internals there.
pub(crate) fn protocol_error_extension(code: Option<&Value>) -> Value {
    let mut mcp = Map::new();
    if let Some(code) = code {
        mcp.insert("code", code.clone());
    }
    Value::Object(mcp)
}

#[cfg(test)]
mod tests {
    use serde_json_bytes::json;

    use super::*;

    #[test]
    fn the_reply_is_found_after_progress_notifications() {
        let events = json!([
            { "jsonrpc": "2.0", "method": "notifications/progress", "params": { "progress": 1 } },
            { "jsonrpc": "2.0", "id": 7, "result": { "structuredContent": { "temp": 21 } } },
        ]);
        let reply = reply(&events).unwrap();
        assert_eq!(tool_output(reply), json!({ "temp": 21 }));
        assert_eq!(failure(Some(reply)), None);
    }

    #[test]
    fn a_request_from_the_server_is_not_a_reply() {
        let events = json!([
            { "jsonrpc": "2.0", "id": 1, "method": "elicitation/create", "params": {} },
        ]);
        assert_eq!(reply(&events), None);
        assert_eq!(failure(None), Some(Failure::NoReply));
    }

    #[test]
    fn a_json_rpc_error_is_a_protocol_failure() {
        let events = json!([
            { "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "Unknown tool: nope", "data": "secret" } },
        ]);
        let reply = reply(&events);
        assert_eq!(
            failure(reply),
            Some(Failure::Protocol {
                message: Some("Unknown tool: nope"),
                code: Some(&json!(-32602)),
            })
        );
        assert_eq!(
            protocol_error_extension(Some(&json!(-32602))),
            json!({ "code": -32602 })
        );
    }

    #[test]
    fn is_error_is_a_tool_failure_with_the_tools_text() {
        let events = json!([{
            "jsonrpc": "2.0", "id": 1,
            "result": { "isError": true, "content": [
                { "type": "text", "text": "Event not found" },
                { "type": "image", "data": "..." },
                { "type": "text", "text": "Try another id" },
            ] },
        }]);
        assert_eq!(
            failure(reply(&events)),
            Some(Failure::Tool {
                message: Some("Event not found\nTry another id".to_string())
            })
        );
    }

    #[test]
    fn text_content_is_parsed_as_json_when_it_is_json() {
        let reply = json!({ "id": 1, "result": { "content": [{ "type": "text", "text": "{\"a\": [1, 2]}" }] } });
        assert_eq!(tool_output(&reply), json!({ "a": [1, 2] }));
    }

    #[test]
    fn text_content_stays_a_string_when_it_is_not_json() {
        let reply =
            json!({ "id": 1, "result": { "content": [{ "type": "text", "text": "hello" }] } });
        assert_eq!(tool_output(&reply), json!("hello"));
    }

    #[test]
    fn a_plain_json_body_folded_to_one_event_still_has_its_reply() {
        let events =
            json!([{ "jsonrpc": "2.0", "id": 1, "result": { "structuredContent": [1, 2, 3] } }]);
        assert_eq!(tool_output(reply(&events).unwrap()), json!([1, 2, 3]));
    }
}
