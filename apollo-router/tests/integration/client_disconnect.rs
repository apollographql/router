//! How the router records a request whose client disconnects or sends a malformed body, and that
//! a failing subgraph or coprocessor is never mistaken for either.
//!
//! These tests drive real HTTP/1 and HTTP/2 clients against the router binary and read the
//! statuses it records from its Prometheus endpoint.

use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;

use crate::integration::IntegrationTest;
use crate::integration::common::graph_os_enabled;

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
