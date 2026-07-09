//! Be aware that this test file contains some potentially flaky tests which embed a number of
//! assumptions about how traces are reported to Apollo Studio.
//!
//! In particular:
//!  - There are timings (sleeps) which work as things are implemented right now, but
//!    may be sources of problems in the future.
//!
//!  - These tests must execute serially across this binary AND across the
//!    sibling `apollo_reports` / `apollo_otel_http_proxy` binaries, because
//!    they each install process-wide OpenTelemetry tracer/meter providers and
//!    Apollo Studio mock collectors that would otherwise stomp each other.
//!    Serialization is enforced by the `serial-apollo-telemetry-integration`
//!    nextest test-group in `.config/nextest.toml` (an in-source mutex cannot
//!    do this because each `tests/*.rs` is a separate binary).
//!    DO NOT run these tests with bare `cargo test` -- only `cargo nextest`
//!    honours the group; bare `cargo test` will race the global state.
//!
//! Summary: The dragons here are ancient and very evil. Do not attempt to take their treasure.
//!
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use apollo_router::TestHarness;
use apollo_router::make_fake_batch;
use apollo_router::services::router;
use apollo_router::services::router::BoxCloneService;
use apollo_router::services::supergraph;
use axum::Extension;
use axum::Json;
use axum::routing::post;
use bytes::Bytes;
use http::header::ACCEPT;
use http_body_util::BodyExt as _;
use once_cell::sync::Lazy;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tower::Service;
use tower::ServiceExt;
use tower_http::decompression::RequestDecompressionLayer;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::path_regex;

mod tracing_common;

static ROUTER_SERVICE_RUNTIME: Lazy<Arc<tokio::runtime::Runtime>> = Lazy::new(|| {
    Arc::new(tokio::runtime::Runtime::new().expect("must be able to create tokio runtime"))
});

async fn config(
    use_legacy_request_span: bool,
    batch: bool,
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
) -> (JoinHandle<()>, serde_json::Value) {
    *apollo_router::_private::APOLLO_KEY.lock() = Some("test".to_string());
    *apollo_router::_private::APOLLO_GRAPH_REF.lock() = Some("test".to_string());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // The OTLP HTTP exporter sends gzip-compressed protobuf to /v1/traces, so we need
    // RequestDecompressionLayer on that route. The Apollo protobuf reporter sends to / with gzip
    // too, but we intentionally don't decompress there so those bytes fail to decode as OTLP proto
    // and are ignored.
    let otlp_routes = axum::Router::new()
        .route("/v1/traces", post(traces_handler))
        .layer(RequestDecompressionLayer::new());
    let app = axum::Router::new()
        .route("/", post(traces_handler))
        .merge(otlp_routes)
        .layer(tower_http::add_extension::AddExtensionLayer::new(reports));

    let task = ROUTER_SERVICE_RUNTIME.spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("could not start axum server")
    });

    let mut config: serde_json::Value = if batch {
        serde_yaml::from_str(include_str!(
            "fixtures/reports/apollo_reports_batch.router.yaml"
        ))
        .expect("apollo_reports.router.yaml was invalid")
    } else {
        serde_yaml::from_str(include_str!("fixtures/reports/apollo_reports.router.yaml"))
            .expect("apollo_reports.router.yaml was invalid")
    };
    config = jsonpath_lib::replace_with(config, "$.telemetry.apollo.endpoint", &mut |_| {
        Some(serde_json::Value::String(format!("http://{addr}")))
    })
    .expect("Could not sub in endpoint");
    config = jsonpath_lib::replace_with(
        config,
        "$.telemetry.apollo.experimental_otlp_endpoint",
        &mut |_| Some(serde_json::Value::String(format!("http://{addr}"))),
    )
    .expect("Could not sub in endpoint");
    config = jsonpath_lib::replace_with(
        config,
        "$.telemetry.apollo.otlp_tracing_sampler",
        &mut |_| Some(serde_json::Value::String("always_on".to_string())),
    )
    .expect("Could not sub in otlp sampler");
    config = jsonpath_lib::replace_with(
        config,
        "$.telemetry.apollo.experimental_otlp_tracing_protocol",
        &mut |_| Some(serde_json::Value::String("http".to_string())),
    )
    .expect("Could not sub in otlp protocol");
    config =
        jsonpath_lib::replace_with(config, "$.telemetry.spans.legacy_request_span", &mut |_| {
            Some(serde_json::Value::Bool(use_legacy_request_span))
        })
        .expect("Could not sub in endpoint");
    (task, config)
}

