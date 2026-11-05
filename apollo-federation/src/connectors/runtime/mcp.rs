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

/// The error code for a 401 from an MCP server: the caller needs a credential the server accepts.
pub(crate) const UNAUTHENTICATED_CODE: &str = "UNAUTHENTICATED";

/// The error code for a 403 from an MCP server: the credential lacks a permission or scope.
pub(crate) const FORBIDDEN_CODE: &str = "FORBIDDEN";

/// The parameters of a `Bearer` challenge in `WWW-Authenticate`, which MCP servers use to tell a
/// client where to get a token (`resource_metadata`, from RFC 9728) and what it must allow
/// (`scope`, `error`, `error_description`, from RFC 6750).
///
/// They become `extensions.mcp.auth`, so a client that can run the OAuth flow itself knows where
/// to start and can retry with a token. Parameters other than these are left out.
pub(crate) fn bearer_challenge(www_authenticate: &str) -> Option<Value> {
    const PARAMS: [(&str, &str); 4] = [
        ("resource_metadata", "resourceMetadata"),
        ("scope", "scope"),
        ("error", "error"),
        ("error_description", "errorDescription"),
    ];
    let rest = www_authenticate.trim_start();
    let scheme_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    if !rest[..scheme_end].eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut auth = Map::new();
    for (name, value) in auth_params(&rest[scheme_end..]) {
        if let Some((_, key)) = PARAMS
            .iter()
            .find(|(param, _)| param.eq_ignore_ascii_case(&name))
        {
            auth.insert(*key, Value::String(value.into()));
        }
    }
    Some(Value::Object(auth))
}

/// The `name=value` pairs of an auth challenge, where a value is a token or a quoted string.
fn auth_params(mut rest: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
        let Some(eq) = rest.find('=') else {
            return params;
        };
        let name = rest[..eq].trim().to_string();
        rest = rest[eq + 1..].trim_start();
        let value = if let Some(quoted) = rest.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, escaped)) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => value.push(c),
                }
            }
            rest = &quoted[end..];
            value
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let value = rest[..end].trim().to_string();
            rest = &rest[end..];
            value
        };
        params.push((name, value));
    }
}

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
    fn a_bearer_challenge_keeps_what_a_client_needs_to_get_a_token() {
        let challenge = r#"Bearer realm="mcp", resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="mcp:connect files:read", error="invalid_token", error_description="The token \"t\" expired""#;
        assert_eq!(
            bearer_challenge(challenge),
            Some(json!({
                "resourceMetadata": "https://mcp.example.com/.well-known/oauth-protected-resource",
                "scope": "mcp:connect files:read",
                "error": "invalid_token",
                "errorDescription": "The token \"t\" expired",
            }))
        );
    }

    #[test]
    fn a_bearer_challenge_with_token_values_and_no_params() {
        assert_eq!(
            bearer_challenge("bearer error=insufficient_scope, scope=admin"),
            Some(json!({ "error": "insufficient_scope", "scope": "admin" }))
        );
        assert_eq!(bearer_challenge("Bearer"), Some(json!({})));
        assert_eq!(bearer_challenge(r#"Basic realm="x""#), None);
    }

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
