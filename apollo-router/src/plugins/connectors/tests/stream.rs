//! `@connect(http: {stream: "array"})`: the selection receives the list of a response's JSON
//! events, whether the server streams them or answers with one plain JSON body.

use serde_json_bytes::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

const STREAM_SCHEMA: &str = include_str!("../testdata/stream.graphql");

/// What a connector with a stream mode sends when the schema sets no `Accept` header.
const STREAM_ACCEPT: &str = "application/json, text/event-stream, application/x-ndjson";

async fn execute(mock_server: &MockServer, query: &str) -> serde_json::Value {
    super::execute(
        STREAM_SCHEMA,
        &mock_server.uri(),
        query,
        Default::default(),
        Some(json!({
            "connectors": {
                "sources": {
                    "streams.mcp": { "override_url": mock_server.uri() }
                }
            }
        })),
        |_| {},
        None,
    )
    .await
}

/// Mounts the MCP `tools/call` endpoint.
async fn mount_tool_call(mock_server: &MockServer, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_json(serde_json::json!({
            "method": "tools/call",
            "params": { "name": "weather", "arguments": { "city": "Paris" } }
        })))
        .respond_with(response)
        .expect(1)
        .mount(mock_server)
        .await;
}

/// The `Accept` header of every request the mock server received.
async fn received_accept_headers(mock_server: &MockServer) -> Vec<String> {
    mock_server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| {
            request
                .headers
                .get("accept")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn an_mcp_tool_call_answered_with_sse_maps_its_last_message() {
    let mock_server = MockServer::start().await;
    let body = concat!(
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":1}}\n",
        "\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"temp\":18}}\n",
        "\n",
    );
    mount_tool_call(
        &mock_server,
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"),
    )
    .await;

    let response = execute(&mock_server, r#"query { weather(city: "Paris") { temp } }"#).await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "weather": {
          "temp": 18
        }
      }
    }
    "#);
    assert_eq!(received_accept_headers(&mock_server).await, [STREAM_ACCEPT]);
}

#[tokio::test]
async fn an_mcp_tool_call_answered_with_plain_json_maps_the_same_way() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        ResponseTemplate::new(200).set_body_raw(
            r#"{"jsonrpc":"2.0","id":1,"result":{"temp":18}}"#,
            "application/json",
        ),
    )
    .await;

    let response = execute(&mock_server, r#"query { weather(city: "Paris") { temp } }"#).await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "weather": {
          "temp": 18
        }
      }
    }
    "#);
}

#[tokio::test]
async fn a_json_lines_stream_maps_every_event() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/progress"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "{\"params\":{\"progress\":0.5}}\n{\"params\":{\"progress\":1}}\n",
            "application/x-ndjson",
        ))
        .expect(1)
        .mount(&mock_server)
        .await;

    let response = execute(&mock_server, "query { progress { percent } }").await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "progress": [
          {
            "percent": 0.5
          },
          {
            "percent": 1
          }
        ]
      }
    }
    "#);
}