/// Spin up a localhost wiremock server that mimics the subset of the
/// `https://jsonplaceholder.typicode.com/` REST surface that
/// `tests/fixtures/supergraph_connect.graphql` exercises:
///
/// - `GET /posts` — list endpoint used by `query{posts{id body title}}`.
///   Returns a small deterministic payload (2 posts) so the resulting OTel
///   trace has a fixed, hermetic shape regardless of network reachability.
/// - `GET /posts/{id}` — entity fetch for the `post(id:)` field.
/// - `GET /missing*` — the `forceError` connector source path; always 404
///   so the `connector_error` test exercises the error path deterministically.
/// - `GET /health*` — the `routerHealth` connector source path; always 200
///   so any incidental health probe doesn't introduce non-determinism.
///
/// The server is leaked (`Box::leak`) so it lives for the duration of the
/// process — same pattern Apollo's test harness uses for the OTLP/Apollo
/// collector mocks above. Tests in this file are serialised by the
/// `serial-apollo-telemetry-integration` nextest group, so leaking is safe.
///
/// Used by `get_connector_router_service` to replace the live-network
/// egress these tests previously relied on. Without this mock the tests
/// flaked whenever CI couldn't reach `jsonplaceholder.typicode.com` (and
/// the snapshot encoded a non-deterministic `connect_request` count
/// matching whatever the live API happened to return).
async fn start_connector_mock_server() -> MockServer {
    let server = wiremock::MockServer::builder().start().await;

    // `GET /posts` — return a 2-element list. Deterministic.
    Mock::given(method("GET"))
        .and(path("/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": 1, "title": "first", "body": "first body"},
            {"id": 2, "title": "second", "body": "second body"},
        ])))
        .mount(&server)
        .await;

    // `GET /posts/{id}` — single-post entity fetch.
    Mock::given(method("GET"))
        .and(path_regex(r"^/posts/\d+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 1, "title": "first", "body": "first body"
        })))
        .mount(&server)
        .await;

    // `GET /missing*` — `forceError` source path. Always 404 so the
    // `connector_error` test exercises the error path deterministically.
    Mock::given(method("GET"))
        .and(path_regex(r"^/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    // `GET /health*` — `routerHealth` source. Not exercised by the current
    // test queries but mocked for completeness so any incidental request
    // doesn't reach off-host.
    Mock::given(method("GET"))
        .and(path_regex(r"^/health"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    server
}

/// Spin up a localhost wiremock server that mimics the three Apollo demo
/// subgraphs (`accounts`, `products`, `reviews`) referenced by
/// `tests/fixtures/supergraph.graphql`. The supergraph hardcodes
/// `https://{name}.demo.starstuff.dev/` for each subgraph, so any test that
/// uses `with_subgraph_network_requests()` makes a real HTTPS call out to
/// those public hosts. When that public infrastructure flakes (TLS RST,
/// transient teardown, etc.) the router surfaces a `SubrequestHttpError`
/// (`ECONNRESET` / `os error 104`) which then poisons the OTel trace shape
/// — `http_request` span has status code 2 (ERROR) instead of OK, and the
/// `apollo_private.ftv1` attribute the subgraph would have emitted is
/// missing, blowing up the snapshot assertion.
///
/// The mock listens on three distinct paths (one per subgraph) so the
/// router can be pointed at it via `override_subgraph_url`. Each path
/// returns a fixed payload captured from the live demo deployment,
/// including a valid base64-encoded FTV1 trace in `extensions.ftv1`. The
/// FTV1 bytes themselves are redacted by `assert_report!` so any
/// non-empty blob suffices, but using captured-from-live blobs keeps the
/// router's federation-trace decoder happy.
///
/// The server is leaked (`Box::leak`) so it lives for the duration of the
/// process — same pattern as `start_connector_mock_server` above. Tests
/// in this file are serialised by the `serial-apollo-telemetry-integration`
/// nextest group, so leaking is safe.
async fn start_demo_subgraphs_mock_server() -> MockServer {
    let server = wiremock::MockServer::builder().start().await;

    // products: `{ topProducts { __typename upc name } }`.
    // Response shape captured from `https://products.demo.starstuff.dev/`
    // — 4 products. Names/upcs don't show up in the snapshot (redacted),
    // but the count drives the count of downstream `reviews` `_entities`
    // representations and ultimately how many `accounts` lookups happen.
    Mock::given(method("POST"))
        .and(path("/products"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "topProducts": [
                    {"__typename": "Product", "upc": "1", "name": "Table"},
                    {"__typename": "Product", "upc": "2", "name": "Couch"},
                    {"__typename": "Product", "upc": "3", "name": "Chair"},
                    {"__typename": "Product", "upc": "4", "name": "Bed"},
                ]
            },
            "extensions": {
                "ftv1": "GgwI+Py80AYQwPCHgAIiDAj4/LzQBhDA8IeAAljyuRRywgJivwIKC3RvcFByb2R1Y3RzGglbUHJvZHVjdF1AyK4KSIDhC2JEEABiHwoDdXBjGgdTdHJpbmchQMS8DUju7w1qB1Byb2R1Y3RiHwoEbmFtZRoGU3RyaW5nQIS3Dkjwyg5qB1Byb2R1Y3RiRBABYh8KA3VwYxoHU3RyaW5nIUDGqA9IutQPagdQcm9kdWN0Yh8KBG5hbWUaBlN0cmluZ0De5g9I7vMPagdQcm9kdWN0YkQQAmIfCgN1cGMaB1N0cmluZyFA4LIQSPC/EGoHUHJvZHVjdGIfCgRuYW1lGgZTdHJpbmdAsOoQSOb7EGoHUHJvZHVjdGJEEANiHwoDdXBjGgdTdHJpbmchQILBEUjI0BFqB1Byb2R1Y3RiHwoEbmFtZRoGU3RyaW5nQLLjEUik8BFqB1Byb2R1Y3RqBVF1ZXJ5+QEAAAAAAADwPw=="
            }
        })))
        .mount(&server)
        .await;

    // reviews: federation `_entities` fetch over Product. Returns a
    // review-shaped payload with author User refs. Captured from
    // `https://reviews.demo.starstuff.dev/` against the exact operation
    // text the router emits for the `test_send_variable_value` query.
    Mock::given(method("POST"))
        .and(path("/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "_entities": [
                    {"reviews": [
                        {"author": {"__typename": "User", "id": "1"}},
                        {"author": {"__typename": "User", "id": "2"}},
                    ]},
                    {"reviews": [
                        {"author": {"__typename": "User", "id": "1"}},
                    ]},
                    {"reviews": [
                        {"author": {"__typename": "User", "id": "2"}},
                    ]},
                    {"reviews": []},
                ]
            },
            "extensions": {
                "ftv1": "GgwI+/y80AYQwObxvAMiDAj7/LzQBhDA1P26A1imyfwBcuEDYt4DCglfZW50aXRpZXMaCltfRW50aXR5XSFAjq/wAUjyr/oBYq0BEABiqAEKB3Jldmlld3MaCFtSZXZpZXddQL7h8wFIwuX0AWI/EABiOwoGYXV0aG9yGgRVc2VyQIa/9QFI/tP1AWIZCgJpZBoDSUQhQNyc9gFIrLH2AWoEVXNlcmoGUmV2aWV3Yj8QAWI7CgZhdXRob3IaBFVzZXJAju32AUiO9/YBYhkKAmlkGgNJRCFA7Jz3AUiGpfcBagRVc2VyagZSZXZpZXdqB1Byb2R1Y3RiaxABYmcKB3Jldmlld3MaCFtSZXZpZXddQKrG9wFIsuP3AWI/EABiOwoGYXV0aG9yGgRVc2VyQKD99wFI0pb4AWIZCgJpZBoDSUQhQISr+AFIssL4AWoEVXNlcmoGUmV2aWV3agdQcm9kdWN0YmsQAmJnCgdyZXZpZXdzGghbUmV2aWV3XUDG5fgBSKSB+QFiPxAAYjsKBmF1dGhvchoEVXNlckDilvkBSNqw+QFiGQoCaWQaA0lEIUDqwvkBSKLf+QFqBFVzZXJqBlJldmlld2oHUHJvZHVjdGIqEANiJgoHcmV2aWV3cxoIW1Jldmlld11A8v35AUjMiPoBagdQcm9kdWN0agVRdWVyefkBAAAAAAAA8D8="
            }
        })))
        .mount(&server)
        .await;

    // accounts: federation `_entities` fetch over User. Returns names.
    // Captured from `https://accounts.demo.starstuff.dev/` — the
    // subgraph that triggered the ECONNRESET in the CircleCI failure
    // (apollographql/router job 377214, ROUTER-1814).
    Mock::given(method("POST"))
        .and(path("/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "_entities": [
                    {"name": "Ada Lovelace"},
                    {"name": "Alan Turing"},
                ]
            },
            "extensions": {
                "ftv1": "GgsIgv280AYQwPP2NCILCIL9vNAGEMDhgjNY3JDaAXJyYnAKCV9lbnRpdGllcxoKW19FbnRpdHldIUCE/dQBSOqn2AFiIhAAYh4KBG5hbWUaBlN0cmluZ0DUuNcBSPru1wFqBFVzZXJiIhABYh4KBG5hbWUaBlN0cmluZ0DgidgBSNCV2AFqBFVzZXJqBVF1ZXJ5+QEAAAAAAADwPw=="
            }
        })))
        .mount(&server)
        .await;

    server
}

