//! Connector-backed subscriptions, end to end: an upstream that answers with
//! a streamed body, and a client that receives one subscription event per
//! upstream event.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::Configuration;
use crate::plugins::connectors::subscription::ConnectorSubscriptionStream;
use crate::router_factory::PipelineFactory;
use crate::router_factory::RouterFactory;
use crate::router_factory::RouterServiceFactory;
use crate::services::router;
use crate::services::supergraph;
use crate::uplink::license_enforcement::LicenseState;

const SCHEMA: &str = include_str!("../testdata/subscription.graphql");
/// The same `streams` subgraph, with `Price` as an entity that a plain GraphQL
/// `catalog` subgraph adds a `name` to.
const ENTITIES_SCHEMA: &str = include_str!("../testdata/subscription_entities.graphql");

/// Opens a subscription against a router whose `streams.market` source and
/// `catalog` subgraph both point at `uri`, and returns the response with its
/// multipart stream still open. `max_requests` is the connector request limit
/// per operation and source, if any.
async fn open(
    schema: &str,
    uri: &str,
    query: &str,
    max_requests: Option<usize>,
) -> router::Response {
    open_with_traffic_shaping(schema, uri, query, max_requests, serde_json::json!({})).await
}

/// [`open`], with a `traffic_shaping` configuration.
async fn open_with_traffic_shaping(
    schema: &str,
    uri: &str,
    query: &str,
    max_requests: Option<usize>,
    traffic_shaping: serde_json::Value,
) -> router::Response {
    let config: Configuration = serde_json::from_value(serde_json::json!({
        "include_subgraph_errors": { "all": true },
        "override_subgraph_url": { "catalog": format!("{uri}/catalog") },
        // No subgraph mode: connectors hold the upstream connection themselves
        "subscription": { "enabled": true },
        "connectors": {
            "max_requests_per_operation_per_source": max_requests,
            "sources": { "streams.market": { "override_url": uri } }
        },
        "traffic_shaping": traffic_shaping,
    }))
    .unwrap();

    let pipeline = PipelineFactory
        .create_pipeline(
            false,
            Arc::new(config.clone()),
            Arc::new(crate::spec::Schema::parse(schema, &config).unwrap()),
            None,
            None,
            // Subscriptions are a licensed feature: unlicensed, the plugin is never instantiated
            Arc::new(LicenseState::Licensed {
                limits: Default::default(),
            }),
        )
        .await
        .unwrap();

    let request = supergraph::Request::fake_builder()
        .query(query)
        .header("accept", "multipart/mixed;subscriptionSpec=1.0")
        .build()
        .unwrap()
        .try_into()
        .unwrap();

    pipeline.create().oneshot(request).await.unwrap()
}

/// Runs a subscription to completion and returns the whole multipart body.
async fn subscribe(schema: &str, uri: &str, query: &str) -> String {
    let mut response = open(schema, uri, query, None).await;
    drain(&mut response).await
}

/// Reads the rest of a response body.
async fn drain(response: &mut router::Response) -> String {
    let mut body = Vec::new();
    while let Some(chunk) = response.next_response().await {
        body.extend_from_slice(&chunk.unwrap());
    }
    String::from_utf8(body).unwrap()
}

