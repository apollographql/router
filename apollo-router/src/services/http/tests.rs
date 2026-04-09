use std::convert::Infallible;
use std::io;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use async_compression::tokio::write::GzipDecoder;
use async_compression::tokio::write::GzipEncoder;
use axum::body::Body;
use http::Request;
use http::StatusCode;
use http::Uri;
use http::Version;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use hyper::body::Incoming;
use hyper_rustls::ConfigBuilderExt;
use hyper_util::rt::TokioExecutor;
use hyper_util::rt::TokioIo;
use mime::APPLICATION_JSON;
use rstest::rstest;
use rustls::RootCertStore;
use rustls::ServerConfig;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::PrivateKeyDer;
use rustls::server::WebPkiClientVerifier;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio_rustls::TlsAcceptor;
use tower::BoxError;
use tower::ServiceExt;

use crate::Configuration;
use crate::Context;
use crate::TestHarness;
use crate::configuration::TlsClient;
use crate::configuration::TlsClientAuth;
use crate::configuration::load_certs;
use crate::configuration::load_key;
use crate::graphql::Response;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::traffic_shaping::Http2Config;
use crate::services::http::HttpClientService;
use crate::services::http::HttpRequest;
use crate::services::http::HttpResponse;
use crate::services::router;
use crate::services::supergraph;

/// Whether we are testing the subgraph or connector TLS/HTTP code path
#[derive(Debug, Clone, Copy)]
enum ServiceKind {
    Subgraph,
    Connector,
}

/// Insert a [`TlsClient`] into the right slot in the configuration
fn insert_tls_config(config: &mut Configuration, kind: ServiceKind, tls: TlsClient) {
    match kind {
        ServiceKind::Subgraph => {
            config.tls.subgraph.subgraphs.insert("test".into(), tls);
        }
        ServiceKind::Connector => {
            config.tls.connector.sources.insert("test".into(), tls);
        }
    }
}

/// Build an [`HttpClientService`] using the factory matching `kind`
fn make_service(
    kind: ServiceKind,
    config: &Configuration,
    client: crate::configuration::shared::Client,
) -> HttpClientService {
    let root_store = &rustls::RootCertStore::empty();
    match kind {
        ServiceKind::Subgraph => {
            HttpClientService::from_config_for_subgraph("test", config, root_store, client)
        }
        ServiceKind::Connector => {
            HttpClientService::from_config_for_connector("test", config, root_store, client)
        }
    }
    .unwrap()
}

/// Send a JSON request through the service and return the response
async fn send_request(service: HttpClientService, uri: Uri, body: &'static str) -> HttpResponse {
    service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(uri)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(body))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap()
}

/// Assert the response is 200 OK with the expected body bytes
async fn assert_response_body(response: HttpResponse, expected: &str) {
    let (parts, body) = response.http_response.into_parts();
    assert_eq!(parts.status, StatusCode::OK);
    let bytes = router::body::into_bytes(body).await.unwrap();
    assert_eq!(std::str::from_utf8(&bytes).unwrap(), expected);
}

/// ALPN consts
mod alpn {
    pub(super) const H2: &[u8] = b"h2";
    pub(super) const HTTP_1_1: &[u8] = b"http/1.1";
}

#[derive(Debug, Clone, PartialEq)]
enum NegotiatedHttpProtocol {
    HTTP1,
    HTTP2,
}

impl TryFrom<Vec<u8>> for NegotiatedHttpProtocol {
    type Error = String;
    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        if value == alpn::HTTP_1_1 {
            Ok(NegotiatedHttpProtocol::HTTP1)
        } else if value == alpn::H2 {
            Ok(NegotiatedHttpProtocol::HTTP2)
        } else {
            Err(format!("{value:?} is not a supported protocol"))
        }
    }
}

/// Tracks which protocol was negotiated across different execution contexts
/// (i.e. in the test server actually handling the negotiation).
#[derive(Debug, Clone)]
struct NegotiatedProtocolTracker {
    protocol: Arc<Mutex<Option<NegotiatedHttpProtocol>>>,
}

impl NegotiatedProtocolTracker {
    fn new() -> Self {
        Self {
            protocol: Arc::new(Mutex::new(None)),
        }
    }

    fn set(&self, protocol: NegotiatedHttpProtocol) {
        *self.protocol.lock().unwrap() = Some(protocol);
    }