/// Routes the three demo subgraph URLs to a localhost wiremock instead of
/// the public `https://*.demo.starstuff.dev/` hosts. Returns canned
/// federation responses (including valid FTV1 traces) so the OTel trace
/// shape matches existing snapshots without off-box network egress.
///
/// Introduced to fix ROUTER-1814: `test_send_variable_value` flaked on
/// Linux CI when the accounts demo subgraph reset the TLS connection
/// (`ECONNRESET` / `os error 104`). See the
/// `start_demo_subgraphs_mock_server` doc comment for the broader root
/// cause.
async fn get_router_service_with_subgraph_mock(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    use_legacy_request_span: bool,
    _mocked: bool,
) -> (JoinHandle<()>, BoxCloneService) {
    let (task, mut config) = config(use_legacy_request_span, false, reports).await;

    let subgraph_mock = start_demo_subgraphs_mock_server().await;
    let mock_url = subgraph_mock.uri();
    // Leak so the wiremock outlives this helper's return — the harness
    // hangs on to the returned router service, which will fire requests
    // at the mock during the test body. Same pattern as
    // `start_connector_mock_server`.
    let _ = Box::leak(Box::new(subgraph_mock));

    // Wire in `override_subgraph_url` so the router rewrites the
    // hardcoded `https://*.demo.starstuff.dev/` URIs to our localhost
    // mock paths. The trailing path segment per subgraph is how the
    // wiremock distinguishes which canned response to return.
    if let Some(obj) = config.as_object_mut() {
        obj.insert(
            "override_subgraph_url".to_string(),
            serde_json::json!({
                "accounts": format!("{mock_url}/accounts"),
                "products": format!("{mock_url}/products"),
                "reviews": format!("{mock_url}/reviews"),
            }),
        );
    }

    let builder = TestHarness::builder()
        .try_log_level("INFO")
        .configuration_json(config)
        .expect("test harness had config errors")
        .schema(include_str!("fixtures/supergraph.graphql"))
        .with_subgraph_network_requests();
    (
        task,
        builder
            .build_router()
            .await
            .expect("could create router test harness"),
    )
}