/// A minimal HTTP/1.1 upstream for one connection: it answers with an SSE
/// stream that carries one price event and then only heartbeats, for as long
/// as the router keeps the connection open. The returned receiver resolves
/// once the router hangs up, which is the only way the stream ever ends.
async fn trickling_sse_upstream() -> (String, tokio::sync::oneshot::Receiver<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.split();

        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }

        let chunk = |payload: &str| format!("{:x}\r\n{payload}\r\n", payload.len());
        writer
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Type: text/event-stream\r\n",
                    "Transfer-Encoding: chunked\r\n",
                    "\r\n",
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        writer
            .write_all(chunk("data: {\"symbol\":\"APOL\",\"amount\":1.5}\n\n").as_bytes())
            .await
            .unwrap();

        // Heartbeats until the router hangs up, seen as EOF on the read side
        // or a failed write
        let mut heartbeat = tokio::time::interval(Duration::from_millis(5));
        let mut eof = [0u8; 1];
        loop {
            tokio::select! {
                read = reader.read(&mut eof) => {
                    if matches!(read, Ok(0) | Err(_)) {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if writer.write_all(chunk(": ping\n\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = closed_tx.send(());
    });
    (uri, closed_rx)
}

#[tokio::test]
async fn dropping_the_client_closes_an_idle_upstream_stream() {
    let (uri, upstream_closed) = trickling_sse_upstream().await;

    let mut response = open(
        SCHEMA,
        &uri,
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
        None,
    )
    .await;

    // The first event proves the stream is open end to end
    let mut body = String::new();
    while !body.contains("\"amount\":1.5") {
        let chunk = response
            .next_response()
            .await
            .expect("the subscription ended before its first event")
            .unwrap();
        body.push_str(std::str::from_utf8(&chunk).unwrap());
    }

    // The client leaves. From here on the upstream only sends heartbeats, which
    // are not events, so the router must notice the client is gone on its own
    // rather than when the next event arrives.
    drop(response);
    tokio::time::timeout(Duration::from_secs(10), upstream_closed)
        .await
        .expect("the router kept the upstream connection open after the client left")
        .unwrap();
}

#[tokio::test]
async fn a_subscription_rejected_before_its_request_is_sent_releases_the_stream_sender() {
    let mock_server = MockServer::start().await;

    // A limit of zero rejects the opening request above the request service
    let mut response = open(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
        Some(0),
    )
    .await;
    let body = drain(&mut response).await;

    assert!(
        body.contains("REQUEST_LIMIT_EXCEEDED"),
        "expected the request limit error, got: {body}"
    );
    assert!(mock_server.received_requests().await.unwrap().is_empty());
    // The sender planted for the request service must not outlive the fetch:
    // `subscription_task` holds this context and waits on the sender's
    // receiver, so a sender left here keeps the task alive forever.
    assert!(
        response
            .context
            .extensions()
            .with_lock(|lock| lock.get::<ConnectorSubscriptionStream>().is_none()),
        "the subscription stream sender was left in the context"
    );
}

/// The payloads of a multipart subscription body, in order, without the
/// heartbeats.
fn payloads(body: &str) -> Vec<serde_json::Value> {
    body.split("\r\n")
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|part| part.as_object().is_some_and(|part| !part.is_empty()))
        .collect()
}

#[tokio::test]
async fn each_sse_event_becomes_a_subscription_event() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream/APOL"))
        .respond_with(
            // `set_body_raw`, because `set_body_string` adds a `text/plain` content type of its own
            ResponseTemplate::new(200).set_body_raw(
                concat!(
                    ": connected\n\n",
                    "event: price\nid: 1\ndata: {\"symbol\":\"APOL\",\"amount\":1.5,\"ignored\":true}\n\n",
                    "event: price\nid: 2\ndata: {\"symbol\":\"APOL\",\"amount\":2.25}\n\n",
                    "data: [DONE]\n\n",
                ),
                "text/event-stream",
            ),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    // Two events: the comment and the `[DONE]` sentinel are not data, and the
    // selection drops the field it doesn't name
    insta::assert_json_snapshot!(payloads(&body), @r#"
    [
      {
        "payload": {
          "data": {
            "prices": {
              "symbol": "APOL",
              "amount": 1.5
            }
          }
        }
      },
      {
        "payload": {
          "data": {
            "prices": {
              "symbol": "APOL",
              "amount": 2.25
            }
          }
        }
      }
    ]
    "#);
}

#[tokio::test]
async fn a_plain_json_response_is_a_subscription_of_one_event() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream/APOL"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "symbol": "APOL", "amount": 3.0 })),
        )
        .mount(&mock_server)
        .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    insta::assert_json_snapshot!(payloads(&body), @r#"
    [
      {
        "payload": {
          "data": {
            "prices": {
              "symbol": "APOL",
              "amount": 3.0
            }
          }
        }
      }
    ]
    "#);
}

#[tokio::test]
async fn each_event_is_completed_by_an_entity_fetch_to_another_subgraph() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream/APOL"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"symbol\":\"APOL\",\"amount\":1.5}\n\n",
                "data: {\"symbol\":\"APOL\",\"amount\":2.25}\n\n",
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/catalog"))
        .and(body_partial_json(serde_json::json!({
            "variables": { "representations": [{ "__typename": "Price", "symbol": "APOL" }] }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "_entities": [{ "name": "Apollo" }] }
        })))
        // Once per event, not once per subscription
        .expect(2)
        .mount(&mock_server)
        .await;

    let body = subscribe(
        ENTITIES_SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount name } }"#,
    )
    .await;

    insta::assert_json_snapshot!(payloads(&body), @r#"
    [
      {
        "payload": {
          "data": {
            "prices": {
              "symbol": "APOL",
              "amount": 1.5,
              "name": "Apollo"
            }
          }
        }
      },
      {
        "payload": {
          "data": {
            "prices": {
              "symbol": "APOL",
              "amount": 2.25,
              "name": "Apollo"
            }
          }
        }
      }
    ]
    "#);
}

