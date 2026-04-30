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

async fn get_router_service(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
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

async fn get_connector_router_service(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    use_legacy_request_span: bool,
    mocked: bool,
) -> (JoinHandle<()>, BoxCloneService) {
    let (task, config) = config(use_legacy_request_span, false, reports).await;

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

macro_rules! assert_report {
        ($report: expr)=> {
            assert_report!($report, false)
        };
        ($report: expr, $batch: literal)=> {
            insta::with_settings!({sort_maps => true}, {
                    insta::assert_yaml_snapshot!($report, {
                        ".**.attributes" => insta::sorted_redaction(),
                        ".**.attributes[]" => insta::dynamic_redaction(|mut value, _| {
                            let mut redacted_attributes = vec![
                                "apollo.client.host",
                                "apollo.client.uname",
                                "apollo.router.id",
                                "apollo.schema.id",
                                "apollo.user.agent",
                                "apollo_private.duration_ns" ,
                                "apollo_private.ftv1",
                                "apollo_private.graphql.variables",
                                "apollo_private.http.response_headers",
                                "apollo_private.sent_time_offset",
                                "trace_id",
                                "graphql.error.path",
                            ];
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

async fn get_trace_report(
    reports: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    request: router::Request,
    use_legacy_request_span: bool,
) -> ExportTraceServiceRequest {
    get_traces(
        get_router_service,
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
        let report = get_trace_report(reports, req, use_legacy_request_span).await;
        assert_report!(report);
    }
}