    fn get(&self) -> Option<NegotiatedHttpProtocol> {
        self.protocol.lock().unwrap().clone()
    }

    fn is_http2(&self) -> bool {
        self.get()
            .is_some_and(|p| p == NegotiatedHttpProtocol::HTTP2)
    }

    fn is_http1(&self) -> bool {
        self.get()
            .is_some_and(|p| p == NegotiatedHttpProtocol::HTTP1)
    }

    /// Note: `None` may mean "not yet processed" rather than "no ALPN".
    fn is_unnegotiated(&self) -> bool {
        self.get().is_none()
    }
}

/// What we expect the ALPN negotiation to have settled on.
#[derive(Debug, Clone)]
enum ExpectedAlpn {
    Http2,
    Http1,
    Http1OrNone,
}

impl ExpectedAlpn {
    fn check(&self, tracker: &NegotiatedProtocolTracker) {
        match self {
            ExpectedAlpn::Http2 => assert!(
                tracker.is_http2(),
                "expected h2 ALPN negotiation, got: {:?}",
                tracker.get()
            ),
            ExpectedAlpn::Http1 => assert!(
                tracker.is_http1(),
                "expected http/1.1 ALPN negotiation, got: {:?}",
                tracker.get()
            ),
            ExpectedAlpn::Http1OrNone => assert!(
                tracker.is_http1() || tracker.is_unnegotiated(),
                "expected http/1.1 or no ALPN, got: {:?}",
                tracker.get()
            ),
        }
    }
}

/// Test server for TLS
async fn tls_server(
    listener: TcpListener,
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    body: &'static str,
    negotiated_protocol_tracker: NegotiatedProtocolTracker,
    alpn_protocols: Vec<Vec<u8>>,
) {
    let mut tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .expect("built our tls config");

    tls_config.alpn_protocols = alpn_protocols;

    let tls_config = Arc::new(tls_config);
    let acceptor = TlsAcceptor::from(tls_config);

    // loop on accepting incoming connections
    loop {
        let (stream, _) = listener.accept().await.expect("accepting connections");
        let acceptor = acceptor.clone();

        let negotiated_protocol = negotiated_protocol_tracker.clone();
        tokio::spawn(async move {
            let acceptor_stream = acceptor.accept(stream).await.expect("accepted stream");
            if let Some(protocol) = acceptor_stream.get_ref().1.alpn_protocol() {
                negotiated_protocol.set(protocol.to_vec().try_into().unwrap());
            }

            let tokio_stream = TokioIo::new(acceptor_stream);

            // Auto-detect the protocol (mirrors the builder used in listeners.rs).
            let mut builder = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());

            if negotiated_protocol.is_http2() {
                builder = builder.http2_only();
            } else if negotiated_protocol.is_http1() {
                builder = builder.http1_only();
            }

            let hyper_service =
                hyper::service::service_fn(move |_request: Request<Incoming>| async {
                    Ok::<_, io::Error>(
                        http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body::<Body>(body.into())
                            .unwrap(),
                    )
                });

            if let Err(err) = builder
                .serve_connection_with_upgrades(tokio_stream, hyper_service)
                .await
            {
                eprintln!("failed to serve connection: {err:#}");
            }
        });
    }
}

/// Test server for TLS with client auth
async fn tls_server_with_client_auth(
    listener: TcpListener,
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    client_root: CertificateDer<'static>,
    body: &'static str,
) {
    let mut client_auth_roots = RootCertStore::empty();
    client_auth_roots.add(client_root).unwrap();

    let client_auth = WebPkiClientVerifier::builder(Arc::new(client_auth_roots))
        .build()
        .unwrap();

    let tls_config = Arc::new(
        ServerConfig::builder()
            .with_client_cert_verifier(client_auth)
            .with_single_cert(certificates, key)
            .unwrap(),
    );
    let acceptor = TlsAcceptor::from(tls_config);

    // loop on accepting incoming connections
    loop {
        let (stream, _) = listener.accept().await.expect("accepting connections");
        let acceptor = acceptor.clone();

        tokio::spawn(async move {
            let acceptor_stream = acceptor.accept(stream).await.expect("accepted stream");
            let tokio_stream = TokioIo::new(acceptor_stream);

            let hyper_service =
                hyper::service::service_fn(move |_request: Request<Incoming>| async {
                    Ok::<_, io::Error>(
                        http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .version(Version::HTTP_11)
                            .body::<Body>(body.into())
                            .unwrap(),
                    )
                });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(tokio_stream, hyper_service)
                .await
            {
                eprintln!("failed to serve connection: {err:#}");
            }
        });
    }
}