async fn get_connector_router_service(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    use_legacy_request_span: bool,
    mocked: bool,
) -> (JoinHandle<()>, BoxCloneService) {
    let (task, mut config) = config(use_legacy_request_span, false, reports).await;

    // Stand up a localhost wiremock to replace the real
    // `https://jsonplaceholder.typicode.com/` egress the connector schema
    // hardcodes. The server is leaked so its lifetime matches the test
    // process (these tests are serialised at the binary level).
    let connector_mock = start_connector_mock_server().await;
    let mock_url = connector_mock.uri();
    // Leak so the server stays alive for the duration of the test (and
    // any other test in this binary) without us threading a guard through
    // the BoxCloneService return type.
    let _ = Box::leak(Box::new(connector_mock));

    // Inject `connectors.sources.<subgraph>.<sourceName>.override_url`
    // entries so the runtime rewrites the connector source baseURL to the
    // mock. The runtime override propagates into the `connect` span's
    // `apollo.connector.source.detail` attribute, so that attribute is
    // redacted by `assert_report!` (see the `redacted_attributes` list)
    // to keep the snapshot hermetic across the random port wiremock
    // chooses.
    if let Some(obj) = config.as_object_mut() {
        let connectors = obj
            .entry("connectors".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let sources = connectors
            .as_object_mut()
            .expect("connectors must be an object")
            .entry("sources".to_string())
            .or_insert_with(|| serde_json::json!({}));
        let sources_obj = sources
            .as_object_mut()
            .expect("connectors.sources must be an object");
        // Subgraph name is `posts` (see `enum join__Graph` in
        // tests/fixtures/supergraph_connect.graphql).
        sources_obj.insert(
            "posts.jsonPlaceholder".to_string(),
            serde_json::json!({"override_url": format!("{mock_url}/")}),
        );
        sources_obj.insert(
            "posts.routerHealth".to_string(),
            serde_json::json!({"override_url": format!("{mock_url}/")}),
        );
    }

    let builder = TestHarness::builder()
        .try_log_level("INFO")
        .configuration_json(config)
        .expect("test harness had config errors")
        .schema(include_str!("fixtures/supergraph_connect.graphql"));
    let builder = if mocked {
        builder.subgraph_hook(|subgraph, _service| tracing_common::subgraph_mocks(subgraph))
    } else {
        builder.with_subgraph_network_requests()
    };
    (
        task,
        builder
            .build_router()
            .await
            .expect("could create router test harness"),
    )
}

async fn get_batch_router_service(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    use_legacy_request_span: bool,
    mocked: bool,
) -> (JoinHandle<()>, BoxCloneService) {
    let (task, config) = config(use_legacy_request_span, true, reports).await;
    let builder = TestHarness::builder()
        .try_log_level("INFO")
        .configuration_json(config)
        .expect("test harness had config errors")
        .schema(include_str!("fixtures/supergraph.graphql"));
    let builder = if mocked {
        builder.subgraph_hook(|subgraph, _service| tracing_common::subgraph_mocks(subgraph))
    } else {
        builder.with_subgraph_network_requests()
    };
    (
        task,
        builder
            .build_router()
            .await
            .expect("could create router test harness"),
    )
}

/// Attribute keys whose *value* is legitimately non-deterministic across
/// runs (wall-clock durations, randomly-chosen mock ports, per-process ids,
/// live trace/client identifiers, etc). `assert_report!` below replaces the
/// value of every one of these keys with a fixed `"[redacted]"` placeholder
/// before the snapshot compare, so two spans that differ *only* in one of
/// these attributes render byte-for-byte identical YAML.
///
/// This is a single source of truth shared by `assert_report!` (which
/// redacts these for display) and `sort_spans_for_snapshot` (which must
/// ignore these same keys when building its ordering key below) — the two
/// lists drifting apart is exactly how a "fixed" sort quietly regresses: if
/// a new volatile attribute is redacted here but not excluded from the sort
/// key (or vice versa), the sort can start keying off run-to-run noise
/// again.
const VOLATILE_ATTRIBUTE_KEYS: &[&str] = &[
    "apollo.client.host",
    "apollo.client.uname",
    "apollo.router.id",
    "apollo.schema.id",
    "apollo.user.agent",
    "apollo_private.duration_ns",
    "apollo_private.ftv1",
    "apollo_private.graphql.variables",
    "apollo_private.http.response_headers",
    "apollo_private.sent_time_offset",
    "trace_id",
    "graphql.error.path",
    "apollo.connector.source.detail",
];

/// Canonicalise span ordering inside an `ExportTraceServiceRequest` so insta
/// snapshots are stable across runs.
///
/// **Why this exists.** The OTLP HTTP exporter ships spans in the order the
/// tracing-opentelemetry layer hands them to the batch span processor, which
/// is the order their associated tokio task drops the `EnteredSpan` guard.
/// Under `flavor = "multi_thread"` two sibling spans (e.g. `parse_query`
/// scheduled on the compute-job pool and the supergraph-side `compute_job` /
/// `compute_job.execution` spans on a different worker) can finish in either
/// relative order, which permutes the `spans` vec the test asserts on. The
/// snapshot content is byte-for-byte identical otherwise — only the array
/// ordering changes — so the cure is to canonicalise the order before
/// asserting. See blog-details.md / T10 (non-deterministic ordering).
///
/// **Why the previous `(start_time, end_time, name)` key was insufficient.**
/// The prior version of this function sorted top-level roots by walking
/// `spans_by_root.keys().cloned().collect()` straight out of a `HashMap` —
/// Rust's default hasher is randomly seeded per process, so whenever two
/// roots *tied* on `(start_time, end_time, name)` (exactly the case for
/// `test_batch_trace_id`, whose two batched operations produce two
/// structurally-identical `supergraph` root spans that can start/end within
/// the same nanosecond in a fast CI sandbox), the pre-sort order — and
/// therefore the stable-sort tiebreak outcome — depended on hash-iteration
/// order, not on any content. Separately, the sibling-level tiebreak used
/// `original_position_within_parent`, i.e. this span's index in the
/// as-received batch — exactly the tokio-drop-order arrival sequence this
/// function exists to cancel out in the first place. Both tiebreaks were
/// "deterministic" only in the trivial sense of being a pure function of
/// already-nondeterministic inputs, so ties could still flip the rendered
/// snapshot from run to run.
///
/// **Fix — keep `(start_time, end_time)` as the primary key, but replace the
/// tiebreak with a content signature.** An earlier draft of this fix made
/// the *entire* key content-based (name + attributes + children, no
/// timestamps at all). That is a valid total order, but it throws away the
/// real chronological signal for the (overwhelmingly common) case where
/// sibling timestamps are NOT tied, reordering siblings that were never
/// ambiguous in the first place and churning almost every snapshot in this
/// file. Instead, `(start_time_unix_nano, end_time_unix_nano)` stays the
/// primary key — unchanged from before, so non-tied cases keep today's
/// order — and a bottom-up content `signature` (name + own attributes,
/// sorted by key with `VOLATILE_ATTRIBUTE_KEYS` stripped out, + the sorted
/// signatures of its children) is used purely as the tiebreak, replacing
/// both the old HashMap-iteration-order root tiebreak and the old
/// arrival-position sibling tiebreak. `span_id`, `trace_id`,
/// `parent_span_id` never enter the key (fresh random bytes every run); the
/// volatile attributes are excluded from the signature for the same reason
/// `assert_report!` redacts them to a fixed placeholder for display — their
/// real value carries run-to-run noise, not information the snapshot can
/// see.
///
/// **Why leftover ties are safe.** If two distinct spans/subtrees still tie
/// on the full `(start, end, signature)` key, every field that ends up in
/// the snapshot is provably equal between them (that is what the signature
/// covers), so swapping their relative order cannot change the rendered
/// YAML. The only thing that can break such a tie is `Vec::sort_by`'s
/// stable fallback to pre-sort order, which is fine here precisely because
/// a tie at this point is a genuine "no observable difference," not an
/// artifact of measuring the wrong thing.
fn sort_spans_for_snapshot(report: &mut ExportTraceServiceRequest) {
    use std::collections::HashMap;

    use opentelemetry_proto::tonic::trace::v1::Span;

    // Cap on recursion depth while computing signatures. A real trace tree
    // won't exceed a few dozen levels of nesting; this defends against
    // pathological parent_span_id cycles that shouldn't exist but would
    // otherwise recurse forever.
    const MAX_DEPTH: usize = 128;

    fn attr_key_is_volatile(key: &str) -> bool {
        VOLATILE_ATTRIBUTE_KEYS.contains(&key)
    }

    /// Bottom-up content signature for the span at `idx`, used only as a
    /// tiebreak after `(start_time, end_time)`. Memoized so a span that
    /// were ever reachable via more than one path (shouldn't happen in a
    /// well-formed tree) is only computed once.
    fn signature_of(
        idx: usize,
        original: &[Span],
        children_of: &HashMap<Vec<u8>, Vec<usize>>,
        memo: &mut HashMap<usize, String>,
        depth: usize,
    ) -> String {
        if let Some(sig) = memo.get(&idx) {
            return sig.clone();
        }
        let span = &original[idx];

        let mut attrs: Vec<String> = span
            .attributes
            .iter()
            .filter(|kv| !attr_key_is_volatile(&kv.key))
            .map(|kv| format!("{}={:?}", kv.key, kv.value))
            .collect();
        attrs.sort();

        let mut child_sigs: Vec<String> = if depth >= MAX_DEPTH {
            // Defensive: bail out of what would otherwise be a cycle-driven
            // infinite recursion. Degrades to a leaf signature.
            Vec::new()
        } else if let Some(children) = children_of.get(&span.span_id) {
            children
                .iter()
                .map(|&c| signature_of(c, original, children_of, memo, depth + 1))
                .collect()
        } else {
            Vec::new()
        };
        child_sigs.sort();

        let sig = format!(
            "name={}|kind={:?}|status={:?}|attrs=[{}]|children=[{}]",
            span.name,
            span.kind,
            span.status,
            attrs.join(","),
            child_sigs.join(";")
        );
        memo.insert(idx, sig.clone());
        sig
    }

    /// Total order key for span `idx`: real chronology first, content
    /// signature only as the tiebreak.
    fn sort_key<'a>(
        idx: usize,
        original: &[Span],
        signatures: &'a [String],
    ) -> (u64, u64, &'a str) {
        let span = &original[idx];
        (
            span.start_time_unix_nano,
            span.end_time_unix_nano,
            signatures[idx].as_str(),
        )
    }

    /// Iterative DFS (avoids blowing the stack on pathological trees),
    /// visiting `idx` then its `sort_key`-sorted children. `visited` guards
    /// against cycles re-emitting a span.
    fn dfs(
        root: usize,
        original: &[Span],
        children_of: &HashMap<Vec<u8>, Vec<usize>>,
        signatures: &[String],
        visited: &mut [bool],
        ordered: &mut Vec<usize>,
    ) {
        let mut stack = vec![root];
        while let Some(idx) = stack.pop() {
            if visited[idx] {
                continue;
            }
            visited[idx] = true;
            ordered.push(idx);
            if let Some(children) = children_of.get(&original[idx].span_id) {
                let mut kids = children.clone();
                kids.sort_by_key(|&i| sort_key(i, original, signatures));
                // Push in reverse so they pop off the stack in sorted order.
                stack.extend(kids.into_iter().rev());
            }
        }
    }

    for resource_spans in &mut report.resource_spans {
        for scope_spans in &mut resource_spans.scope_spans {
            // Take the spans out so we can re-insert them in canonical order.
            let original: Vec<Span> = std::mem::take(&mut scope_spans.spans);
            if original.is_empty() {
                continue;
            }

            // Stable index from span_id -> position in `original`.
            let id_to_idx: HashMap<Vec<u8>, usize> = original
                .iter()
                .enumerate()
                .map(|(i, s)| (s.span_id.clone(), i))
                .collect();

            // children_of[parent_span_id] = child indices. Only ever read
            // via `.get(span_id)` below, never iterated wholesale, so this
            // HashMap's randomized iteration order can't leak into the
            // output ordering the way the old `spans_by_root.keys()`
            // collect did.
            let mut children_of: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
            for (idx, span) in original.iter().enumerate() {
                if !span.parent_span_id.is_empty() && id_to_idx.contains_key(&span.parent_span_id) {
                    children_of
                        .entry(span.parent_span_id.clone())
                        .or_default()
                        .push(idx);
                }
            }

            // A span is a root if it has no parent, or its parent isn't
            // present in this batch (defensive — keeps stray spans visible
            // instead of silently dropped).
            let mut roots: Vec<usize> = (0..original.len())
                .filter(|&i| {
                    let parent = &original[i].parent_span_id;
                    parent.is_empty() || !id_to_idx.contains_key(parent)
                })
                .collect();

            let mut memo: HashMap<usize, String> = HashMap::new();
            let signatures: Vec<String> = (0..original.len())
                .map(|i| signature_of(i, &original, &children_of, &mut memo, 0))
                .collect();

            roots.sort_by_key(|&i| sort_key(i, &original, &signatures));

            let mut visited = vec![false; original.len()];
            let mut ordered: Vec<usize> = Vec::with_capacity(original.len());
            for &root in &roots {
                dfs(
                    root,
                    &original,
                    &children_of,
                    &signatures,
                    &mut visited,
                    &mut ordered,
                );
            }

            // Safety net: any span not reached from a root (only possible
            // via a parent_span_id cycle, which shouldn't occur in real
            // traces) is appended in sort-key order so spans are never
            // silently dropped from the snapshot.
            let mut leftover: Vec<usize> = (0..original.len()).filter(|&i| !visited[i]).collect();
            leftover.sort_by_key(|&i| sort_key(i, &original, &signatures));
            ordered.extend(leftover);

            // Reconstruct the spans vec in the canonical order. Wrap each
            // span in Option so we can `.take()` it exactly once even if
            // the input contains duplicate span_ids (which would be a bug,
            // but the sort shouldn't silently drop spans on its behalf).
            let mut slots: Vec<Option<Span>> = original.into_iter().map(Some).collect();
            scope_spans.spans = ordered
                .into_iter()
                .filter_map(|idx| slots[idx].take())
                .collect();
        }
    }
}