/// Mounts `GET {route}` answering 200 with `body` as `content_type`.
async fn mount_stream(
    mock_server: &MockServer,
    route: &str,
    body: &'static str,
    content_type: &str,
) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, content_type))
        .expect(1)
        .mount(mock_server)
        .await;
}

#[tokio::test]
async fn an_event_whose_selection_yields_nothing_is_not_delivered() {
    let mock_server = MockServer::start().await;
    mount_stream(
        &mock_server,
        "/quotes/APOL",
        concat!(
            "data: {\"type\":\"progress\",\"progress\":0.5}\n\n",
            "data: {\"quote\":{\"symbol\":\"APOL\",\"amount\":1.5}}\n\n",
            "data: {\"type\":\"progress\",\"progress\":1}\n\n",
            "data: {\"quote\":{\"symbol\":\"APOL\",\"amount\":2.25}}\n\n",
        ),
        "text/event-stream",
    )
    .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { quotes(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    // `$.quote { ... }` is missing for the progress events, so only the quotes are delivered
    insta::assert_json_snapshot!(payloads(&body), @r#"
    [
      {
        "payload": {
          "data": {
            "quotes": {
              "symbol": "APOL",
              "amount": 1.5
            }
          }
        }
      },
      {
        "payload": {
          "data": {
            "quotes": {
              "symbol": "APOL",
              "amount": 2.25
            }
          }
        }
      }
    ]
    "#);
}

#[tokio::test]
async fn under_stream_array_each_event_maps_every_event_so_far() {
    let mock_server = MockServer::start().await;
    mount_stream(
        &mock_server,
        "/stream/APOL",
        "{\"symbol\":\"APOL\",\"amount\":1}\n{\"symbol\":\"APOL\",\"amount\":2}\n{\"symbol\":\"APOL\",\"amount\":3}\n",
        "application/x-ndjson",
    )
    .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { history(symbol: "APOL") { amount } }"#,
    )
    .await;

    let amounts: Vec<serde_json::Value> = payloads(&body)
        .iter()
        .map(|payload| {
            payload["payload"]["data"]["history"]
                .as_array()
                .unwrap()
                .iter()
                .map(|price| price["amount"].clone())
                .collect()
        })
        .collect();
    assert_eq!(
        amounts,
        [
            serde_json::json!([1]),
            serde_json::json!([1, 2]),
            serde_json::json!([1, 2, 3])
        ]
    );
}

#[tokio::test]
async fn a_declared_error_is_reported_on_the_payload_of_its_event() {
    let mock_server = MockServer::start().await;
    mount_stream(
        &mock_server,
        "/stream/APOL",
        concat!(
            "data: {\"symbol\":\"APOL\",\"amount\":1.5}\n\n",
            "data: {\"symbol\":\"APOL\",\"amount\":0}\n\n",
            "data: {\"symbol\":\"APOL\",\"amount\":2.25}\n\n",
        ),
        "text/event-stream",
    )
    .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { checked(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    let payloads = payloads(&body);
    assert_eq!(payloads.len(), 3, "{body}");
    let connector_errors =
        |index: usize| payloads[index]["payload"]["extensions"]["connectorErrors"].clone();
    assert_eq!(connector_errors(0), serde_json::Value::Null);
    assert_eq!(connector_errors(1)[0]["message"], "Zero price");
    assert_eq!(connector_errors(2), serde_json::Value::Null);
    // The field still resolves: a declared error is reported, not raised
    assert_eq!(payloads[1]["payload"]["data"]["checked"]["amount"], 0);
}

#[tokio::test]
async fn an_empty_body_delivers_no_events() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream/APOL"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    // No subscription payload: the router answers a subscription that ends before its first
    // event with only an empty primary response
    assert!(
        payloads(&body)
            .iter()
            .all(|part| part.get("payload").is_none()),
        "{body}"
    );
    assert!(!body.contains("\"amount\""), "{body}");
}

#[tokio::test]
async fn an_invalid_plain_body_is_an_error_event() {
    let mock_server = MockServer::start().await;
    mount_stream(
        &mock_server,
        "/stream/APOL",
        "{not json",
        "application/json",
    )
    .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    assert!(
        body.contains("CONNECTOR_SUBSCRIPTION_INVALID_BODY"),
        "expected an invalid body error, got: {body}"
    );
}

#[tokio::test]
async fn an_error_status_is_one_error_event() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/stream/APOL"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(serde_json::json!({ "message": "market closed" })),
        )
        .expect(1)
        .mount(&mock_server)
        .await;

    let body = subscribe(
        SCHEMA,
        &mock_server.uri(),
        r#"subscription { prices(symbol: "APOL") { symbol amount } }"#,
    )
    .await;

    assert!(
        body.contains("\"errors\""),
        "expected an error payload, got: {body}"
    );
    assert!(!body.contains("\"amount\""), "{body}");
}

/// A minimal HTTP/1.1 upstream for one connection: after `header_delay` it sends its response
/// headers and one price event, and a second event only after `delay`, then ends the stream.
async fn slow_sse_upstream(header_delay: Duration, delay: Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (mut reader, mut writer) = socket.split();

        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => request.extend_from_slice(&buf[..n]),
            }
        }

        let chunk = |payload: &str| format!("{:x}\r\n{payload}\r\n", payload.len());
        tokio::time::sleep(header_delay).await;
        let _ = writer
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\n",
                    "Content-Type: text/event-stream\r\n",
                    "Transfer-Encoding: chunked\r\n",
                    "\r\n",
                )
                .as_bytes(),
            )
            .await;
        let _ = writer
            .write_all(chunk("data: {\"symbol\":\"APOL\",\"amount\":1.5}\n\n").as_bytes())
            .await;
        tokio::time::sleep(delay).await;
        let _ = writer
            .write_all(chunk("data: {\"symbol\":\"APOL\",\"amount\":2.25}\n\n").as_bytes())
            .await;
        let _ = writer.write_all(b"0\r\n\r\n").await;
    });
    uri
}

