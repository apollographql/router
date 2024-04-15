use std::convert::Infallible;
use std::io;
use std::net::TcpListener;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_compression::tokio::write::GzipDecoder;
use async_compression::tokio::write::GzipEncoder;
use axum::Server;
use http::header::CONTENT_ENCODING;
use http::header::CONTENT_TYPE;
use http::StatusCode;
use http::Uri;
use http::Version;
use hyper::server::conn::AddrIncoming;
use hyper::service::make_service_fn;
use hyper::Body;
use hyper_rustls::ConfigBuilderExt;
use hyper_rustls::TlsAcceptor;
#[cfg(unix)]
use hyperlocal::UnixServerExt;
use mime::APPLICATION_JSON;
use rustls::server::AllowAnyAuthenticatedClient;
use rustls::Certificate;
use rustls::PrivateKey;
use rustls::RootCertStore;
use rustls::ServerConfig;
use serde_json_bytes::ByteString;
use serde_json_bytes::Value;
use tokio::io::AsyncWriteExt;
use tower::service_fn;
use tower::BoxError;
use tower::ServiceExt;

use crate::configuration::load_certs;
use crate::configuration::load_key;
use crate::configuration::TlsClient;
use crate::configuration::TlsClientAuth;
use crate::graphql::Response;
use crate::plugin::PluginInit;
use crate::plugin::PluginPrivate;
use crate::plugins::traffic_shaping::Http2Config;
use crate::services::http::HttpClientService;
use crate::services::http::HttpRequest;
use crate::services::supergraph;
use crate::Configuration;
use crate::Context;
use crate::TestHarness;

