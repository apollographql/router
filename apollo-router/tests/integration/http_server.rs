use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use http::StatusCode;
use hyper_util::rt::TokioExecutor;
use rstest::rstest;
use rustls::RootCertStore;
use tower::BoxError;

use crate::integration::IntegrationTest;

/// See [`apollo_router::services::http::tests::tls_self_signed`] for detail about how this was generated
/// and when it expires.
const SERVER_CERT: &str = include_str!("../../src/services/http/testdata/server_self_signed.crt");
const TLS_CONFIG: &str = include_str!("./fixtures/tls.router.yml");
const TLS_CONFIG_WITH_SMALL_H2_HEADER_LIMIT: &str =
    include_str!("./fixtures/tls.header_limited.router.yml");
const TCP_CONFIG_WITH_H2_HEADER_LIMIT: &str =
    include_str!("./fixtures/tcp.header_limited.router.yml");
#[cfg(unix)]
const UNIX_CONFIG_WITH_H2_HEADER_LIMIT: &str =
    include_str!("./fixtures/unix.header_limited.router.yml");

fn load_cert_to_root_store(cert_pem: &str) -> RootCertStore {
    let mut root_store = RootCertStore::empty();
    let cert = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .expect("valid cert");
    root_store.add(cert[0].clone()).expect("add cert");
    root_store
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_server_negotiates_http2_with_client() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder().config(TLS_CONFIG).build().await;

    router.start().await;
    router.assert_started().await;

    let https_addr = router.bind_address();

    let root_store = load_cert_to_root_store(SERVER_CERT);
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    // NOTE: both http1 and http2 are enabled
    let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    let client =
        hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https_connector);

    let uri: http::Uri = format!("https://localhost:{}/", https_addr.port()).parse()?;
    let request = http::Request::builder()
        .uri(uri)
        .method("POST")
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(bytes::Bytes::from(
            r#"{"query":"{ __typename }"}"#,
        )))?;

    let response = client.request(request).await?;

    assert_eq!(response.status(), StatusCode::OK);

    // http2 used!
    assert_eq!(
        response.version(),
        http::Version::HTTP_2,
        "Expected HTTP/2 to be negotiated"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_router_server_falls_back_to_http1_with_client() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder().config(TLS_CONFIG).build().await;

    router.start().await;
    router.assert_started().await;

    let https_addr = router.bind_address();

    let root_store = load_cert_to_root_store(SERVER_CERT);
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        // NOTE: only http1 enabled
        .enable_http1()
        .build();

    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        // NOTE: disabling http2!
        .http2_only(false)
        .build(https_connector);

    let uri: http::Uri = format!("https://localhost:{}/", https_addr.port()).parse()?;
    let request = http::Request::builder()
        .uri(uri)
        .method("POST")
        .header("content-type", "application/json")
        .body(http_body_util::Full::new(bytes::Bytes::from(
            r#"{"query":"{ __typename }"}"#,
        )))?;

    let response = client.request(request).await?;

    assert_eq!(response.status(), StatusCode::OK);

    assert_eq!(
        response.version(),
        // http1 used!
        http::Version::HTTP_11,
        "Expected HTTP/1.1 to be negotiated as fallback"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_http2_max_header_list_size_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(TLS_CONFIG_WITH_SMALL_H2_HEADER_LIMIT)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let https_addr = router.bind_address();

    let root_store = load_cert_to_root_store(SERVER_CERT);
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    let client =
        hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https_connector);

    let uri: http::Uri = format!("https://localhost:{}/", https_addr.port()).parse()?;

    // much bigger than the config's limit (20MiB)! this also tests that the hyper default (16kb)
    // is overridden
    let large_header_value = "x".repeat(21 * 1024 * 1024);

    let request = http::Request::builder()
        .uri(uri)
        .method("POST")
        .header("content-type", "application/json")
        .header("x-large-header", large_header_value)
        .body(http_body_util::Full::new(bytes::Bytes::from(
            r#"{"query":"{ __typename }"}"#,
        )))?;

    let response = client.request(request).await?;

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
        "Expected 431 Request Header Fields Too Large when header list exceeds 20MiB limit"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_http2_max_header_list_size_within_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(TLS_CONFIG_WITH_SMALL_H2_HEADER_LIMIT)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let https_addr = router.bind_address();

    let root_store = load_cert_to_root_store(SERVER_CERT);
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let https_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    let client =
        hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https_connector);

    let uri: http::Uri = format!("https://localhost:{}/", https_addr.port()).parse()?;

    // create a header value that stays within the 20MiB limit of the config
    let acceptable_header_value = "y".repeat(10 * 1024 * 1024);

    let request = http::Request::builder()
        .uri(uri)
        .method("POST")
        .header("content-type", "application/json")
        .header("x-medium-header", acceptable_header_value)
        .body(http_body_util::Full::new(bytes::Bytes::from(
            r#"{"query":"{ __typename }"}"#,
        )))?;

    let response = client.request(request).await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.version(),
        http::Version::HTTP_2,
        "Expected HTTP/2 to be negotiated"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_tcp_max_header_list_size_exceeded() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(TCP_CONFIG_WITH_H2_HEADER_LIMIT)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let tcp_addr = router.bind_address();

    // Create a custom connector for TCP
    let connector = tower::service_fn(move |_uri: http::Uri| {
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect(tcp_addr).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        })
    });

    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(connector);

    let uri: http::Uri = format!("http://{}/", tcp_addr).parse()?;

    // much bigger than the config's limit (20MiB)! this also tests that the hyper default (16kb)
    // is overridden
    let large_header_value = "x".repeat(21 * 1024 * 1024);

    let request = http::Request::builder()
        .uri(uri)
        .method("POST")
        .header("content-type", "application/json")
        .header("x-large-header", large_header_value)
        .body(http_body_util::Full::new(bytes::Bytes::from(
            r#"{"query":"{ __typename }"}"#,
        )))?;

    let response = client.request(request).await?;

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
        "Expected 431 Request Header Fields Too Large when header list exceeds 20MiB limit for TCP"
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_tcp_max_header_list_size_within_limit() -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(TCP_CONFIG_WITH_H2_HEADER_LIMIT)
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let tcp_addr = router.bind_address();

    // Create a custom connector for TCP
    let connector = tower::service_fn(move |_uri: http::Uri| {
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect(tcp_addr).await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        })
    });

    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(connector);

    let uri: http::Uri = format!("http://{}/", tcp_addr).parse()?;

    // create a header value that stays within the 20MiB limit of the config
    let acceptable_header_value = "y".repeat(10 * 1024 * 1024);

    let request = http::Request::builder()
        .uri(uri)
        .method("POST")
        .header("content-type", "application/json")
        .header("x-medium-header", acceptable_header_value)
        .body(http_body_util::Full::new(bytes::Bytes::from(
            r#"{"query":"{ __typename }"}"#,
        )))?;

    let response = client.request(request).await?;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Expected successful response when header list is within 20MiB limit for TCP"
    );
    assert_eq!(
        response.version(),
        http::Version::HTTP_2,
        "Expected HTTP/2 to be negotiated for TCP"
    );

    router.graceful_shutdown().await;
    Ok(())
}