macro_rules! assert_report {
        ($report: expr)=> {
            assert_report!($report, false)
        };
        ($report: expr, $batch: literal)=> {
            // Take ownership locally so we can canonicalise span ordering
            // without forcing every call site to declare `let mut report`.
            // Without the sort, the OTLP exporter's spans vec is permuted
            // by tokio-task drop order — see `sort_spans_for_snapshot`.
            let mut report = $report;
            sort_spans_for_snapshot(&mut report);
            insta::with_settings!({sort_maps => true}, {
                    insta::assert_yaml_snapshot!(report, {
                        ".**.attributes" => insta::sorted_redaction(),
                        ".**.attributes[]" => insta::dynamic_redaction(|mut value, _| {
                            // Shared with `sort_spans_for_snapshot`'s content
                            // signature (see `VOLATILE_ATTRIBUTE_KEYS`) so the
                            // two lists can't drift apart: an attribute
                            // redacted here for display must also be excluded
                            // from the pre-snapshot sort key, or its
                            // run-to-run-random value would leak back into
                            // span ordering.
                            let mut redacted_attributes = VOLATILE_ATTRIBUTE_KEYS.to_vec();
                            if $batch {
                                redacted_attributes.append(&mut vec![
                                "apollo_private.operation_signature",
                                "graphql.operation.name"
                            ]);
                            }
                            if let insta::internals::Content::Struct(name, key_value)  = &mut value{
                                if name == &"KeyValue" {
                                    if redacted_attributes.contains(&key_value[0].1.as_str().unwrap()) {
                                        key_value[1].1 = insta::internals::Content::NewtypeVariant(
                                            "Value", 0, "stringValue", Box::new(insta::internals::Content::from("[redacted]"))
                                        );
                                    }
                                }
                            }
                            value
                        }),
                        ".resourceSpans[].scopeSpans[].scope.version" => "[version]",
                        ".**.traceId" => "[trace_id]",
                        ".**.spanId" => "[span_id]",
                        ".**.parentSpanId" => "[span_id]",
                        ".**.startTimeUnixNano" => "[start_time]",
                        ".**.endTimeUnixNano" => "[end_time]",
                        ".**.timeUnixNano" => "[time]",
                    });
                });
        }
    }