async fn tls_server(
    listener: tokio::net::TcpListener,
    certificates: Vec<Certificate>,
    key: PrivateKey,
    body: &'static str,
) {
    let acceptor = TlsAcceptor::builder()
        .with_single_cert(certificates, key)
        .unwrap()
        .with_all_versions_alpn()
        .with_incoming(AddrIncoming::from_listener(listener).unwrap());
    let service = make_service_fn(|_| async {
        Ok::<_, io::Error>(service_fn(|_req| async {
            Ok::<_, io::Error>(
                http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .version(Version::HTTP_11)
                    .body::<Body>(body.into())
                    .unwrap(),
            )
        }))
    });
    let server = Server::builder(acceptor).serve(service);
    server.await.unwrap()
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    let subgraph_service = HttpClientService::from_config(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        Http2Config::Enable,
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(r#"{"query":"{ me { name username } }"#.into())
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        std::str::from_utf8(
            &hyper::body::to_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data": null}"#
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    let subgraph_service = HttpClientService::from_config(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        Http2Config::Enable,
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(r#"{"query":"{ me { name username } }"#.into())
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &hyper::body::to_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data": null}"#
    );
}

async fn tls_server_with_client_auth(
    listener: tokio::net::TcpListener,
    certificates: Vec<Certificate>,
    key: PrivateKey,
    client_root: Certificate,
    body: &'static str,
) {
    let mut client_auth_roots = RootCertStore::empty();
    client_auth_roots.add(&client_root).unwrap();

    let client_auth = AllowAnyAuthenticatedClient::new(client_auth_roots).boxed();

    let acceptor = TlsAcceptor::builder()
        .with_tls_config(
            ServerConfig::builder()
                .with_safe_defaults()
                .with_client_cert_verifier(client_auth)
                .with_single_cert(certificates, key)
                .unwrap(),
        )
        .with_all_versions_alpn()
        .with_incoming(AddrIncoming::from_listener(listener).unwrap());
    let service = make_service_fn(|_| async {
        Ok::<_, io::Error>(service_fn(|_req| async {
            Ok::<_, io::Error>(
                http::Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                    .status(StatusCode::OK)
                    .version(Version::HTTP_11)
                    .body::<Body>(body.into())
                    .unwrap(),
            )
        }))
    });
    let server = Server::builder(acceptor).serve(service);
    server.await.unwrap()
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

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
            client_authentication: Some(TlsClientAuth {
                certificate_chain: client_certificates,
                key: client_key,
            }),
        },
    );
    let subgraph_service = HttpClientService::from_config(
        "test",
        &config,
        &rustls::RootCertStore::empty(),
        Http2Config::Enable,
    )
    .unwrap();

    let url = Uri::from_str(&format!("https://localhost:{}", socket_addr.port())).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(r#"{"query":"{ me { name username } }"#.into())
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &hyper::body::to_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data": null}"#
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

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::from_tcp(listener)
        .unwrap()
        .http2_only(true)
        .serve(make_svc);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_subgraph_h2c() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(emulate_h2c_server(listener));
    let subgraph_service = HttpClientService::new(
        "test",
        Http2Config::Http2Only,
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_native_roots()
            .with_no_client_auth(),
    )
    .expect("can create a HttpService");

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .body(r#"{"query":"{ me { name username } }"#.into())
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        std::str::from_utf8(
            &hyper::body::to_bytes(response.http_response.into_parts().1)
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
        let body = hyper::body::to_bytes(request.into_body())
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

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });
    let server = Server::from_tcp(listener).unwrap().serve(make_svc);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_compressed_request_response_body() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let socket_addr = listener.local_addr().unwrap();
    tokio::task::spawn(emulate_subgraph_compressed_response(listener));
    let subgraph_service = HttpClientService::new(
        "test",
        Http2Config::Http2Only,
        rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_native_roots()
            .with_no_client_auth(),
    )
    .expect("can create a HttpService");

    let url = Uri::from_str(&format!("http://{socket_addr}")).unwrap();
    let response = subgraph_service
        .oneshot(HttpRequest {
            http_request: http::Request::builder()
                .uri(url)
                .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
                .header(CONTENT_ENCODING, "gzip")
                .body(r#"{"query":"{ me { name username } }"#.into())
                .unwrap(),
            context: Context::new(),
        })
        .await
        .unwrap();

    assert_eq!(
        std::str::from_utf8(
            &hyper::body::to_bytes(response.http_response.into_parts().1)
                .await
                .unwrap()
        )
        .unwrap(),
        r#"{"data":"test"}"#
    );
}

const SCHEMA: &str = r#"schema
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
        subscription: Subscription
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "http://localhost:4001/graphql")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       currentUser: User @join__field(graph: USER)
   }

   type Subscription @join__type(graph: USER) {
        userWasCreated: User
   }

   type User
   @join__owner(graph: USER)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id"){
       id: ID!
       name: String
       activeOrganization: Organization
   }
   type Organization
   @join__owner(graph: ORGA)
   @join__type(graph: ORGA, key: "id")
   @join__type(graph: USER, key: "id") {
       id: ID
       creatorUser: User
       name: String
       nonNullId: ID!
       suborga: [Organization]
   }"#;

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
        @core(feature: "https://specs.apollo.dev/core/v0.1")
        @core(feature: "https://specs.apollo.dev/join/v0.1")
        @core(feature: "https://specs.apollo.dev/inaccessible/v0.1")
         {
        query: Query
   }
   directive @core(feature: String!) repeatable on SCHEMA
   directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
   directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
   directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
   directive @join__graph(name: String!, url: String!) on ENUM_VALUE
   directive @inaccessible on OBJECT | FIELD_DEFINITION | INTERFACE | UNION
   scalar join__FieldSet
   enum join__Graph {
       USER @join__graph(name: "user", url: "unix://"#.to_string()+path+r#"")
       ORGA @join__graph(name: "orga", url: "http://localhost:4002/graphql")
   }
   type Query {
       currentUser: User @join__field(graph: USER)
   }

   type User
   @join__owner(graph: USER)
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

    let make_service = make_service_fn(|_| async {
        Ok::<_, hyper::Error>(service_fn(|mut req: http::Request<Body>| async move {
            let data = hyper::body::to_bytes(req.body_mut()).await.unwrap();
            let body = std::str::from_utf8(&data).unwrap();
            println!("{:?}", body);
            let response = http::Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{ "data": { "currentUser": { "id": "0" } } }"#,
                ))
                .unwrap();
            Ok::<_, hyper::Error>(response)
        }))
    });

    tokio::task::spawn(async move {
        hyper::Server::bind_unix(path)
            .unwrap()
            .serve(make_service)
            .await
            .unwrap();
    });

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