/// Plaintext test server
pub(crate) async fn serve<Handler, Fut>(
    listener: TcpListener,
    handle: Handler,
) -> std::io::Result<()>
where
    Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
    Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
{
    // loop on accepting incoming connections
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handle = handle.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(|request: Request<Incoming>| {
                handle(request.map(Body::new))
            });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("server error: {err}");
            }
        });
    }
}

/// Unix test server
#[cfg(unix)]
async fn serve_unix<Handler, Fut>(
    listener: UnixListener,
    handle: Handler,
    version_tracker: Option<HttpVersionTracker>,
) -> std::io::Result<()>
where
    Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
    Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
{
    // loop on accepting incoming connections
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handle = handle.clone();
        let version_tracker = version_tracker.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(|request: Request<Incoming>| {
                if let Some(ref tracker) = version_tracker {
                    tracker.set(request.version());
                }
                handle(request.map(Body::new))
            });
            if let Err(err) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, svc)
                .await
            {
                eprintln!("server error: {err}");
            }
        });
    }
}

/// Tracks the HTTP version used in requests (HTTP/1.1 vs HTTP/2)
#[cfg(unix)]
#[derive(Debug, Clone)]
struct HttpVersionTracker {
    version: Arc<Mutex<Option<Version>>>,
}

#[cfg(unix)]
impl HttpVersionTracker {
    fn new() -> Self {
        Self {
            version: Arc::new(Mutex::new(None)),
        }
    }

    fn set(&self, version: Version) {
        *self.version.lock().unwrap() = Some(version);
    }

    fn get(&self) -> Option<Version> {
        *self.version.lock().unwrap()
    }
}

#[cfg(unix)]
#[derive(Debug, Clone)]
enum ExpectedVersion {
    Http2,
    Http11,
    Any,
}

#[cfg(unix)]
impl ExpectedVersion {
    fn check(&self, tracker: &HttpVersionTracker) {
        let got = tracker.get();
        match self {
            ExpectedVersion::Http2 => assert!(
                got == Some(Version::HTTP_2),
                "expected HTTP/2, got: {got:?}"
            ),
            ExpectedVersion::Http11 => assert!(
                got == Some(Version::HTTP_11),
                "expected HTTP/1.1, got: {got:?}"
            ),
            ExpectedVersion::Any => assert!(got.is_some(), "expected a version to be captured"),
        }
    }
}

mod tls {
    // Note: The TLS tests rely on checked-in certificates valid until Oct 7 2033.
    // If tests fail after that date, regenerate in the testdata directory:
    //   openssl x509 -req -in server_self_signed.csr -signkey server.key \
    //     -out server_self_signed.crt -extfile server.ext -days 3650
    use super::*;

