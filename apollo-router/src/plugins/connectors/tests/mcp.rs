//! `@connect(mcp:)`: the router writes the JSON-RPC `tools/call` request, reads the reply out of a
//! JSON or Server-Sent Events answer, and maps the tool's output.

use serde_json_bytes::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const MCP_SCHEMA: &str = include_str!("../testdata/mcp.graphql");

/// Runs `query` against the `quakes` MCP server mocked by `mock_server`. `sessions` is the
/// source's `mcp.sessions` setting, or `None` to leave it to the default for an MCP source.
async fn execute(
    mock_server: &MockServer,
    query: &str,
    sessions: Option<&str>,
) -> serde_json::Value {
    let mut source = json!({ "override_url": mock_server.uri() });
    if let Some(sessions) = sessions {
        source
            .as_object_mut()
            .unwrap()
            .insert("mcp", json!({ "sessions": sessions }));
    }
    super::execute(
        MCP_SCHEMA,
        &mock_server.uri(),
        query,
        Default::default(),
        Some(json!({ "connectors": { "sources": { "quakes.quakes": source } } })),
        |_| {},
        None,
    )
    .await
}

/// Runs `query` against a stateless server: the session layer is switched off, so the mock only
/// has to answer `tools/call`.
async fn execute_stateless(mock_server: &MockServer, query: &str) -> serde_json::Value {
    execute(mock_server, query, Some("off")).await
}

/// Mounts a `tools/call` of `tool` with `arguments` on the source's MCP endpoint.
async fn mount_tool_call(
    mock_server: &MockServer,
    tool: &str,
    arguments: serde_json::Value,
    response: ResponseTemplate,
) {
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_partial_json(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        })))
        .respond_with(response)
        .expect(1)
        .mount(mock_server)
        .await;
}

fn sse(events: &[&str]) -> ResponseTemplate {
    let body: String = events
        .iter()
        .map(|event| format!("event: message\ndata: {event}\n\n"))
        .collect();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

fn json_reply(reply: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(reply.to_string(), "application/json")
}

#[tokio::test]
async fn a_tool_call_sends_its_arguments_and_maps_the_structured_result_from_sse() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "search_events",
        serde_json::json!({ "limit": 2 }),
        sse(&[
            r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"events":[{"id":"a","magnitude":5.1},{"id":"b","magnitude":4.7}]},"content":[{"type":"text","text":"two events"}]}}"#,
        ]),
    )
    .await;

    let response = execute_stateless(
        &mock_server,
        "query { recentQuakes(limit: 2) { id magnitude } }",
    )
    .await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "recentQuakes": [
          {
            "id": "a",
            "magnitude": 5.1
          },
          {
            "id": "b",
            "magnitude": 4.7
          }
        ]
      }
    }
    "#);
}

#[tokio::test]
async fn a_tool_without_arguments_gets_an_empty_arguments_object_and_json_text_is_parsed() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "latest_event",
        serde_json::json!({}),
        json_reply(
            r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\"id\":\"c\",\"magnitude\":6.2}"}]}}"#,
        ),
    )
    .await;

    let response = execute_stateless(&mock_server, "query { latestQuake { id magnitude } }").await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "latestQuake": {
          "id": "c",
          "magnitude": 6.2
        }
      }
    }
    "#);
}

#[tokio::test]
async fn a_json_rpc_error_is_a_protocol_error_without_its_data() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "latest_event",
        serde_json::json!({}),
        sse(&[
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Unknown tool: latest_event","data":{"stack":"internal detail"}}}"#,
        ]),
    )
    .await;

    let response = execute_stateless(&mock_server, "query { latestQuake { id } }").await;

    assert_eq!(response["data"]["latestQuake"], serde_json::Value::Null);
    let error = &response["errors"][0];
    assert_eq!(error["message"], "Unknown tool: latest_event");
    assert_eq!(error["extensions"]["code"], "MCP_PROTOCOL_ERROR");
    assert_eq!(
        error["extensions"]["mcp"],
        serde_json::json!({ "code": -32602 })
    );
    assert!(
        !response.to_string().contains("internal detail"),
        "the JSON-RPC error's data stays out of the response: {response}"
    );
}