pub(crate) mod plugins {
    pub(crate) mod telemetry {
        pub(crate) mod apollo_exporter {
            use serde::ser::SerializeStruct;

            pub(crate) fn serialize_timestamp<S>(
                timestamp: &Option<prost_types::Timestamp>,
                serializer: S,
            ) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                match timestamp {
                    Some(ts) => {
                        let mut ts_strukt = serializer.serialize_struct("Timestamp", 2)?;
                        ts_strukt.serialize_field("seconds", &ts.seconds)?;
                        ts_strukt.serialize_field("nanos", &ts.nanos)?;
                        ts_strukt.end()
                    }
                    None => serializer.serialize_none(),
                }
            }
        }
    }
}

async fn traces_handler(
    Extension(state): Extension<Arc<Mutex<Vec<ExportTraceServiceRequest>>>>,
    bytes: Bytes,
) -> Result<Json<()>, http::StatusCode> {
    // Note OTel exporter via HTTP isn't using compression.
    // dbg!(base64::encode(&*bytes));  // useful for debugging with a protobuf parser
    if let Ok(traces_request) = ExportTraceServiceRequest::decode(&*bytes) {
        state.lock().await.push(traces_request);
        // Seems like we always receive some other unparseable data before receiving the request.
        // Maybe it's a handshake or something but not sure.
    }
    Ok(Json(()))
}