    #[rstest]
    #[case::subgraph(ServiceKind::Subgraph)]
    #[case::connector(ServiceKind::Connector)]
    #[tokio::test(flavor = "multi_thread")]
    async fn tls_self_signed(#[case] kind: ServiceKind) {
        let certificate_pem = include_str!("./testdata/server_self_signed.crt");
        let key_pem = include_str!("./testdata/server.key");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let negotiated_protocol = NegotiatedProtocolTracker::new();

        tokio::task::spawn(tls_server(
            listener,
            load_certs(certificate_pem).unwrap(),
            load_key(key_pem).unwrap(),
            r#"{"data": null}"#,
            negotiated_protocol.clone(),
            vec![alpn::H2.to_vec(), alpn::HTTP_1_1.to_vec()],
        ));

        let mut config = Configuration::default();
        insert_tls_config(
            &mut config,
            kind,
            TlsClient {
                certificate_authorities: Some(certificate_pem.into()),
                client_authentication: None,
            },
        );
        let service = make_service(kind, &config, Default::default());

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
        let response = send_request(service, url, r#"{"query":"{ test }"}"#).await;
        assert_response_body(response, r#"{"data": null}"#).await;
    }

    #[rstest]
    #[case::subgraph(ServiceKind::Subgraph)]
    #[case::connector(ServiceKind::Connector)]
    #[tokio::test(flavor = "multi_thread")]
    async fn tls_custom_root(#[case] kind: ServiceKind) {
        let certificate_pem = include_str!("./testdata/server.crt");
        let ca_pem = include_str!("./testdata/CA/ca.crt");
        let key_pem = include_str!("./testdata/server.key");

        let mut certificates = load_certs(certificate_pem).unwrap();
        certificates.extend(load_certs(ca_pem).unwrap());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let negotiated_protocol = NegotiatedProtocolTracker::new();

        tokio::task::spawn(tls_server(
            listener,
            certificates,
            load_key(key_pem).unwrap(),
            r#"{"data": null}"#,
            negotiated_protocol.clone(),
            vec![alpn::H2.to_vec(), alpn::HTTP_1_1.to_vec()],
        ));

        let mut config = Configuration::default();
        insert_tls_config(
            &mut config,
            kind,
            TlsClient {
                certificate_authorities: Some(ca_pem.into()),
                client_authentication: None,
            },
        );
        let service = make_service(kind, &config, Default::default());

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
        let response = send_request(service, url, r#"{"query":"{ test }"}"#).await;
        assert_response_body(response, r#"{"data": null}"#).await;
    }

    #[rstest]
    #[case::subgraph(ServiceKind::Subgraph)]
    #[case::connector(ServiceKind::Connector)]
    #[tokio::test(flavor = "multi_thread")]
    async fn tls_client_auth(#[case] kind: ServiceKind) {
        let server_certificate_pem = include_str!("./testdata/server.crt");
        let ca_pem = include_str!("./testdata/CA/ca.crt");
        let server_key_pem = include_str!("./testdata/server.key");

        let mut server_certificates = load_certs(server_certificate_pem).unwrap();
        let ca_certificate = load_certs(ca_pem).unwrap().remove(0);
        server_certificates.push(ca_certificate.clone());

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();

        tokio::task::spawn(tls_server_with_client_auth(
            listener,
            server_certificates,
            load_key(server_key_pem).unwrap(),
            ca_certificate,
            r#"{"data": null}"#,
        ));

        let client_certificate_pem = include_str!("./testdata/client.crt");
        let client_key_pem = include_str!("./testdata/client.key");

        let mut config = Configuration::default();
        insert_tls_config(
            &mut config,
            kind,
            TlsClient {
                certificate_authorities: Some(ca_pem.into()),
                client_authentication: Some(Arc::new(TlsClientAuth {
                    certificate_chain: load_certs(client_certificate_pem).unwrap(),
                    key: load_key(client_key_pem).unwrap(),
                })),
            },
        );
        let service = make_service(kind, &config, Default::default());

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
        let response = send_request(service, url, r#"{"query":"{ test }"}"#).await;
        assert_response_body(response, r#"{"data": null}"#).await;
    }
}

mod h2c_cleartext {
    use super::*;
    use crate::configuration::shared::Client;

    // Starts a local server that responds with a default GraphQL response over plain HTTP.
    async fn emulate_h2c_server(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let response_builder =
                http::Response::builder().header(CONTENT_TYPE, APPLICATION_JSON.essence_str());

            let response = match request.version() {
                Version::HTTP_2 => {
                    let response_body = serde_json::to_string(&Response {
                        data: Some(Value::default()),
                        ..Response::default()
                    });
                    response_builder
                        .status(StatusCode::OK)
                        .body(response_body.unwrap().into())
                }
                Version::HTTP_11 => response_builder
                    .status(StatusCode::HTTP_VERSION_NOT_SUPPORTED)
                    .body(Body::empty()),
                version => panic!("unexpected version {version:?}"),
            };

            Ok(response.unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_h2c_works_with_http2only() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_h2c_server(listener));

        let client_config = Client::builder()
            .experimental_http2(Http2Config::Http2Only)
            .build();
        let subgraph_service =
            HttpClientService::from_client_config(client_config).expect("can create a HttpService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = send_request(
            subgraph_service,
            url,
            r#"{"query":"{ me { name username } }"#,
        )
        .await;

        assert_response_body(response, r#"{"data":null}"#).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_subgraph_h2c_not_used_with_enable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_h2c_server(listener));

        let client_config = Client::builder()
            .experimental_http2(Http2Config::Enable)
            .build();
        let subgraph_service =
            HttpClientService::from_client_config(client_config).expect("can create a HttpService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = send_request(
            subgraph_service,
            url,
            r#"{"query":"{ me { name username } }"#,
        )
        .await;

        // h2c only works with `Http2Config::Http2Only` - hyper only supports HTTP/2 with TLS or
        // with 'prior knowledge'
        // https://github.com/hyperium/hyper/issues/2411
        assert_eq!(
            response.http_response.status(),
            StatusCode::HTTP_VERSION_NOT_SUPPORTED
        );
    }
}

mod h2c_keep_alive {
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;
    use std::time::Duration;

    use tokio::io::AsyncRead;
    use tokio::io::AsyncWrite;
    use tokio::io::ReadBuf;
    use tokio::net::TcpStream;
    use tokio::sync::mpsc::Sender;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::configuration::shared::Client;

    /// Wraps a TcpStream and emits a `()` on the `ping_tx` channel each time the server reads an H2
    /// PING frame sent by the client.
    struct SpyStream {
        inner: TcpStream,
        ping_tx: Sender<()>,
    }

    impl SpyStream {
        fn new(stream: TcpStream, ping_tx: Sender<()>) -> Self {
            Self {
                inner: stream,
                ping_tx,
            }
        }
    }

    impl AsyncRead for SpyStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let before = buf.filled().len();

            let result = Pin::new(&mut self.inner).poll_read(cx, buf);
            if matches!(result, Poll::Ready(Ok(()))) {
                let new_bytes = buf.filled()[before..].to_vec();
                if is_ping_frame(&new_bytes) {
                    self.ping_tx.try_send(()).unwrap();
                }
            }

            result
        }
    }

    impl AsyncWrite for SpyStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// Check whether a raw byte slice is an H2 PING frame (not an ACK) per RFC 7540.
    ///
    /// This assumes the PING frame arrives as its own chunk — it inspects fixed offsets 3 and 4
    /// for the frame type and flags rather than doing full frame parsing. In practice this works
    /// because hyper sends keep-alive PINGs as standalone writes.
    ///
    /// References: [frame header] (§4.1), [PING frame] (§6.7)
    ///
    /// [frame header]: https://datatracker.ietf.org/doc/html/rfc7540#section-4.1
    /// [PING frame]: https://datatracker.ietf.org/doc/html/rfc7540#section-6.7
    fn is_ping_frame(data: &[u8]) -> bool {
        const FRAME_HEADER_SIZE: usize = 9;
        const PING_FRAME_TYPE: u8 = 0x06;
        const ACK_FLAG: u8 = 0x01;

        if data.len() < FRAME_HEADER_SIZE {
            return false;
        }

        let frame_type = data[3];
        let flags = data[4];

        frame_type == PING_FRAME_TYPE && flags & ACK_FLAG == 0
    }

    /// Start a spy H2 server that counts H2 PING frames sent by the client. Returns a
    /// [`JoinHandle`] that resolves to the total ping count once the server connection closes.
    fn start_spy_server_and_ping_counter(listener: TcpListener) -> JoinHandle<usize> {
        let (ping_tx, mut ping_rx) = tokio::sync::mpsc::channel(100);

        let _spy_server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let spy_stream = SpyStream::new(stream, ping_tx);

            let svc = hyper::service::service_fn(|_request: Request<Incoming>| async {
                let response_body = serde_json::to_string(&Response {
                    data: Some(Value::default()),
                    ..Response::default()
                })
                .unwrap();
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                        .status(StatusCode::OK)
                        .body(Body::from(response_body))
                        .unwrap(),
                )
            });

            // serve_connection drives the h2 connection, including automatic PING ACKs
            let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(spy_stream), svc)
                .await;
        });

        tokio::spawn(async move {
            let mut ping_count = 0;
            while let Some(()) = ping_rx.recv().await {
                ping_count += 1;
            }
            ping_count
        })
    }

    /// Start a spy server, make one request with the given client config, wait for keep-alive
    /// intervals to fire, then return the number of H2 PING frames the server observed.
    async fn run_server_and_count_keep_alive_pings(
        client_config: Client,
    ) -> Result<usize, BoxError> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let socket_addr = listener.local_addr()?;

        let ping_counter = start_spy_server_and_ping_counter(listener);
        let service = HttpClientService::from_client_config(client_config)?;

        let url = Uri::from_str(&format!("http://{socket_addr}"))?;
        let request = HttpRequest {
            http_request: Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(r#"{"query":"{ me }"}"#))?,
            context: crate::Context::new(),
        };

        // Clone so the service (and its connection pool) stays alive after the request
        service.clone().oneshot(request).await?;

        // Wait for several keep-alive intervals while the connection sits idle in the pool
        tokio::time::sleep(Duration::from_millis(500)).await;
        drop(service);

        Ok(ping_counter.await?)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_keep_alive_pings_are_sent() {
        let client_config = Client::builder()
            .experimental_http2(Http2Config::Http2Only)
            .experimental_http2_keep_alive_interval(Duration::from_millis(50))
            .build();

        let ping_count = run_server_and_count_keep_alive_pings(client_config)
            .await
            .unwrap();
        assert!(
            ping_count > 0,
            "expected at least one H2 PING frame from the client, got {ping_count}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_no_pings_without_keep_alive() {
        let client_config = Client::builder()
            .experimental_http2(Http2Config::Http2Only)
            // no keep-alive interval configured
            .build();

        let ping_count = run_server_and_count_keep_alive_pings(client_config)
            .await
            .unwrap();
        assert_eq!(
            ping_count, 0,
            "expected no H2 PING frames when keep-alive is not configured"
        );
    }
}

mod compressed_req_res {
    use super::*;

    async fn emulate_subgraph_compressed_response(listener: TcpListener) {
        async fn handle(request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let body = router::body::into_bytes(request.into_body())
                .await
                .unwrap()
                .to_vec();
            let mut decoder = GzipDecoder::new(Vec::new());
            decoder.write_all(&body).await.unwrap();
            decoder.shutdown().await.unwrap();
            let body = decoder.into_inner();
            assert_eq!(
                r#"{"query":"{ me { name username } }"#,
                std::str::from_utf8(&body).unwrap()
            );

            let original_body = Response {
                data: Some(Value::String(ByteString::from("test"))),
                ..Response::default()
            };
            let mut encoder = GzipEncoder::new(Vec::new());
            encoder
                .write_all(&serde_json::to_vec(&original_body).unwrap())
                .await
                .unwrap();
            encoder.shutdown().await.unwrap();
            let compressed_body = encoder.into_inner();

            Ok(http::Response::builder()
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header(CONTENT_ENCODING, "gzip")
                .status(StatusCode::OK)
                .body(compressed_body.into())
                .unwrap())
        }

        serve(listener, handle).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_compressed_request_response_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        tokio::task::spawn(emulate_subgraph_compressed_response(listener));

        let subgraph_service = HttpClientService::test_new(
            "test",
            rustls::ClientConfig::builder()
                .with_native_roots()
                .expect("read native TLS root certificates")
                .with_no_client_auth(),
            crate::configuration::shared::Client::builder()
                .experimental_http2(Http2Config::Http2Only)
                .build(),
        )
        .expect("can create a HttpService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
        let response = subgraph_service
            .oneshot(HttpRequest {
                http_request: http::Request::builder()
                    .uri(url)
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .header(CONTENT_ENCODING, "gzip")
                    .body(router::body::from_bytes(
                        r#"{"query":"{ me { name username } }"#,
                    ))
                    .unwrap(),
                context: Context::new(),
            })
            .await
            .unwrap();

        assert_response_body(response, r#"{"data":"test"}"#).await;
    }
}

mod plugin_loading {
    use super::*;

    const SCHEMA: &str = include_str!("../../testdata/orga_supergraph.graphql");

    struct TestPlugin {
        started: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl PluginPrivate for TestPlugin {
        type Config = ();

        fn http_client_service(
            &self,
            _subgraph_name: &str,
            service: crate::services::http::BoxCloneService,
        ) -> crate::services::http::BoxCloneService {
            self.started.store(true, Ordering::Release);
            service
        }

        // `new` is never called – the plugin is injected directly via `extra_private_plugin`.
        async fn new(_init: PluginInit<Self::Config>) -> Result<Self, BoxError> {
            Err("error".to_string().into())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_http_plugin_is_loaded() {
        let started = Arc::new(AtomicBool::new(false));

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(SCHEMA)
            .extra_private_plugin(TestPlugin {
                started: started.clone(),
            })
            .with_subgraph_network_requests()
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(r#"query { currentUser { id } }"#)
            .build()
            .unwrap();
        let _response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();

        assert!(started.load(Ordering::Acquire));
    }
}

mod unix_sockets {
    use super::*;

    fn make_schema(path: &str) -> String {
        format!(
            r#"schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
         {{
        query: Query
   }}
   directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
   directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
   directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
   directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION

   scalar link__Import

   enum link__Purpose {{
    """
    `SECURITY` features provide metadata necessary to securely resolve fields.
    """
    SECURITY

    """
    `EXECUTION` features provide metadata necessary for operation execution.
    """
    EXECUTION
  }}

   scalar join__FieldSet
   enum join__Graph {{
       USER @join__graph(name: "user", url: "unix://{path}")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }}
   type Query
   @join__type(graph: ORGA)
   @join__type(graph: USER)
   {{
       currentUser: User @join__field(graph: USER)
   }}

   type User
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){{
       id: ID!
       name: String
   }}"#
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(target_os = "windows"))]
    async fn test_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("router.sock");
        let schema = make_schema(path.to_str().unwrap());

        async fn handle(mut req: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let _data = router::body::into_bytes(req.body_mut()).await.unwrap();
            Ok(http::Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{ "data": { "currentUser": { "id": "0" } } }"#,
                ))
                .unwrap())
        }

        let listener = UnixListener::bind(path).unwrap();
        tokio::task::spawn(serve_unix(listener, handle, None));

        let service = TestHarness::builder()
            .configuration_json(serde_json::json!({"include_subgraph_errors": { "all": true } }))
            .unwrap()
            .schema(&schema)
            .with_subgraph_network_requests()
            .build_supergraph()
            .await
            .unwrap();

        let request = supergraph::Request::fake_builder()
            .query(r#"query { currentUser { id } }"#)
            .build()
            .unwrap();
        let response = service
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap();
        insta::assert_json_snapshot!(response);
    }
}

mod alpn_negotiation {
    use super::*;

    #[rstest]
    #[case::both_support_h2_subgraph(
    vec![alpn::H2.to_vec(), alpn::HTTP_1_1.to_vec()],
    Http2Config::Enable,
    ServiceKind::Subgraph,
    ExpectedAlpn::Http2,
)]
    #[case::server_only_http1_subgraph(
    vec![alpn::HTTP_1_1.to_vec()],
    Http2Config::Enable,
    ServiceKind::Subgraph,
    ExpectedAlpn::Http1,
)]
    #[case::client_disables_h2_subgraph(
    vec![alpn::HTTP_1_1.to_vec()],
    Http2Config::Disable,
    ServiceKind::Subgraph,
    ExpectedAlpn::Http1OrNone,
)]
    #[case::both_support_h2_connector(
    vec![alpn::H2.to_vec(), alpn::HTTP_1_1.to_vec()],
    Http2Config::Enable,
    ServiceKind::Connector,
    ExpectedAlpn::Http2,
)]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_alpn_negotiation(
        #[case] server_alpn: Vec<Vec<u8>>,
        #[case] http2_config: Http2Config,
        #[case] kind: ServiceKind,
        #[case] expected: ExpectedAlpn,
    ) {
        let certificate_pem = include_str!("./testdata/server_self_signed.crt");
        let key_pem = include_str!("./testdata/server.key");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let negotiated_protocol = NegotiatedProtocolTracker::new();

        tokio::task::spawn(tls_server(
            listener,
            load_certs(certificate_pem).unwrap(),
            load_key(key_pem).unwrap(),
            r#"{"data": null}"#,
            negotiated_protocol.clone(),
            server_alpn,
        ));

        let mut config = Configuration::default();
        insert_tls_config(
            &mut config,
            kind,
            TlsClient {
                certificate_authorities: Some(certificate_pem.into()),
                client_authentication: None,
            },
        );
        let service = make_service(
            kind,
            &config,
            crate::configuration::shared::Client::builder()
                .experimental_http2(http2_config)
                .build(),
        );

        let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
        let response = send_request(service, url, r#"{"query":"{ test }"}"#).await;

        expected.check(&negotiated_protocol);
        assert_response_body(response, r#"{"data": null}"#).await;
    }
}

mod http_version_negotiation {
    use super::*;

    #[cfg(unix)]
    #[rstest]
    #[case::http2_only(Http2Config::Http2Only, ExpectedVersion::Http2)]
    #[case::http2_enabled(Http2Config::Enable, ExpectedVersion::Any)]
    #[case::http2_disabled(Http2Config::Disable, ExpectedVersion::Http11)]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_unix_socket_http_version(
        #[case] http2_config: Http2Config,
        #[case] expected: ExpectedVersion,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");

        let version_tracker = HttpVersionTracker::new();

        async fn handle(mut req: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
            let _data = router::body::into_bytes(req.body_mut()).await.unwrap();
            Ok(http::Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(Body::from(r#"{"data": "success"}"#))
                .unwrap())
        }

        let listener = UnixListener::bind(&path).unwrap();
        tokio::task::spawn(serve_unix(listener, handle, Some(version_tracker.clone())));

        let service = HttpClientService::from_config_for_subgraph(
            "test",
            &Configuration::default(),
            &rustls::RootCertStore::empty(),
            crate::configuration::shared::Client::builder()
                .experimental_http2(http2_config)
                .build(),
        )
        .expect("created http client");

        let hyperlocal_uri: http::Uri = hyperlocal::Uri::new(path.to_str().unwrap(), "/").into();
        let response = send_request(service, hyperlocal_uri, r#"{"query":"{ test }"}"#).await;

        expected.check(&version_tracker);
        assert_response_body(response, r#"{"data": "success"}"#).await;
    }
}

mod pool_idle_timeout {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::*;

    /// Server that counts how many TCP connections are accepted
    async fn serve_counting(
        listener: TcpListener,
        connection_count: Arc<AtomicUsize>,
    ) -> std::io::Result<()> {
        loop {
            let (stream, _) = listener.accept().await?;
            connection_count.fetch_add(1, Ordering::SeqCst);
            let io = TokioIo::new(stream);
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(|_request: Request<Incoming>| async {
                    Ok::<_, Infallible>(
                        http::Response::builder()
                            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                            .status(StatusCode::OK)
                            .body::<Body>(r#"{"data":null}"#.into())
                            .unwrap(),
                    )
                });
                let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(io, svc)
                    .await;
            });
        }
    }

    fn make_client_config(timeout: Option<Duration>) -> crate::configuration::shared::Client {
        match timeout {
            Some(d) => crate::configuration::shared::Client::builder()
                .pool_idle_timeout(d)
                .build(),
            None => crate::configuration::shared::Client {
                pool_idle_timeout: None,
                ..Default::default()
            },
        }
    }

    #[rstest]
    #[case::short_timeout_evicts(
        Some(Duration::from_millis(50)),
        Duration::from_millis(200),
        2, // expect a new connection after the idle timeout
    )]
    #[case::long_timeout_reuses(
        Some(Duration::from_secs(60)),
        Duration::from_millis(200),
        1, // expect the pooled connection to be reused
    )]
    #[case::none_disables_eviction(
        None,
        Duration::from_millis(200),
        1, // None means no idle timeout → connection stays pooled
    )]
    #[tokio::test(flavor = "multi_thread")]
    async fn test_pool_idle_timeout_evicts_connections(
        #[case] timeout: Option<Duration>,
        #[case] sleep_between: Duration,
        #[case] expected_connections: usize,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket_addr = listener.local_addr().unwrap();
        let connection_count = Arc::new(AtomicUsize::new(0));

        tokio::task::spawn(serve_counting(listener, connection_count.clone()));

        let mut service = HttpClientService::test_new(
            "test",
            rustls::ClientConfig::builder()
                .with_native_roots()
                .expect("read native TLS root certificates")
                .with_no_client_auth(),
            make_client_config(timeout),
        )
        .expect("can create HttpClientService");

        let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();

        let response = send_request(service.clone(), url.clone(), r#"{"query":"{ a }"}"#).await;
        assert_eq!(response.http_response.status(), StatusCode::OK);
        assert_eq!(
            connection_count.load(Ordering::SeqCst),
            1,
            "first request opens one connection"
        );

        tokio::time::sleep(sleep_between).await;

        tower::ServiceExt::ready(&mut service).await.unwrap();
        let response = send_request(service, url, r#"{"query":"{ b }"}"#).await;
        assert_eq!(response.http_response.status(), StatusCode::OK);
        assert_eq!(
            connection_count.load(Ordering::SeqCst),
            expected_connections,
            "expected {expected_connections} total TCP connections for timeout {timeout:?} with {sleep_between:?} sleep"
        );
    }
}
