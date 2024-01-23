//! Be aware that this test file contains some fairly flaky tests which embed a number of
//! assumptions about how traces and stats are reported to Apollo Studio.
//!
//! In particular:
//!  - There are timings (sleeps) which work as things are implemented right now, but
//!    may be sources of problems in the future.
//!
//!  - There is a global TEST lock which forces these tests to execute serially to stop router
//!    global tracing effect from breaking the tests. DO NOT BE TEMPTED to remove this TEST lock to
//!    try and speed things up (unless you have time and patience to re-work a lot of test code).
//!
//!  - There are assumptions about the different ways in which traces and metrics work. The main
//!    limitation with these tests is that you are unlikely to get a single report containing all the
//!    metrics that you need to make a test assertion. You might, but raciness in the way metrics are
//!    generated in the router means you probably won't. That's why the test `test_batch_stats` has
//!    its own stack of functions for testing and only tests that the total number of requests match.
//!
//! Summary: The dragons here are ancient and very evil. Do not attempt to take their treasure.
//!
use std::future::Future;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::router;
use apollo_router::services::router::BoxCloneService;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use axum::body::Bytes;
use axum::routing::post;
use axum::Extension;
use axum::Json;
use base64::prelude::BASE64_STANDARD;
use base64::Engine as _;
use flate2::read::GzDecoder;
use http::header::ACCEPT;
use once_cell::sync::Lazy;
use prost::Message;
use prost_types::Timestamp;
use proto::reports::trace::Node;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tower::Service;
use tower::ServiceExt;
use tower_http::decompression::DecompressionLayer;

use crate::proto::reports::trace::node::Id::Index;
use crate::proto::reports::trace::node::Id::ResponseName;
use crate::proto::reports::Report;
use crate::proto::reports::Trace;

static ROUTER_SERVICE_RUNTIME: Lazy<Arc<tokio::runtime::Runtime>> = Lazy::new(|| {
    Arc::new(tokio::runtime::Runtime::new().expect("must be able to create tokio runtime"))
});
static TEST: Lazy<Arc<Mutex<()>>> = Lazy::new(Default::default);

async fn config(
    use_legacy_request_span: bool,
    batch: bool,
    reports: Arc<Mutex<Vec<Report>>>,
) -> (JoinHandle<()>, serde_json::Value) {
    std::env::set_var("APOLLO_KEY", "test");
    std::env::set_var("APOLLO_GRAPH_REF", "test");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route("/", post(report))
        .layer(DecompressionLayer::new())
        .layer(tower_http::add_extension::AddExtensionLayer::new(reports));

    let task = ROUTER_SERVICE_RUNTIME.spawn(async move {
        axum::Server::from_tcp(listener)
            .expect("mut be able to create report receiver")
            .serve(app.into_make_service())
            .await
            .expect("could not start axum server")
    });

    let mut config: serde_json::Value = if batch {
        serde_yaml::from_str(include_str!("fixtures/apollo_reports_batch.router.yaml"))
            .expect("apollo_reports.router.yaml was invalid")
    } else {
        serde_yaml::from_str(include_str!("fixtures/apollo_reports.router.yaml"))
            .expect("apollo_reports.router.yaml was invalid")
    };
    config = jsonpath_lib::replace_with(config, "$.telemetry.apollo.endpoint", &mut |_| {
        Some(serde_json::Value::String(format!("http://{addr}")))
    })
    .expect("Could not sub in endpoint");
    config =
        jsonpath_lib::replace_with(config, "$.telemetry.spans.legacy_request_span", &mut |_| {
            Some(serde_json::Value::Bool(use_legacy_request_span))
        })
        .expect("Could not sub in endpoint");
    (task, config)
}