/// Routes all subgraph traffic through a localhost wiremock instead of
/// the public `https://*.demo.starstuff.dev/` hosts, making every caller
/// hermetic against live-network flakes (ECONNRESET / os error 104/10054).
/// See `start_demo_subgraphs_mock_server` and `ROUTER-1878`.
async fn get_trace_report_with_subgraph_mock(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    request: router::Request,
    use_legacy_request_span: bool,
) -> ExportTraceServiceRequest {
    get_traces(
        get_router_service_with_subgraph_mock,
        reports,
        use_legacy_request_span,
        false,
        request,
        |r| {
            !r.resource_spans
                .first()
                .expect("resource spans required")
                .scope_spans
                .first()
                .expect("scope spans required")
                .spans
                .is_empty()
        },
    )
    .await
}

async fn get_connector_trace_report(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    request: router::Request,
    use_legacy_request_span: bool,
) -> ExportTraceServiceRequest {
    get_traces(
        get_connector_router_service,
        reports,
        use_legacy_request_span,
        false,
        request,
        |r| {
            !r.resource_spans
                .first()
                .expect("resource spans required")
                .scope_spans
                .first()
                .expect("scope spans required")
                .spans
                .is_empty()
        },
    )
    .await
}

async fn get_batch_trace_report(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    request: router::Request,
    use_legacy_request_span: bool,
) -> ExportTraceServiceRequest {
    get_traces(
        get_batch_router_service,
        reports,
        use_legacy_request_span,
        false,
        request,
        |r| {
            !r.resource_spans
                .first()
                .expect("resource spans required")
                .scope_spans
                .first()
                .expect("scope spans required")
                .spans
                .is_empty()
        },
    )
    .await
}

async fn get_traces<
    Fut,
    T: Fn(&&ExportTraceServiceRequest) -> bool + Send + Sync + Copy + 'static,
