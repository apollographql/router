//! How the router records a request whose client disconnects or sends a malformed body, and that
//! a failing subgraph or coprocessor is never mistaken for either.
//!
//! These tests drive real HTTP/1 and HTTP/2 clients against the router binary and read the
//! statuses it records from its Prometheus endpoint.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tower::BoxError;
use wiremock::ResponseTemplate;

use crate::integration::IntegrationTest;
use crate::integration::common::graph_os_enabled;

const QUERY: &str = r#"{"query":"query { topProducts { name } }"}"#;

const PROMETHEUS: &str = r#"
telemetry:
  exporters:
    metrics:
      prometheus:
        enabled: true
        path: /metrics
  instrumentation:
    instruments:
      router:
        http.server.active_requests: true
"#;

fn config(early_cancel: bool) -> String {
    format!("{PROMETHEUS}\nsupergraph:\n  early_cancel: {early_cancel}\n")
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Client {
    Http1,
    Http2StreamReset,
    Http2ConnectionClose,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// The client leaves after sending part of the request body.
    BodyRead,
    /// The client leaves after sending the whole request, while the router waits for a subgraph.
    Handling,
}

/// A client connection with a request in flight. Dropping it closes the connection.
enum InFlight {
    Http1(#[allow(dead_code)] TcpStream),
    Http2 {
        body: h2::SendStream<bytes::Bytes>,
        _response: h2::client::ResponseFuture,
        connection: tokio::task::JoinHandle<()>,
    },
}

async fn send_request(address: SocketAddr, client: Client, phase: Phase) -> InFlight {
    // In the body-read phase the client promises more body than it sends.
    let content_length = match phase {
        Phase::BodyRead => QUERY.len() + 100,
        Phase::Handling => QUERY.len(),
    };
    let tcp = TcpStream::connect(address).await.unwrap();
    if client == Client::Http1 {
        let mut tcp = tcp;
        let head = format!(
            "POST / HTTP/1.1\r\nhost: {address}\r\ncontent-type: application/json\r\n\
             content-length: {content_length}\r\n\r\n{QUERY}"
        );
        tcp.write_all(head.as_bytes()).await.unwrap();
        return InFlight::Http1(tcp);
    }

    let (h2, connection) = h2::client::handshake(tcp).await.unwrap();
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut h2 = h2.ready().await.unwrap();
    let request = http::Request::post(format!("http://{address}/"))
        .header("content-type", "application/json")
        .header("content-length", content_length)
        .body(())
        .unwrap();
    let (response, mut body) = h2.send_request(request, false).unwrap();
    body.send_data(QUERY.into(), phase == Phase::Handling)
        .unwrap();
    InFlight::Http2 {
        body,
        _response: response,
        connection,
    }
}

impl InFlight {
    /// Disconnects the way `client` does, and returns whatever must stay alive until the router
    /// has reacted: an HTTP/2 stream reset leaves the connection itself open.
    fn disconnect(self, client: Client) -> Option<tokio::task::JoinHandle<()>> {
        match (self, client) {
            (
                InFlight::Http2 {
                    mut body,
                    connection,
                    ..
                },
                Client::Http2StreamReset,
            ) => {
                body.send_reset(h2::Reason::CANCEL);
                Some(connection)
            }
            (InFlight::Http2 { connection, .. }, _) => {
                connection.abort();
                None
            }
            (InFlight::Http1(_), _) => None,
        }
    }
}

async fn assert_disconnects_recorded_as_499(early_cancel: bool) {
    let (subgraph_calls, mut subgraph_called) = mpsc::unbounded_channel();
    let mut router = IntegrationTest::builder()
        .config(config(early_cancel))
        // Long enough for every client to leave while the router waits for the subgraph.
        .responder(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {"topProducts": [{"name": "Table"}]}}))
                .set_delay(Duration::from_secs(2)),
        )
        .subgraph_callback(Box::new(move || {
            let _ = subgraph_calls.send(());
        }))
        .build()
        .await;
    router.start().await;
    router.assert_started().await;
    let address = router.bind_address();

    let mut recorded = 0;
    for client in [
        Client::Http1,
        Client::Http2StreamReset,
        Client::Http2ConnectionClose,
    ] {
        for phase in [Phase::BodyRead, Phase::Handling] {
            let request = send_request(address, client, phase).await;
            match phase {
                // The router has started the request and is waiting for the rest of the body.
                Phase::BodyRead => {
                    router
                        .assert_metrics_contains(r#"http_server_active_requests{<any>} 1"#, None)
                        .await
                }
                Phase::Handling => subgraph_called.recv().await.unwrap(),
            }
            let keep_open = request.disconnect(client);

            recorded += 1;
            println!("{client:?} / {phase:?} / early_cancel: {early_cancel}");
            assert_status_count(&router, 499, recorded).await;
            router
                .assert_metrics_contains(r#"http_server_active_requests{<any>} 0"#, None)
                .await;
            drop(keep_open);
        }
    }
    router
        .assert_metrics_does_not_contain(r#"http_response_status_code="500""#)
        .await;
    assert_every_request_has_a_status(&router).await;
    router.graceful_shutdown().await;
}

/// A request recorded without a status would mean it was recorded twice: once when it was
/// dropped, and once with its status.
async fn assert_every_request_has_a_status(router: &IntegrationTest) {
    let metrics = router
        .get_metrics_response()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for line in metrics.lines().filter(|line| {
        line.starts_with("apollo_router_operations_total{")
            || line.starts_with("http_server_request_duration_seconds_count{")
    }) {
        assert!(
            line.contains("http_response_status_code="),
            "recorded without a status: {line}"
        );
    }
}

/// Asserts that `apollo.router.operations` and `http.server.request.duration` have each recorded
/// exactly `count` requests with `status`.
async fn assert_status_count(router: &IntegrationTest, status: u16, count: usize) {
    router
        .assert_metrics_contains(
            &format!(
                "apollo_router_operations_total{{http_response_status_code=\"{status}\",otel_scope_name=\"apollo/router\"}} {count}\n"
            ),
            None,
        )
        .await;
    router
        .assert_metrics_contains(
            &format!(
                "http_server_request_duration_seconds_count{{<any>http_response_status_code=\"{status}\"<any>}} {count}\n"
            ),
            None,
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disconnects_are_recorded_as_499_with_early_cancel() {
    assert_disconnects_recorded_as_499(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_disconnects_are_recorded_as_499_without_early_cancel() {
    assert_disconnects_recorded_as_499(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_request_body_is_recorded_as_400() {
    let mut router = IntegrationTest::builder().config(PROMETHEUS).build().await;
    router.start().await;
    router.assert_started().await;

    let mut tcp = TcpStream::connect(router.bind_address()).await.unwrap();
    tcp.write_all(
        b"POST / HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\n\
          transfer-encoding: chunked\r\n\r\nnot-a-chunk-size\r\n",
    )
    .await
    .unwrap();
    let mut response = String::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), tcp.read_to_string(&mut response)).await;
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "unexpected response: {response}"
    );

    assert_status_count(&router, 400, 1).await;
    router.graceful_shutdown().await;
}

/// h2 resets a stream whose body breaks the protocol, so no response can reach the client and
/// hyper drops the request, as it does when the client leaves.
#[tokio::test(flavor = "multi_thread")]
async fn an_http2_body_that_breaks_the_protocol_is_recorded_as_499() {
    let mut router = IntegrationTest::builder().config(PROMETHEUS).build().await;
    router.start().await;
    router.assert_started().await;
    let address = router.bind_address();

    let tcp = TcpStream::connect(address).await.unwrap();
    let (h2, connection) = h2::client::handshake(tcp).await.unwrap();
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut h2 = h2.ready().await.unwrap();
    // The body ends before the length its content-length promises.
    let request = http::Request::post(format!("http://{address}/"))
        .header("content-type", "application/json")
        .header("content-length", QUERY.len() + 100)
        .body(())
        .unwrap();
    let (response, mut body) = h2.send_request(request, false).unwrap();
    body.send_data(QUERY.into(), true).unwrap();
    let reset = response.await.expect_err("h2 resets the stream");
    assert_eq!(reset.reason(), Some(h2::Reason::PROTOCOL_ERROR));

    // hyper can drop the request before it reaches router telemetry, so only the outermost
    // count is certain to see it.
    router
        .assert_metrics_contains(
            r#"apollo_router_operations_total{http_response_status_code="499",otel_scope_name="apollo/router"} 1
"#,
            None,
        )
        .await;
    router
        .assert_metrics_does_not_contain(r#"http_response_status_code="400""#)
        .await;
    drop(connection);
    router.graceful_shutdown().await;
}

/// How a test upstream fails after reading a request.
#[derive(Clone, Copy)]
enum UpstreamFailure {
    /// Promises a longer body than it sends, then closes the connection.
    TruncatedBody,
    /// Resets the connection.
    Reset,
}

/// Starts an HTTP/1 server that fails every request in the same way, and returns its URL.
async fn failing_upstream(failure: UpstreamFailure) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                read_request(&mut tcp).await;
                match failure {
                    UpstreamFailure::TruncatedBody => {
                        let _ = tcp
                            .write_all(
                                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                                  content-length: 1000\r\n\r\n{\"data\"",
                            )
                            .await;
                    }
                    UpstreamFailure::Reset => {
                        let _ = tcp.set_zero_linger();
                    }
                }
            });
        }
    });
    format!("http://{address}/")
}

/// Reads one HTTP/1 request with a `content-length` body.
async fn read_request(tcp: &mut TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let Ok(read) = tcp.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
        if let Some(head_end) = text.find("\r\n\r\n") {
            let content_length = text
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|length| length.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= head_end + 4 + content_length {
                return;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_coprocessor_is_not_a_client_disconnect() {
    if !graph_os_enabled() {
        return;
    }
    for failure in [UpstreamFailure::TruncatedBody, UpstreamFailure::Reset] {
        let coprocessor = failing_upstream(failure).await;
        let mut router = IntegrationTest::builder()
            .config(format!(
                "{PROMETHEUS}\ncoprocessor:\n  url: \"{coprocessor}\"\n  router:\n    request:\n      body: true\n"
            ))
            .build()
            .await;
        router.start().await;
        router.assert_started().await;

        let (_, response) = router.execute_default_query().await;
        assert_eq!(response.status(), 500);
        assert_status_count(&router, 500, 1).await;
        router
            .assert_metrics_does_not_contain(r#"http_response_status_code="499""#)
            .await;
        router.graceful_shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_subgraph_is_not_a_client_disconnect() {
    for failure in [UpstreamFailure::TruncatedBody, UpstreamFailure::Reset] {
        let subgraph = failing_upstream(failure).await;
        let mut router = IntegrationTest::builder()
            .config(PROMETHEUS)
            .subgraph_overrides([("products".to_string(), subgraph)].into())
            .build()
            .await;
        router.start().await;
        router.assert_started().await;

        let (_, response) = router.execute_default_query().await;
        let status = response.status().as_u16();
        assert_ne!(status, 499);
        assert_status_count(&router, status, 1).await;
        router
            .assert_metrics_does_not_contain(r#"http_response_status_code="499""#)
            .await;
        router.graceful_shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_leaves_after_the_first_deferred_part_is_not_a_499() -> Result<(), BoxError> {
    // The reviews subgraph answers only once the test lets it, which is after the client left.
    let (release_reviews, mut reviews_released) = mpsc::channel::<()>(1);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let reviews = format!("http://{}/", listener.local_addr()?);
    tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        read_request(&mut tcp).await;
        reviews_released.recv().await;
        let body = r#"{"data":{"_entities":[{"reviews":[]}]}}"#;
        let _ = tcp
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await;
    });

    let mut router = IntegrationTest::builder()
        .config(PROMETHEUS)
        .responder(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"topProducts": [{"__typename": "Product", "upc": "1", "name": "Table"}]}
        })))
        .subgraph_overrides([("reviews".to_string(), reviews)].into())
        .build()
        .await;
    router.start().await;
    router.assert_started().await;

    let mut response = reqwest::Client::new()
        .post(format!("http://{}/", router.bind_address()))
        .header("content-type", "application/json")
        .header("accept", "multipart/mixed;deferSpec=20220824")
        .json(&json!({"query": "{ topProducts { name ... @defer { reviews { body } } } }"}))
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    let first_part = response.chunk().await?.expect("the first deferred part");
    assert!(String::from_utf8_lossy(&first_part).contains("Table"));
    drop(response);
    release_reviews.send(()).await?;

    assert_status_count(&router, 200, 1).await;
    router
        .assert_metrics_does_not_contain(r#"http_response_status_code="499""#)
        .await;
    router.graceful_shutdown().await;
    Ok(())
}
