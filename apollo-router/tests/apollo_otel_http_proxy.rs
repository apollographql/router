//! Validates that Apollo OTLP HTTP telemetry respects standard HTTP proxy environment variables.
//!
//! `HTTP_PROXY` and `HTTPS_PROXY` are read by `hyper-util`'s proxy matcher
//! unconditionally at `reqwest::Client` creation time — which happens when the
//! router initialises its OTLP exporter on start-up.  This behaviour was
//! introduced in apollographql/router#9055 and this test serves as a regression
//! guard for the traces HTTP export path.
//!
//! The test:
//!
//! 1. Starts a mock OTLP backend that records received trace reports.
//! 2. Starts a simple in-process HTTP forward proxy that records intercepted
//!    request URIs and forwards them to the backend.
//! 3. Sets `HTTP_PROXY` to point at the in-process proxy *before* building the
//!    router, so the reqwest client picks it up at initialisation time.
//! 4. Sends a GraphQL query through the router and waits for the OTLP batch to
//!    flush.
//! 5. Asserts that the proxy intercepted a `/v1/traces` request *and* that the
//!    backend decoded a valid `ExportTraceServiceRequest` with resource spans.
//!
//! # Why there is no metrics proxy test
//!
//! The Apollo OTLP metrics pipeline uses a `PeriodicReader` that only calls
//! `export()` when `scope_metrics` is non-empty.  The `ApolloRealtime` meter
//! provider only accepts instruments matching
//! `apollo.router.operations.(error|fetch.duration)`, which are recorded by the
//! real HTTP subgraph fetch layer — bypassed by `TestHarness` mock subgraphs.
//! An equivalent metrics proxy test requires the `IntegrationTest` framework
//! (which starts a real router process) rather than `TestHarness`.  The same
//! `reqwest::Client` proxy mechanism that this test validates for traces applies
//! identically to the metrics exporter.

use std::sync::Arc;
use std::time::Duration;

use apollo_router::TestHarness;
use apollo_router::services::router;
use apollo_router::services::router::BoxCloneService;
use apollo_router::services::supergraph;
use axum::Router;
use axum::extract::State;
use axum::routing::post;
use bytes::Bytes;
use http_body_util::BodyExt as _;
use once_cell::sync::Lazy;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tower::Service;
use tower::ServiceExt;
use tower_http::decompression::RequestDecompressionLayer;

mod tracing_common;

// The protobuf-generated reports.rs (included by tracing_common via
// `tonic::include_proto!("reports")`) contains serde attributes that reference
// `crate::plugins::telemetry::apollo_exporter::serialize_timestamp`.  We must
// provide that path in this test crate.
pub(crate) mod plugins {
    pub(crate) mod telemetry {
        pub(crate) mod apollo_exporter {
            pub(crate) fn serialize_timestamp<S>(
                timestamp: &Option<prost_types::Timestamp>,
                serializer: S,
            ) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                use serde::ser::SerializeStruct as _;
                match timestamp {
                    Some(ts) => {
                        let mut s = serializer.serialize_struct("Timestamp", 2)?;
                        s.serialize_field("seconds", &ts.seconds)?;
                        s.serialize_field("nanos", &ts.nanos)?;
                        s.end()
                    }
                    None => serializer.serialize_none(),
                }
            }
        }
    }
}

static ROUTER_SERVICE_RUNTIME: Lazy<Arc<tokio::runtime::Runtime>> = Lazy::new(|| {
    Arc::new(tokio::runtime::Runtime::new().expect("must be able to create tokio runtime"))
});
// All tests in this file must run serially: each test installs a process-wide
// tracing subscriber and mutates process-wide environment variables.  Note:
// each tests/*.rs file compiles as a separate binary, so this mutex is NOT
// shared with apollo_otel_traces.rs or any other test file.
static TEST: Lazy<Arc<Mutex<()>>> = Lazy::new(Default::default);

// ---------------------------------------------------------------------------
// Mock OTLP backend
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct BackendState {
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
}

/// Decodes gzip-decompressed protobuf bytes into an `ExportTraceServiceRequest`
/// and appends it to the shared report list.  `RequestDecompressionLayer` on the
/// `/v1/traces` route handles decompression before this handler is called.
async fn backend_traces_handler(State(state): State<BackendState>, bytes: Bytes) -> axum::Json<()> {
    if let Ok(report) = ExportTraceServiceRequest::decode(&*bytes) {
        state.reports.lock().await.push(report);
    }
    axum::Json(())
}

