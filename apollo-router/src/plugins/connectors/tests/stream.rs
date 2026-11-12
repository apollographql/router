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
async fn an_mcp_error_answered_with_sse_is_mapped_by_is_success_and_errors() {
    let mock_server = MockServer::start().await;
    let body = concat!(
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":1}}\n",
        "\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32602,\"message\":\"Unknown city\"}}\n",
        "\n",
    );
    mount_tool_call(
        &mock_server,
        ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"),
    )
    .await;

    let response = execute(&mock_server, r#"query { weather(city: "Paris") { temp } }"#).await;

    // `isSuccess` and `errors` see the same list of events as the selection
    assert_eq!(response["data"]["weather"], serde_json::Value::Null);
    assert_eq!(response["errors"][0]["message"], "Unknown city");
}

#[tokio::test]
async fn an_error_status_with_a_plain_json_body_is_mapped_as_a_list_of_one() {
    let mock_server = MockServer::start().await;
    mount_tool_call(
        &mock_server,
        ResponseTemplate::new(400).set_body_raw(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Bad request"}}"#,
            "application/json",
        ),
    )
    .await;

    let response = execute(&mock_server, r#"query { weather(city: "Paris") { temp } }"#).await;

    assert_eq!(response["data"]["weather"], serde_json::Value::Null);
    assert_eq!(response["errors"][0]["message"], "Bad request");
}

#[tokio::test]
async fn an_entity_resolver_with_entity_true_maps_one_event() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/featured"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(r#"{"id":"1"}"#, "application/json"))
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/products/1"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"id\":\"1\",\"name\":\"Draft\"}\n",
                "\n",
                "data: {\"id\":\"1\",\"name\":\"Lamp\"}\n",
                "\n",
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&mock_server)
        .await;

    let response = execute(&mock_server, "query { featured { id name } }").await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "featured": {
          "id": "1",
          "name": "Lamp"
        }
      }
    }
    "#);
}

#[tokio::test]
async fn an_entity_resolver_on_a_type_maps_one_event() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/featured-store"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(r#"{"id":"7"}"#, "application/json"))
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/stores/7"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "{\"id\":\"7\",\"name\":\"Opening\"}\n{\"id\":\"7\",\"name\":\"Main Street\"}\n",
            "application/x-ndjson",
        ))
        .expect(1)
        .mount(&mock_server)
        .await;

    let response = execute(&mock_server, "query { featuredStore { id name } }").await;

    insta::assert_json_snapshot!(response, @r#"
    {
      "data": {
        "featuredStore": {
          "id": "7",
          "name": "Main Street"
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
