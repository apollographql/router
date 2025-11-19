use http::StatusCode;
use hyper_util::rt::TokioExecutor;
use rustls::RootCertStore;
use tower::BoxError;

use crate::integration::IntegrationTest;

const SERVER_CERT: &str = include_str!("./fixtures/tls_localhost.crt");
const TLS_CONFIG: &str = include_str!("./fixtures/tls.router.yml");
const TLS_CONFIG_WITH_SMALL_H2_HEADER_LIMIT: &str =
    include_str!("./fixtures/tls.header_limited.router.yml");

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