// ---------------------------------------------------------------------------
// In-process HTTP forward proxy
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct ProxyState {
    /// URIs of requests the proxy has intercepted (absolute form, e.g.
    /// `http://127.0.0.1:PORT/v1/traces`).
    intercepted_uris: Arc<Mutex<Vec<String>>>,
    /// reqwest client configured with `.no_proxy()` so the forwarded request
    /// does NOT loop back through the proxy.
    client: reqwest::Client,
}

/// When reqwest sends a request through an HTTP proxy it uses the *absolute-form*
/// URI in the request line:
///
/// ```text
/// POST http://127.0.0.1:PORT/v1/traces HTTP/1.1
/// Host: 127.0.0.1:PORT
/// Content-Type: application/x-protobuf
/// Content-Encoding: gzip
/// x-api-key: test
/// …
/// ```
///
/// Hyper (which axum builds on) preserves the absolute URI, so `req.uri()` in
/// this handler contains the full target URL.  We fall back to reconstructing it
/// from the `Host` header + path when the URI is relative (e.g. a direct
/// connection rather than a proxy connection).
async fn proxy_forward_handler(
    State(state): State<ProxyState>,
    req: axum::extract::Request,
) -> impl axum::response::IntoResponse {
    let target_url = if req.uri().scheme().is_some() {
        req.uri().to_string()
    } else {
        let host = req
            .headers()
            .get(http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        format!("http://{}{}", host, req.uri().path())
    };

    state.intercepted_uris.lock().await.push(target_url.clone());

    let method = req.method().clone();
    let original_headers = req.headers().clone();
    let body_bytes = req
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();

    let mut req_builder = state.client.request(method, &target_url);

    for (name, value) in &original_headers {
        if name != http::header::HOST {
            req_builder = req_builder.header(name, value);
        }
    }

    match req_builder.body(body_bytes).send().await {
        Ok(resp) => (resp.status(), axum::body::Body::empty()),
        Err(err) => {
            eprintln!("[proxy] forward error: {err}");
            (http::StatusCode::BAD_GATEWAY, axum::body::Body::empty())
        }
    }
}

// ---------------------------------------------------------------------------
// Test setup
// ---------------------------------------------------------------------------

async fn setup(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    intercepted_uris: Arc<Mutex<Vec<String>>>,
) -> (JoinHandle<()>, JoinHandle<()>, BoxCloneService) {
    // 1. Start the mock OTLP backend.
    //    /v1/traces: OTLP HTTP path — decompresses gzip, decodes protobuf.
    //    /         : legacy Apollo reporter path — intentionally ignored here.
    let backend_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_addr = backend_listener.local_addr().unwrap();
    let backend_state = BackendState {
        reports: reports.clone(),
    };
    let backend_app = Router::new()
        .route("/", post(|| async { axum::Json(()) }))
        .merge(
            Router::new()
                .route("/v1/traces", post(backend_traces_handler))
                .layer(RequestDecompressionLayer::new())
                .with_state(backend_state),
        );
    let backend_task = ROUTER_SERVICE_RUNTIME.spawn(async move {
        axum::serve(backend_listener, backend_app)
            .await
            .expect("backend server failed")
    });

    // 2. Start the HTTP forward proxy.
    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    let no_proxy_client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("failed to build no-proxy reqwest client");
    let proxy_state = ProxyState {
        intercepted_uris: intercepted_uris.clone(),
        client: no_proxy_client,
    };
    let proxy_app = Router::new()
        .fallback(proxy_forward_handler)
        .with_state(proxy_state);
    let proxy_task = ROUTER_SERVICE_RUNTIME.spawn(async move {
        axum::serve(proxy_listener, proxy_app)
            .await
            .expect("proxy server failed")
    });

    // 3. Configure the router to send OTLP traces via HTTP.
    *apollo_router::_private::APOLLO_KEY.lock() = Some("test".to_string());
    *apollo_router::_private::APOLLO_GRAPH_REF.lock() = Some("test".to_string());

    let mut config: serde_json::Value =
        serde_yaml::from_str(include_str!("fixtures/reports/apollo_reports.router.yaml"))
            .expect("apollo_reports.router.yaml was invalid");

    config = jsonpath_lib::replace_with(config, "$.telemetry.apollo.endpoint", &mut |_| {
        Some(serde_json::Value::String(format!("http://{backend_addr}")))
    })
    .unwrap();
    config = jsonpath_lib::replace_with(
        config,
        "$.telemetry.apollo.experimental_otlp_endpoint",
        &mut |_| Some(serde_json::Value::String(format!("http://{backend_addr}"))),
    )
    .unwrap();
    config = jsonpath_lib::replace_with(
        config,
        "$.telemetry.apollo.otlp_tracing_sampler",
        &mut |_| Some(serde_json::Value::String("always_on".to_string())),
    )
    .unwrap();
    config = jsonpath_lib::replace_with(
        config,
        "$.telemetry.apollo.experimental_otlp_tracing_protocol",
        &mut |_| Some(serde_json::Value::String("http".to_string())),
    )
    .unwrap();

    // 4. Route OTLP HTTP traffic through the in-process proxy.
    //
    //    hyper-util reads HTTP_PROXY at client creation time (when the router
    //    initialises its OTLP exporters).  The env var must therefore be set
    //    before build_router() is called.
    //
    //    Safety: the TEST mutex guarantees at most one test in this file runs
    //    at a time, so no concurrent code reads or writes these variables.
    //
    //    We establish a clean proxy environment before setting our own values.
    //    Two classes of interference are possible:
    //
    //    * A stale HTTP_PROXY / HTTPS_PROXY would cause traffic to hit a
    //      different proxy instead of ours.
    //    * A NO_PROXY entry matching 127.0.0.1 would bypass our proxy entirely,
    //      causing intercepted_uris to remain empty.
    //
    //    The second case is the CI-relevant one: CircleCI injects
    //    `NO_PROXY=127.0.0.1,localhost,circleci-internal-outer-build-agent`
    //    into every Linux container.  Because our backend and proxy both bind
    //    to 127.0.0.1, that NO_PROXY entry silently bypasses the proxy on Linux
    //    CI while macOS / Windows runners pass (they have no such injection).
    //    Clearing all six variables before calling build_router() removes the
    //    interference regardless of what the host or CI environment has set.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("http_proxy");
        std::env::remove_var("HTTPS_PROXY");
        std::env::remove_var("https_proxy");
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
        std::env::set_var("HTTP_PROXY", format!("http://{proxy_addr}"));
    }

    let router_service = TestHarness::builder()
        .try_log_level("INFO")
        .configuration_json(config)
        .expect("test harness had config errors")
        .schema(include_str!("fixtures/supergraph.graphql"))
        .subgraph_hook(|subgraph, _service| tracing_common::subgraph_mocks(subgraph))
        .build_router()
        .await
        .expect("could not create router test harness");

    (backend_task, proxy_task, router_service)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

/// Verifies that OTLP HTTP traces flow through an HTTP proxy without data loss.
///
/// Asserts:
/// - The proxy intercepted at least one request to `/v1/traces`.
/// - The backend decoded a valid `ExportTraceServiceRequest` with resource spans.
#[tokio::test(flavor = "multi_thread")]
async fn test_otlp_http_traces_through_proxy() {
    let _guard = TEST.lock().await;

    let reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>> = Arc::new(Mutex::new(vec![]));
    let intercepted_uris: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));

    let (backend_task, proxy_task, mut service) =
        setup(reports.clone(), intercepted_uris.clone()).await;

    // Send a simple query that produces a traceable supergraph span.
    let request = supergraph::Request::fake_builder()
        .query("query { topProducts { name reviews { author { name } } } }")
        .build()
        .unwrap();
    let req: router::Request = request.try_into().expect("could not convert request");

    let response = service
        .ready()
        .await
        .expect("router was never ready")
        .call(req)
        .await
        .expect("router call failed");

    // Drain the response body so the router can proceed with span export.
    let _ = response.response.into_body().collect().await;

    // Wait up to ~1 s for the OTLP batch to flush.
    let mut trace_received = false;
    for _ in 0..10 {
        if !reports.lock().await.is_empty() {
            trace_received = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Clean up env and background tasks before asserting, so any failure message
    // does not leave environment pollution or zombie tasks behind.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("http_proxy");
        std::env::remove_var("HTTPS_PROXY");
        std::env::remove_var("https_proxy");
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }
    backend_task.abort();
    proxy_task.abort();

    // --- Assertion 1: proxy intercepted OTLP trace traffic ----------------
    let uris = intercepted_uris.lock().await;
    assert!(
        uris.iter().any(|u| u.contains("/v1/traces")),
        "Expected proxy to intercept a /v1/traces request; intercepted URIs: {uris:?}"
    );

    // --- Assertion 2: backend received valid OTLP trace data --------------
    assert!(
        trace_received,
        "Backend should have received OTLP trace data through the proxy, but none arrived"
    );
    let backend_reports = reports.lock().await;
    let first_report = backend_reports
        .first()
        .expect("expected at least one trace report");
    assert!(
        !first_report.resource_spans.is_empty(),
        "Received trace report contains no resource spans"
    );

    println!("[proxy] intercepted {} request(s): {:?}", uris.len(), *uris);
    println!(
        "[backend] received {} trace report(s); first report has {} resource span(s)",
        backend_reports.len(),
        first_report.resource_spans.len()
    );
}