#[tokio::test]
async fn a_tool_result_with_is_error_is_a_tool_error_with_the_tools_text() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "get_event",
        serde_json::json!({ "event_id": "zzz" }),
        json_reply(
            r#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"No event with id zzz"}]}}"#,
        ),
    )
    .await;

    let response = execute_stateless(&mock_server, r#"query { quake(id: "zzz") { id } }"#).await;

    assert_eq!(response["data"]["quake"], serde_json::Value::Null);
    let error = &response["errors"][0];
    assert_eq!(error["message"], "No event with id zzz");
    assert_eq!(error["extensions"]["code"], "MCP_TOOL_ERROR");
}

#[tokio::test]
async fn a_stream_with_no_reply_is_an_invalid_response() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "latest_event",
        serde_json::json!({}),
        sse(&[r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progress":1}}"#]),
    )
    .await;

    let response = execute_stateless(&mock_server, "query { latestQuake { id } }").await;

    assert_eq!(response["data"]["latestQuake"], serde_json::Value::Null);
    assert_eq!(
        response["errors"][0]["extensions"]["code"],
        "MCP_INVALID_RESPONSE"
    );
}

#[tokio::test]
async fn an_authors_errors_mapping_sees_the_json_rpc_reply() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "latest_event",
        serde_json::json!({}),
        json_reply(
            r#"{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"Catalog offline"}]}}"#,
        ),
    )
    .await;

    let response = execute_stateless(&mock_server, "query { checkedQuake { id } }").await;

    assert_eq!(response["data"]["checkedQuake"], serde_json::Value::Null);
    let error = &response["errors"][0];
    assert_eq!(error["message"], "Catalog offline");
    assert_eq!(error["extensions"]["code"], "QUAKE_TOOL_FAILED");
}

#[tokio::test]
async fn a_batch_of_entities_is_resolved_by_one_tool_call() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        "list_stations",
        serde_json::json!({}),
        json_reply(
            r#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"stations":[{"id":"s1"},{"id":"s2"}]}}}"#,
        ),
    )
    .await;
    mount_tool_call(
        &mock_server,
        "get_stations",
        serde_json::json!({ "ids": ["s1", "s2"] }),
        json_reply(
            r#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"stations":[{"id":"s2","name":"Two"},{"id":"s1","name":"One"}]}}}"#,
        ),
    )
    .await;

    let response = execute_stateless(&mock_server, "query { stations { id name } }").await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "stations": [
          {
            "id": "s1",
            "name": "One"
          },
          {
            "id": "s2",
            "name": "Two"
          }
        ]
      }
    }
    "#);
}

#[tokio::test]
async fn an_mcp_source_holds_a_session_without_any_router_config() {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_partial_json(serde_json::json!({ "method": "initialize" })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("mcp-session-id", "session-1")
                .set_body_raw(
                    r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"quakes","version":"1"}}}"#,
                    "application/json",
                ),
        )
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_partial_json(
            serde_json::json!({ "method": "notifications/initialized" }),
        ))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;
    // The server only answers a tool call that carries the session it issued
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-session-id", "session-1"))
        .and(body_partial_json(serde_json::json!({
            "method": "tools/call",
            "params": { "name": "latest_event", "arguments": {} }
        })))
        .respond_with(json_reply(
            r#"{"jsonrpc":"2.0","id":1,"result":{"structuredContent":{"id":"d","magnitude":5.5}}}"#,
        ))
        .expect(1)
        .with_priority(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string("Bad Request: Mcp-Session-Id header is required"),
        )
        .expect(0)
        .mount(&mock_server)
        .await;

    let response = execute(&mock_server, "query { latestQuake { id magnitude } }", None).await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "latestQuake": {
          "id": "d",
          "magnitude": 5.5
        }
      }
    }
    "#);
}