>(
    service_fn: impl FnOnce(Arc<Mutex<Vec<ExportTraceServiceRequest>>>, bool, bool) -> Fut,
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    use_legacy_request_span: bool,
    mocked: bool,
    request: router::Request,
    filter: T,
) -> ExportTraceServiceRequest
where
    Fut: Future<Output = (JoinHandle<()>, BoxCloneService)>,
{
    reports.lock().await.clear();
    let (task, mut service) = service_fn(reports.clone(), use_legacy_request_span, mocked).await;
    let started_at = Instant::now();
    let response = service
        .ready()
        .await
        .expect("router service was never ready")
        .call(request)
        .await
        .expect("router service call failed");

    // Drain the response. We capture the body verbatim (or its decode error) so the
    // post-deadline panic below can report whether the request succeeded, returned
    // GATEWAY_TIMEOUT, etc. — the matcher's "no matching report" message is
    // ambiguous between (a) router never produced a trace and (b) router produced
    // a trace whose shape didn't match the filter, and the request body
    // disambiguates the two.
    let response_body: Result<String, String> = match response
        .response
        .into_body()
        .collect()
        .await
        .map(|b| String::from_utf8(b.to_bytes().to_vec()))
    {
        Ok(Ok(body)) => {
            if body.contains("errors") {
                eprintln!("response had errors {body}");
            }
            Ok(body)
        }
        Ok(Err(utf8_err)) => Err(format!("response body was not valid UTF-8: {utf8_err}")),
        Err(collect_err) => Err(format!("failed to drain response body: {collect_err}")),
    };

    // Poll until a report passes `filter`. The 10 s deadline was set when Phase 1
    // of the de-flaking effort widened the window from `10 × 100 ms` (≈1 s) to
    // give CI plenty of slack for normal export latency. If we hit it anyway, the
    // problem is almost always upstream of the OTLP exporter (the request itself
    // didn't complete, the matcher doesn't fit the trace shape the router
    // produced, etc.) — see the diagnostic panic below.
    let deadline = Instant::now() + Duration::from_secs(10);
    let found_report;
    loop {
        let my_reports = reports.lock().await;
        if let Some(report) = my_reports.iter().find(filter) {
            found_report = report.clone();
            break;
        }
        if Instant::now() >= deadline {
            // Build a diagnostic that turns "timed out" into something actionable.
            // Per-report shape: resource_spans / scope_spans / total spans, plus the
            // distinct span names we saw — enough to tell at a glance whether the
            // problem is "no spans at all" (request timed out / never traced) vs.
            // "wrong spans" (filter mismatch) vs. "right spans but on a later
            // batch we never waited long enough for" (rare; deadline too short).
            let summary: Vec<String> = my_reports
                .iter()
                .enumerate()
                .map(|(idx, r)| {
                    let resource_count = r.resource_spans.len();
                    let scope_count: usize =
                        r.resource_spans.iter().map(|rs| rs.scope_spans.len()).sum();
                    let total_spans: usize = r
                        .resource_spans
                        .iter()
                        .flat_map(|rs| &rs.scope_spans)
                        .map(|ss| ss.spans.len())
                        .sum();
                    let mut names: Vec<&str> = r
                        .resource_spans
                        .iter()
                        .flat_map(|rs| &rs.scope_spans)
                        .flat_map(|ss| &ss.spans)
                        .map(|s| s.name.as_str())
                        .collect();
                    names.sort();
                    names.dedup();
                    format!(
                        "[{idx}] resource_spans={resource_count} scope_spans={scope_count} \
                         total_spans={total_spans} span_names={names:?}"
                    )
                })
                .collect();
            let report_count = my_reports.len();
            drop(my_reports);
            let elapsed = started_at.elapsed();
            let body_summary = match &response_body {
                Ok(body) if body.contains("errors") => {
                    let snippet: String = body.chars().take(400).collect();
                    format!("response had errors (first 400 chars): {snippet}")
                }
                Ok(body) => {
                    let snippet: String = body.chars().take(200).collect();
                    format!("response ok (first 200 chars): {snippet}")
                }
                Err(e) => format!("response unavailable: {e}"),
            };
            panic!(
                "timed out waiting for matching trace report after {elapsed:?} \
                 (deadline 10s); reports collected: {report_count}; \
                 {body_summary}; report shapes: {summary:?}"
            );
        }
        drop(my_reports);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());

    found_report
}

#[tokio::test(flavor = "multi_thread")]
async fn connector() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{posts{id body title}}")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report = get_connector_trace_report(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connector_error() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{posts{id body title forceError}}")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report = get_connector_trace_report(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn non_defer() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_condition_if() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query($if: Boolean!) {topProducts {  name    ... @defer(if: $if) {  reviews {    author {      name    }  }  reviews {    author {      name    }  }    }}}")
            .variable("if", true)
            .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_condition_else() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
        .query("query($if: Boolean!) {topProducts {  name    ... @defer(if: $if) {  reviews {    author {      name    }  }  reviews {    author {      name    }  }    }}}")
        .variable("if", false)
        .header(ACCEPT, "multipart/mixed;deferSpec=20220824")
        .build()
        .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_trace_id() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_trace_id() {
    for use_legacy_request_span in [true, false] {
        let request = make_fake_batch(
            supergraph::Request::fake_builder()
                .query("query one {topProducts{name reviews {author{name}} reviews{author{name}}}}")
                .operation_name("one")
                .build()
                .unwrap()
                .supergraph_request,
            Some(("one", "two")),
        );
        let reports = Arc::new(Mutex::new(vec![]));
        let report = get_batch_trace_report(reports, request.into(), use_legacy_request_span).await;
        assert_report!(report, true);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_client_name() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .header("apollographql-client-name", "my client")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_client_version() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .header("apollographql-client-version", "my client version")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_send_header() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .header("send-header", "Header value")
            .header("dont-send-header", "Header value")
            .build()
            .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_send_header() {
    for use_legacy_request_span in [true, false] {
        let request = make_fake_batch(
            supergraph::Request::fake_builder()
                .query("query one {topProducts{name reviews {author{name}} reviews{author{name}}}}")
                .operation_name("one")
                .header("send-header", "Header value")
                .header("dont-send-header", "Header value")
                .build()
                .unwrap()
                .supergraph_request,
            Some(("one", "two")),
        );
        let reports = Arc::new(Mutex::new(vec![]));
        let report = get_batch_trace_report(reports, request.into(), use_legacy_request_span).await;
        assert_report!(report, true);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_send_variable_value() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
        .query("query($sendValue:Boolean!, $dontSendValue: Boolean!){topProducts{name reviews @include(if: $sendValue) {author{name}} reviews @include(if: $dontSendValue){author{name}}}}")
        .variable("sendValue", true)
        .variable("dontSendValue", true)
        .build()
        .unwrap();
        let req: router::Request = request.try_into().expect("could not convert request");
        let reports = Arc::new(Mutex::new(vec![]));
        let report =
            get_trace_report_with_subgraph_mock(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}