enum HttpProtocol {
    Http1,
    Http2,
}

// both http1 and http2 have connection persistence by default; http1 uses keep-alive, but since
// http2 uses a single connection with multiple requests, the headers sent to intermediate servers
// can't be used as connection-specific headers because connections are no longer identifiable with
// a single request; so, for http2 connections default to persistently open and only close when
// explicitly closed by the client or server (via GOAWAY frames, eg)
//
// this happens as the default, so the tests below only test the persistence of connections rather
// than the explicit headers (for http1, eg) to make sure that we haven't broken anything or that
// there wasn't a regression in any of the libraries we use breaking something
#[tokio::test(flavor = "multi_thread")]
#[rstest]
#[case::http1_conn_persistence(HttpProtocol::Http1)]
#[case::http2_conn_persistence(HttpProtocol::Http2)]
async fn test_http1_connection_persistence(
    #[case] http_protocol: HttpProtocol,
) -> Result<(), BoxError> {
    let mut router = IntegrationTest::builder()
        .config(
            r#"
            supergraph:
              listen: localhost:80
            "#,
        )
        .build()
        .await;

    router.start().await;
    router.assert_started().await;

    let addr = router.bind_address();

    // using an Arc to count connections across async boundaries
    let connection_count = Arc::new(AtomicUsize::new(0));
    let connection_count_clone = connection_count.clone();

    let connector = tower::service_fn(move |uri: http::Uri| {
        let connection_count = connection_count_clone.clone();
        Box::pin(async move {
            // Increment connection counter each time a new connection is established
            connection_count.fetch_add(1, Ordering::SeqCst);
            let stream = tokio::net::TcpStream::connect(format!(
                "{}:{}",
                uri.host().unwrap_or("localhost"),
                uri.port_u16().unwrap_or(80)
            ))
            .await?;
            Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
        })
    });

    let is_http2 = matches!(http_protocol, HttpProtocol::Http2);
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .http2_only(is_http2)
        .build(connector);

    let uri: http::Uri = format!("http://{}/", addr).parse()?;

    // same client, multiple requests
    let num_requests = 5;
    for i in 0..num_requests {
        let request = http::Request::builder()
            .uri(uri.clone())
            .method("POST")
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(bytes::Bytes::from(
                r#"{"query":"{ __typename }"}"#,
            )))?;

        let response = client.request(request).await?;

        // keep-alive is the default; so, the header might not be there, but we only care if the
        // connection remains open (ie, doesn't contain 'close')
        let connection_header = response.headers().get(http::header::CONNECTION);
        if let Some(value) = connection_header {
            let value_str = value.to_str().unwrap_or("");
            assert!(
                !value_str.contains("close"),
                "Connection should not be closed, got: {} on request {}",
                value_str,
                i + 1
            );
        }
    }

    // this is the core thing to check for keep-alive: that the number of connections is fewer than
    // the number of requests, showing re-use
    let total_connections = connection_count.load(Ordering::SeqCst);
    assert!(
        total_connections < num_requests,
        "Expected connection reuse: {} connections should be less than {} requests",
        total_connections,
        num_requests
    );

    router.graceful_shutdown().await;
    Ok(())
}