async fn get_router_service(
    reports: Arc<Mutex<Vec<Report>>>,
    use_legacy_request_span: bool,
    mocked: bool,
) -> (JoinHandle<()>, BoxCloneService) {
    let (task, config) = config(use_legacy_request_span, false, reports).await;
    let builder = TestHarness::builder()
        .try_log_level("INFO")
        .configuration_json(config)
        .expect("test harness had config errors")
        .schema(include_str!("fixtures/supergraph.graphql"));
    let builder = if mocked {
        builder.subgraph_hook(|subgraph, _service| subgraph_mocks(subgraph))
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
    reports: Arc<Mutex<Vec<Report>>>,
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
        builder.subgraph_hook(|subgraph, _service| subgraph_mocks(subgraph))
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

fn encode_ftv1(trace: Trace) -> String {
    BASE64_STANDARD.encode(trace.encode_to_vec())
}

macro_rules! assert_report {
        ($report: expr)=> {
            insta::with_settings!({sort_maps => true}, {
                    insta::assert_yaml_snapshot!($report, {
                        ".**.agent_version" => "[agent_version]",
                        ".**.executable_schema_id" => "[executable_schema_id]",
                        ".header.hostname" => "[hostname]",
                        ".header.uname" => "[uname]",
                        ".**.seconds" => "[seconds]",
                        ".**.nanos" => "[nanos]",
                        ".**.duration_ns" => "[duration_ns]",
                        ".**.child[].start_time" => "[start_time]",
                        ".**.child[].end_time" => "[end_time]",
                        ".**.trace_id.value[]" => "[trace_id]",
                        ".**.sent_time_offset" => "[sent_time_offset]",
                        ".**.my_trace_id" => "[my_trace_id]",
                        ".**.latency_count" => "[latency_count]",
                        ".**.cache_latency_count" => "[cache_latency_count]",
                        ".**.public_cache_ttl_count" => "[public_cache_ttl_count]",
                        ".**.private_cache_ttl_count" => "[private_cache_ttl_count]",
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

#[allow(unreachable_pub)]
pub(crate) mod proto {
    pub(crate) mod reports {
        #![allow(clippy::derive_partial_eq_without_eq)]
        tonic::include_proto!("reports");
    }
}

async fn report(
    Extension(state): Extension<Arc<Mutex<Vec<Report>>>>,
    bytes: Bytes,
) -> Result<Json<()>, http::StatusCode> {
    let mut gz = GzDecoder::new(&*bytes);
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf)
        .expect("could not decompress bytes");
    let report = Report::decode(&*buf).expect("could not deserialize report");

    state.lock().await.push(report);
    Ok(Json(()))
}

async fn get_trace_report(
    reports: Arc<Mutex<Vec<Report>>>,
    request: router::Request,
    use_legacy_request_span: bool,
) -> Report {
    get_report(
        get_router_service,
        reports,
        use_legacy_request_span,
        false,
        request,
        |r| {
            !r.traces_per_query
                .values()
                .next()
                .expect("traces and stats required")
                .trace
                .is_empty()
        },
    )
    .await
}

async fn get_batch_trace_report(
    reports: Arc<Mutex<Vec<Report>>>,
    request: router::Request,
    use_legacy_request_span: bool,
) -> Report {
    get_report(
        get_batch_router_service,
        reports,
        use_legacy_request_span,
        false,
        request,
        |r| {
            !r.traces_per_query
                .values()
                .next()
                .expect("traces and stats required")
                .trace
                .is_empty()
        },
    )
    .await
}

fn has_metrics(r: &&Report) -> bool {
    !r.traces_per_query
        .values()
        .next()
        .expect("traces and stats required")
        .stats_with_context
        .is_empty()
}

async fn get_metrics_report(reports: Arc<Mutex<Vec<Report>>>, request: router::Request) -> Report {
    get_report(
        get_router_service,
        reports,
        false,
        false,
        request,
        has_metrics,
    )
    .await
}

async fn get_batch_metrics_report(
    reports: Arc<Mutex<Vec<Report>>>,
    request: router::Request,
) -> u64 {
    get_batch_stats_report(reports, false, request, has_metrics).await
}

async fn get_metrics_report_mocked(
    reports: Arc<Mutex<Vec<Report>>>,
    request: router::Request,
) -> Report {
    get_report(
        get_router_service,
        reports,
        false,
        true,
        request,
        has_metrics,
    )
    .await
}

async fn get_report<Fut, T: Fn(&&Report) -> bool + Send + Sync + Copy + 'static>(
    service_fn: impl FnOnce(Arc<Mutex<Vec<Report>>>, bool, bool) -> Fut,
    reports: Arc<Mutex<Vec<Report>>>,
    use_legacy_request_span: bool,
    mocked: bool,
    request: router::Request,
    filter: T,
) -> Report
where
    Fut: Future<Output = (JoinHandle<()>, BoxCloneService)>,
{
    let _guard = TEST.lock().await;
    reports.lock().await.clear();
    let (task, mut service) = service_fn(reports.clone(), use_legacy_request_span, mocked).await;
    let response = service
        .ready()
        .await
        .expect("router service was never ready")
        .call(request)
        .await
        .expect("router service call failed");

    // Drain the response
    let mut found_report = match hyper::body::to_bytes(response.response.into_body())
        .await
        .map(|b| String::from_utf8(b.to_vec()))
    {
        Ok(Ok(response)) => {
            if response.contains("errors") {
                eprintln!("response had errors {response}");
            }
            Ok(None)
        }
        _ => Err(anyhow!("error retrieving response")),
    };

    // We must always try to find the report regardless of if the response had failures
    for _ in 0..10 {
        let my_reports = reports.lock().await;
        let report = my_reports.iter().find(filter);
        if report.is_some() && matches!(found_report, Ok(None)) {
            found_report = Ok(report.cloned());
            break;
        }
        drop(my_reports);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    task.abort();

    found_report
        .expect("failed to get report")
        .expect("failed to find report")
}

async fn get_batch_stats_report<T: Fn(&&Report) -> bool + Send + Sync + Copy + 'static>(
    reports: Arc<Mutex<Vec<Report>>>,
    mocked: bool,
    request: router::Request,
    filter: T,
) -> u64 {
    let _guard = TEST.lock().await;
    reports.lock().await.clear();
    let (task, mut service) = get_batch_router_service(reports.clone(), mocked, false).await;
    let response = service
        .ready()
        .await
        .expect("router service was never ready")
        .call(request)
        .await
        .expect("router service call failed");

    // Drain the response (and throw it away)
    let _found_report = hyper::body::to_bytes(response.response.into_body()).await;

    // Give the server a little time to export something
    // If this test fails, consider increasing this time.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut request_count = 0;

    // In a more ideal world we would have an implementation of `AddAssign<&reports::Report>
    // However we don't. Let's do the minimal amount of checking and ensure that at least the
    // number of requests can be tested. Clearly, this doesn't test all of the stats, but it's a
    // fairly reliable check and at least we are testing something.
    for report in reports.lock().await.iter().filter(filter) {
        let stats = &report
            .traces_per_query
            .values()
            .next()
            .expect("has something")
            .stats_with_context;
        request_count += stats[0].query_latency_stats.as_ref().unwrap().request_count;
    }
    task.abort();
    request_count
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_trace_id() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .build()
            .unwrap()
            .supergraph_request
            .map(|req| {
                // Modify the request so that it is a valid array of requests.
                let mut json_bytes = serde_json::to_vec(&req).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes.clone());
                result.push(b',');
                result.append(&mut json_bytes);
                result.push(b']');
                hyper::Body::from(result)
            });
        let reports = Arc::new(Mutex::new(vec![]));
        let report = get_batch_trace_report(reports, request.into(), use_legacy_request_span).await;
        assert_report!(report);
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_send_header() {
    for use_legacy_request_span in [true, false] {
        let request = supergraph::Request::fake_builder()
            .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
            .header("send-header", "Header value")
            .header("dont-send-header", "Header value")
            .build()
            .unwrap()
            .supergraph_request
            .map(|req| {
                // Modify the request so that it is a valid array of requests.
                let mut json_bytes = serde_json::to_vec(&req).unwrap();
                let mut result = vec![b'['];
                result.append(&mut json_bytes.clone());
                result.push(b',');
                result.append(&mut json_bytes);
                result.push(b']');
                hyper::Body::from(result)
            });
        let reports = Arc::new(Mutex::new(vec![]));
        let report = get_batch_trace_report(reports, request.into(), use_legacy_request_span).await;
        assert_report!(report);
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stats() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let req: router::Request = request.try_into().expect("could not convert request");
    let reports = Arc::new(Mutex::new(vec![]));
    let report = get_metrics_report(reports, req).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_batch_stats() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap()
        .supergraph_request
        .map(|req| {
            // Modify the request so that it is a valid array containing 2 requests.
            let mut json_bytes = serde_json::to_vec(&req).unwrap();
            let mut result = vec![b'['];
            result.append(&mut json_bytes.clone());
            result.push(b',');
            result.append(&mut json_bytes);
            result.push(b']');
            hyper::Body::from(result)
        });
    let reports = Arc::new(Mutex::new(vec![]));
    // We can't do a report assert here because we will probably have multiple reports which we
    // can't merge...
    // Let's call a function that enables us to at least assert that we received the correct number
    // of requests.
    let request_count = get_batch_metrics_report(reports, request.into()).await;
    assert_eq!(2, request_count);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stats_mocked() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let req: router::Request = request.try_into().expect("could not convert request");
    let reports = Arc::new(Mutex::new(vec![]));
    let report = get_metrics_report_mocked(reports, req).await;
    let per_query = report.traces_per_query.values().next().unwrap();
    let stats = per_query.stats_with_context.first().unwrap();
    insta::with_settings!({sort_maps => true}, {
        insta::assert_yaml_snapshot!(stats, {
            ".query_latency_stats.latency_count" => "[latency_count]"
        });
    });
}

fn subgraph_mocks(subgraph: &str) -> subgraph::BoxService {
    let builder = MockSubgraph::builder();
    // base64 FTV1 blobs were manually captured from un-mocked responses
    if subgraph == "products" {
        let trace = Trace {
            start_time: Some(Timestamp { seconds: 1677594281, nanos: 831000000 }),
            end_time: Some(Timestamp { seconds: 1677594281, nanos: 832000000 }),
            duration_ns: 726851,
            root: Some(
                Node {
                    original_field_name: "".into(),
                    r#type: "".into(),
                    parent_type: "".into(),
                    cache_policy: None,
                    start_time: 0,
                    end_time: 0,
                    error: vec![],
                    child: vec![
                        Node {
                            original_field_name: "".into(),
                            r#type: "[Product]".into(),
                            parent_type: "Query".into(),
                            cache_policy: None,
                            start_time: 402005,
                            end_time: 507563,
                            // Synthetic errors for testing error stats
                            error: vec![Default::default(), Default::default()],
                            child: vec![
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String!".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 580346,
                                            end_time: 593649,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("upc".into())),
                                        },
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 602613,
                                            end_time: 609973,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("name".into())),
                                        },
                                    ],
                                    id: Some(Index(0)),
                                },
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String!".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 626113,
                                            end_time: 630409,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("upc".into())),
                                        },
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 637000,
                                            end_time: 639867,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("name".into())),
                                        },
                                    ],
                                    id: Some(Index(1)),
                                },
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String!".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 651656,
                                            end_time: 654866,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("upc".into())),
                                        },
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 658295,
                                            end_time: 661247,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("name".into())),
                                        },
                                    ],
                                    id: Some(Index(2)),
                                },
                            ],
                            id: Some(ResponseName("topProducts".into())),
                        },
                    ],
                    id: None,
                },
            ),
            field_execution_weight: 1.0,
            ..Default::default()
        };
        builder.with_json(
            json!({"query": "{topProducts{__typename upc name}}"}),
            json!({
                "data": {"topProducts": [
                    {"__typename": "Product", "upc": "1", "name": "Table"},
                    {"__typename": "Product", "upc": "2", "name": "Couch"},
                    {"__typename": "Product", "upc": "3", "name": "Chair"}
                ]},
                "errors": [
                    {"message": "", "path": ["topProducts"]},
                    {"message": "", "path": ["topProducts"]},
                ],
                "extensions": {"ftv1": encode_ftv1(trace)}
            }),
        )
    } else if subgraph == "reviews" {
        let trace = Trace {
            start_time: Some(Timestamp { seconds: 1677594281, nanos: 915000000 }),
            end_time: Some(Timestamp { seconds: 1677594281, nanos: 917000000 }),
            duration_ns: 1772792,
            root: Some(
                Node {
                    original_field_name: "".into(),
                    r#type: "".into(),
                    parent_type: "".into(),
                    cache_policy: None,
                    start_time: 0,
                    end_time: 0,
                    error: vec![],
                    child: vec![
                        Node {
                            original_field_name: "".into(),
                            r#type: "[_Entity]!".into(),
                            parent_type: "Query".into(),
                            cache_policy: None,
                            start_time: 264001,
                            end_time: 358151,
                            error: vec![],
                            child: vec![
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "[Review]".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 401851,
                                            end_time: 1540892,
                                            error: vec![],
                                            child: vec![
                                                Node {
                                                    original_field_name: "".into(),
                                                    r#type: "".into(),
                                                    parent_type: "".into(),
                                                    cache_policy: None,
                                                    start_time: 0,
                                                    end_time: 0,
                                                    error: vec![],
                                                    child: vec![
                                                        Node {
                                                            original_field_name: "".into(),
                                                            r#type: "User".into(),
                                                            parent_type: "Review".into(),
                                                            cache_policy: None,
                                                            start_time: 1558122,
                                                            end_time: 1688492,
                                                            error: vec![],
                                                            child: vec![
                                                                Node {
                                                                    original_field_name: "".into(),
                                                                    r#type: "ID!".into(),
                                                                    parent_type: "User".into(),
                                                                    cache_policy: None,
                                                                    start_time: 1699382,
                                                                    end_time: 1703952,
                                                                    error: vec![],
                                                                    child: vec![],
                                                                    id: Some(ResponseName("id".into())),
                                                                },
                                                            ],
                                                            id: Some(ResponseName("author".into())),
                                                        },
                                                    ],
                                                    id: Some(Index(0)),
                                                },
                                                Node {
                                                    original_field_name: "".into(),
                                                    r#type: "".into(),
                                                    parent_type: "".into(),
                                                    cache_policy: None,
                                                    start_time: 0,
                                                    end_time: 0,
                                                    error: vec![],
                                                    child: vec![
                                                        Node {
                                                            original_field_name: "".into(),
                                                            r#type: "User".into(),
                                                            parent_type: "Review".into(),
                                                            cache_policy: None,
                                                            start_time: 1596072,
                                                            end_time: 1706952,
                                                            error: vec![],
                                                            child: vec![
                                                                Node {
                                                                    original_field_name: "".into(),
                                                                    r#type: "ID!".into(),
                                                                    parent_type: "User".into(),
                                                                    cache_policy: None,
                                                                    start_time: 1710962,
                                                                    end_time: 1713162,
                                                                    error: vec![],
                                                                    child: vec![],
                                                                    id: Some(ResponseName("id".into())),
                                                                },
                                                            ],
                                                            id: Some(ResponseName("author".into())),
                                                        },
                                                    ],
                                                    id: Some(Index(1)),
                                                },
                                            ],
                                            id: Some(ResponseName("reviews".into())),
                                        },
                                    ],
                                    id: Some(Index(0)),
                                },
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "[Review]".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 478041,
                                            end_time: 1620202,
                                            error: vec![],
                                            child: vec![
                                                Node {
                                                    original_field_name: "".into(),
                                                    r#type: "".into(),
                                                    parent_type: "".into(),
                                                    cache_policy: None,
                                                    start_time: 0,
                                                    end_time: 0,
                                                    error: vec![],
                                                    child: vec![
                                                        Node {
                                                            original_field_name: "".into(),
                                                            r#type: "User".into(),
                                                            parent_type: "Review".into(),
                                                            cache_policy: None,
                                                            start_time: 1626482,
                                                            end_time: 1714552,
                                                            error: vec![],
                                                            child: vec![
                                                                Node {
                                                                    original_field_name: "".into(),
                                                                    r#type: "ID!".into(),
                                                                    parent_type: "User".into(),
                                                                    cache_policy: None,
                                                                    start_time: 1718812,
                                                                    end_time: 1720712,
                                                                    error: vec![],
                                                                    child: vec![],
                                                                    id: Some(ResponseName("id".into())),
                                                                },
                                                            ],
                                                            id: Some(ResponseName("author".into())),
                                                        },
                                                    ],
                                                    id: Some(Index(0)),
                                                },
                                            ],
                                            id: Some(ResponseName("reviews".into())),
                                        },
                                    ],
                                    id: Some(Index(1)),
                                },
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "[Review]".into(),
                                            parent_type: "Product".into(),
                                            cache_policy: None,
                                            start_time: 1457461,
                                            end_time: 1649742,
                                            error: vec![],
                                            child: vec![
                                                Node {
                                                    original_field_name: "".into(),
                                                    r#type: "".into(),
                                                    parent_type: "".into(),
                                                    cache_policy: None,
                                                    start_time: 0,
                                                    end_time: 0,
                                                    error: vec![],
                                                    child: vec![
                                                        Node {
                                                            original_field_name: "".into(),
                                                            r#type: "User".into(),
                                                            parent_type: "Review".into(),
                                                            cache_policy: None,
                                                            start_time: 1655462,
                                                            end_time: 1722082,
                                                            error: vec![],
                                                            child: vec![
                                                                Node {
                                                                    original_field_name: "".into(),
                                                                    r#type: "ID!".into(),
                                                                    parent_type: "User".into(),
                                                                    cache_policy: None,
                                                                    start_time: 1726282,
                                                                    end_time: 1728152,
                                                                    error: vec![],
                                                                    child: vec![],
                                                                    id: Some(ResponseName("id".into())),
                                                                },
                                                            ],
                                                            id: Some(ResponseName("author".into())),
                                                        },
                                                    ],
                                                    id: Some(Index(0)),
                                                },
                                            ],
                                            id: Some(ResponseName("reviews".into())),
                                        },
                                    ],
                                    id: Some(Index(2)),
                                },
                            ],
                            id: Some(ResponseName("_entities".into())),
                        },
                    ],
                    id: None,
                },
            ),
            field_execution_weight: 1.0,
            ..Default::default()
        };
        builder.with_json(
            json!({
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on Product{reviews{author{__typename id}}}}}",
                "variables": {"representations": [
                    {"__typename": "Product", "upc": "1"},
                    {"__typename": "Product", "upc": "2"},
                    {"__typename": "Product", "upc": "3"},
                ]}
            }),
            json!({
                "data": {"_entities": [
                    {"reviews": [
                        {"author": {"__typename": "User", "id": "1"}},
                        {"author": {"__typename": "User", "id": "2"}},
                    ]},
                    {"reviews": [
                        {"author": {"__typename": "User", "id": "1"}},
                    ]},
                    {"reviews": [
                        {"author": {"__typename": "User", "id": "2"}},
                    ]}
                ]},
                "extensions": {"ftv1": encode_ftv1(trace)}
            })
        )
    } else if subgraph == "accounts" {
        let trace = Trace {
            start_time: Some(Timestamp { seconds: 1677594281, nanos: 961000000 }),
            end_time: Some(Timestamp { seconds: 1677594281, nanos: 961000000 }),
            duration_ns: 922066,
            root: Some(
                Node {
                    original_field_name: "".into(),
                    r#type: "".into(),
                    parent_type: "".into(),
                    cache_policy: None,
                    start_time: 0,
                    end_time: 0,
                    error: vec![],
                    child: vec![
                        Node {
                            original_field_name: "".into(),
                            r#type: "[_Entity]!".into(),
                            parent_type: "Query".into(),
                            cache_policy: None,
                            start_time: 517152,
                            end_time: 689749,
                            error: vec![],
                            child: vec![
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String".into(),
                                            parent_type: "User".into(),
                                            cache_policy: None,
                                            start_time: 1000000,
                                            end_time: 1002000,
                                            error: vec![],
                                            child: vec![],
                                            id: Some(ResponseName("name".into())),
                                        },
                                    ],
                                    id: Some(Index(0)),
                                },
                                Node {
                                    original_field_name: "".into(),
                                    r#type: "".into(),
                                    parent_type: "".into(),
                                    cache_policy: None,
                                    start_time: 0,
                                    end_time: 0,
                                    error: vec![],
                                    child: vec![
                                        Node {
                                            original_field_name: "".into(),
                                            r#type: "String".into(),
                                            parent_type: "User".into(),
                                            cache_policy: None,
                                            start_time: 811212,
                                            end_time: 821266,
                                            // Synthetic error for testing error stats
                                            error: vec![Default::default()],
                                            child: vec![],
                                            id: Some(ResponseName("name".into())),
                                        },
                                    ],
                                    id: Some(Index(1)),
                                },
                            ],
                            id: Some(ResponseName("_entities".into())),
                        },
                    ],
                    id: None,
                },
            ),
            field_execution_weight: 1.0,
            ..Default::default()
        };
        builder.with_json(
            json!({
                "query": "query($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "variables": {"representations": [
                    {"__typename": "User", "id": "1"},
                    {"__typename": "User", "id": "2"},
                ]}
            }),
            json!({
                "data": {"_entities": [
                    {"name": "Ada Lovelace"},
                    {"name": "Alan Turing"},
                ]},
                "errors": [
                    {"message": "", "path": ["_entities", 1, "name"]},
                ],
                "extensions": {"ftv1": encode_ftv1(trace)}
            })
        )
    } else {
        builder
    }
    .build()
    .boxed()
}