#[tokio::test]
async fn the_source_timeout_bounds_the_response_headers_not_the_stream() {
    // The second event arrives well after the source's timeout has passed
    let uri = slow_sse_upstream(Duration::ZERO, Duration::from_millis(500)).await;

    let body = subscribe_with_timeout(&uri, "100ms").await;

    let amounts: Vec<serde_json::Value> = payloads(&body)
        .iter()
        .map(|payload| payload["payload"]["data"]["prices"]["amount"].clone())
        .collect();
    assert_eq!(amounts, [1.5, 2.25], "{body}");
}

#[tokio::test]
async fn the_source_timeout_applies_while_waiting_for_the_response_headers() {
    // The counterpart of the test above, showing that its timeout is in force
    let uri = slow_sse_upstream(Duration::from_millis(500), Duration::ZERO).await;

    let body = subscribe_with_timeout(&uri, "100ms").await;

    assert!(body.contains("GATEWAY_TIMEOUT"), "{body}");
    assert!(!body.contains("\"amount\""), "{body}");
}

/// Runs a `prices` subscription to completion against `uri` with a source `timeout`.
async fn subscribe_with_timeout(uri: &str, timeout: &str) -> String {
    let mut response = open_with_traffic_shaping(
        SCHEMA,
        uri,
        r#"subscription { prices(symbol: "APOL") { amount } }"#,
        None,
        serde_json::json!({
            "connector": { "sources": { "streams.market": { "timeout": timeout } } }
        }),
    )
    .await;
    drain(&mut response).await
}
