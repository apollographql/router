use std::convert::Infallible;
use std::io;
use std::str::FromStr;
use std::sync::Arc;
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
use crate::services::router;
use crate::services::supergraph;

async fn tls_server(
    listener: TcpListener,
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    body: &'static str,
) {
    let tls_config = Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)
            .expect("built our tls config"),
    );
    let acceptor = TlsAcceptor::from(tls_config);

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

async fn serve<Handler, Fut>(listener: TcpListener, handle: Handler) -> std::io::Result<()>
where
    Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
    Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
{
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handle = handle.clone();
        tokio::spawn(async move {
            // N.B. should use hyper service_fn here, since it's required to be implemented hyper Service trait!
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

#[cfg(unix)]
async fn serve_unix<Handler, Fut>(listener: UnixListener, handle: Handler) -> std::io::Result<()>
where
    Handler: (Fn(http::Request<Body>) -> Fut) + Clone + Sync + Send + 'static,
    Fut: std::future::Future<Output = Result<http::Response<Body>, Infallible>> + Send + 'static,
{
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let handle = handle.clone();
        tokio::spawn(async move {
            // N.B. should use hyper service_fn here, since it's required to be implemented hyper Service trait!
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

// Note: This test relies on a checked in certificate with the following validity
// characteristics:
//         Validity
//           Not Before: Oct 10 07:32:39 2023 GMT
//           Not After : Oct  7 07:32:39 2033 GMT
// If this test fails and it is October 7th 2033, you will need to generate a
// new self signed cert. Currently, we use openssl to do this, in the future I
// hope we have something better...
// In the testdata directory run:
// openssl x509 -req -in server_self_signed.csr -signkey server.key -out server_self_signed.crt -extfile server.ext -days 3650
// That will give you another 10 years, assuming nothing else in the signing
// framework has expired.
#[tokio::test(flavor = "multi_thread")]
async fn tls_self_signed() {
    let certificate_pem = include_str!("./testdata/server_self_signed.crt");
    let key_pem = include_str!("./testdata/server.key");

    let certificates = load_certs(certificate_pem).unwrap();
    let key = load_key(key_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(tls_server(listener, certificates, key, r#"{"data": null}"#));

    // we cannot parse a configuration from text, because certificates are generally
    // added by file expansion and we don't have access to that here, and inserting
    // the PEM data directly generates parsing issues due to end of line characters
    let mut config = Configuration::default();
    config.tls.subgraph.subgraphs.insert(
        "test".to_string(),
        TlsClient {
            certificate_authorities: Some(certificate_pem.into()),
            client_authentication: None,
        },
    );
    let subgraph_service = HttpClientService::from_config_for_subgraph(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(
                    r#"{"query":"{ me { name username } }"#,
                ))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data": null}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_self_signed_connector() {
    let certificate_pem = include_str!("./testdata/server_self_signed.crt");
    let key_pem = include_str!("./testdata/server.key");

    let certificates = load_certs(certificate_pem).unwrap();
    let key = load_key(key_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(tls_server(
        listener,
        certificates,
        key,
        r#"{"my_field": "abc"}"#,
    ));

    // we cannot parse a configuration from text, because certificates are generally
    // added by file expansion and we don't have access to that here, and inserting
    // the PEM data directly generates parsing issues due to end of line characters
    let mut config = Configuration::default();
    config.tls.connector.sources.insert(
        "test".to_string(),
        TlsClient {
            certificate_authorities: Some(certificate_pem.into()),
            client_authentication: None,
        },
    );
    let subgraph_service = HttpClientService::from_config_for_connector(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(r#"{}"#))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"my_field": "abc"}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_custom_root() {
    let certificate_pem = include_str!("./testdata/server.crt");
    let ca_pem = include_str!("./testdata/CA/ca.crt");
    let key_pem = include_str!("./testdata/server.key");

    let mut certificates = load_certs(certificate_pem).unwrap();
    certificates.extend(load_certs(ca_pem).unwrap());
    let key = load_key(key_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(tls_server(listener, certificates, key, r#"{"data": null}"#));

    // we cannot parse a configuration from text, because certificates are generally
    // added by file expansion and we don't have access to that here, and inserting
    // the PEM data directly generates parsing issues due to end of line characters
    let mut config = Configuration::default();
    config.tls.subgraph.subgraphs.insert(
        "test".to_string(),
        TlsClient {
            certificate_authorities: Some(ca_pem.into()),
            client_authentication: None,
        },
    );
    let subgraph_service = HttpClientService::from_config_for_subgraph(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(
                    r#"{"query":"{ me { name username } }"#,
                ))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data": null}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_custom_root_connector() {
    let certificate_pem = include_str!("./testdata/server.crt");
    let ca_pem = include_str!("./testdata/CA/ca.crt");
    let key_pem = include_str!("./testdata/server.key");

    let mut certificates = load_certs(certificate_pem).unwrap();
    certificates.extend(load_certs(ca_pem).unwrap());
    let key = load_key(key_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(tls_server(
        listener,
        certificates,
        key,
        r#"{"my_field": "abc"}"#,
    ));

    // we cannot parse a configuration from text, because certificates are generally
    // added by file expansion and we don't have access to that here, and inserting
    // the PEM data directly generates parsing issues due to end of line characters
    let mut config = Configuration::default();
    config.tls.connector.sources.insert(
        "test".to_string(),
        TlsClient {
            certificate_authorities: Some(ca_pem.into()),
            client_authentication: None,
        },
    );
    let subgraph_service = HttpClientService::from_config_for_connector(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(r#"{}"#))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"my_field": "abc"}"#
    );
}

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

#[tokio::test(flavor = "multi_thread")]
async fn tls_client_auth() {
    let server_certificate_pem = include_str!("./testdata/server.crt");
    let ca_pem = include_str!("./testdata/CA/ca.crt");
    let server_key_pem = include_str!("./testdata/server.key");

    let mut server_certificates = load_certs(server_certificate_pem).unwrap();
    let ca_certificate = load_certs(ca_pem).unwrap().remove(0);
    server_certificates.push(ca_certificate.clone());
    let key = load_key(server_key_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(tls_server_with_client_auth(
        listener,
        server_certificates,
        key,
        ca_certificate,
        r#"{"data": null}"#,
    ));

    let client_certificate_pem = include_str!("./testdata/client.crt");
    let client_key_pem = include_str!("./testdata/client.key");

    let client_certificates = load_certs(client_certificate_pem).unwrap();
    let client_key = load_key(client_key_pem).unwrap();

    // we cannot parse a configuration from text, because certificates are generally
    // added by file expansion and we don't have access to that here, and inserting
    // the PEM data directly generates parsing issues due to end of line characters
    let mut config = Configuration::default();
    config.tls.subgraph.subgraphs.insert(
        "test".to_string(),
        TlsClient {
            certificate_authorities: Some(ca_pem.into()),
            client_authentication: Some(Arc::new(TlsClientAuth {
                certificate_chain: client_certificates,
                key: client_key,
            })),
        },
    );
    let subgraph_service = HttpClientService::from_config_for_subgraph(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(
                    r#"{"query":"{ me { name username } }"#,
                ))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data": null}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_client_auth_connector() {
    let server_certificate_pem = include_str!("./testdata/server.crt");
    let ca_pem = include_str!("./testdata/CA/ca.crt");
    let server_key_pem = include_str!("./testdata/server.key");

    let mut server_certificates = load_certs(server_certificate_pem).unwrap();
    let ca_certificate = load_certs(ca_pem).unwrap().remove(0);
    server_certificates.push(ca_certificate.clone());
    let key = load_key(server_key_pem).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(tls_server_with_client_auth(
        listener,
        server_certificates,
        key,
        ca_certificate,
        r#"{"my_field": "abc"}"#,
    ));

    let client_certificate_pem = include_str!("./testdata/client.crt");
    let client_key_pem = include_str!("./testdata/client.key");

    let client_certificates = load_certs(client_certificate_pem).unwrap();
    let client_key = load_key(client_key_pem).unwrap();

    // we cannot parse a configuration from text, because certificates are generally
    // added by file expansion and we don't have access to that here, and inserting
    // the PEM data directly generates parsing issues due to end of line characters
    let mut config = Configuration::default();
    config.tls.connector.sources.insert(
        "test".to_string(),
        TlsClient {
            certificate_authorities: Some(ca_pem.into()),
            client_authentication: Some(Arc::new(TlsClientAuth {
                certificate_chain: client_certificates,
                key: client_key,
            })),
        },
    );
    let subgraph_service = HttpClientService::from_config_for_connector(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        crate::configuration::shared::Client::default(),
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(router::body::from_bytes(r#"{}"#))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"my_field": "abc"}"#
    );
}

// starts a local server emulating a subgraph returning status code 401
async fn emulate_h2c_server(listener: TcpListener) {
    async fn handle(_request: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        println!("h2C server got req: {_request:?}");
        Ok(http::Response::builder()
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .status(StatusCode::OK)
            .body(
                serde_json::to_string(&Response {
                    data: Some(Value::default()),
                    ..Response::default()
                })
                .expect("always valid")
                .into(),
            )
            .unwrap())
    }

    // XXX(@goto-bus-stop): ideally this server would *only* support HTTP 2 and not HTTP 1
    serve(listener, handle).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_h2c() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(emulate_h2c_server(listener));
    let subgraph_service = HttpClientService::new(
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
                .body(router::body::from_bytes(
                    r#"{"query":"{ me { name username } }"#,
                ))
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data":null}"#
    );
}

// starts a local server emulating a subgraph returning compressed response
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
    // Though the server doesn't use TLS, the client still supports it, and so we need crypto stuff

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(emulate_subgraph_compressed_response(listener));
    let subgraph_service = HttpClientService::new(
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

    assert_eq!(
        std::str::from_utf8(
            &router::body::into_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data":"test"}"#
    );
}

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
        service: crate::services::http::BoxService,
    ) -> crate::services::http::BoxService {
        self.started.store(true, Ordering::Release);
        service
    }

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

fn make_schema(path: &str) -> String {
    r#"schema
      @link(url: "https://specs.apollo.dev/link/v1.0")
      @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
      @link(url: "https://specs.apollo.dev/authenticated/v0.1", for: SECURITY)
         {
        query: Query
   }
   directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA
   directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE
   directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR
   directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION

   scalar link__Import

   enum link__Purpose {
    """
    `SECURITY` features provide metadata necessary to securely resolve fields.
    """
    SECURITY
  
    """
    `EXECUTION` features provide metadata necessary for operation execution.
    """
    EXECUTION
  }
  
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "unix://"#.to_string()+path+r#"")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query 
   @join__type(graph: ORGA)
   @join__type(graph: USER)
   {
       currentUser: User @join__field(graph: USER)
   }

   type User
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
   }"#
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(not(target_os = "windows"))]
async fn test_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("router.sock");
    let schema = make_schema(path.to_str().unwrap());

    async fn handle(mut req: http::Request<Body>) -> Result<http::Response<Body>, Infallible> {
        let data = router::body::into_bytes(req.body_mut()).await.unwrap();
        let body = std::str::from_utf8(&data).unwrap();
        println!("{body:?}");
        let response = http::Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{ "data": { "currentUser": { "id": "0" } } }"#,
            ))
            .unwrap();
        Ok(response)
    }

    let listener = UnixListener::bind(path).unwrap();
    tokio::task::spawn(serve_unix(listener, handle));

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