#[cfg(unix)]
mod unix_tests {
    use hyper_util::rt::TokioIo;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    #[rstest]
    #[case::header_within_limits_of_config(UNIX_CONFIG_WITH_H2_HEADER_LIMIT, "y".repeat(10*1024*1024), StatusCode::OK)]
    #[case::header_bigger_than_config(UNIX_CONFIG_WITH_H2_HEADER_LIMIT, "n".repeat(21*1024*1024), StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE)]
    async fn test_unix_socket_max_header_list_size(
        #[case] config: &str,
        #[case] header: String,
        #[case] status_code: StatusCode,
    ) -> Result<(), BoxError> {
        use uuid::Uuid;

        // generate a unique socket path to avoid conflicts
        let uuid = Uuid::new_v4().simple().to_string();
        let socket_path = format!("/tmp/apollo_router_test_{}.sock", uuid);
        let config = config.replace("{{RANDOM}}", &uuid);

        let mut router = IntegrationTest::builder().config(&config).build().await;

        router.start().await;
        router.assert_started().await;

        // connect directly to the Unix socket using HTTP/2
        let stream = tokio::net::UnixStream::connect(&socket_path).await?;
        let (mut sender, conn) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await?;

        tokio::task::spawn(async move {
            if let Err(err) = conn.await {
                eprintln!("Connection failed: {err:?}");
            }
        });

        let request = http::Request::builder()
            .uri("http://localhost/")
            .method("POST")
            .header("content-type", "application/json")
            .header("x-target-header", header)
            .body(http_body_util::Full::new(bytes::Bytes::from(
                r#"{"query":"{ __typename }"}"#,
            )))?;

        let response = sender.send_request(request).await?;

        assert_eq!(
            response.status(),
            status_code,
            "Expected status code {:?} for Unix socket with header size test",
            status_code
        );
        assert_eq!(
            response.version(),
            http::Version::HTTP_2,
            "Expected HTTP/2 to be negotiated for Unix socket"
        );

        router.graceful_shutdown().await;

        // clean up the socket file
        let _ = std::fs::remove_file(&socket_path);

        Ok(())
    }
}
