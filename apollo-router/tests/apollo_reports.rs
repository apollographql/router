use std::io::Cursor;
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
use flate2::read::GzDecoder;
use http::header::ACCEPT;
use once_cell::sync::Lazy;
use prost::Message;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::sync::OnceCell;
use tower::Service;
use tower::ServiceExt;
use tower_http::decompression::DecompressionLayer;

use crate::proto::reports::Report;
use crate::proto::reports::Trace;

static REPORTS: Lazy<Arc<Mutex<Vec<Report>>>> = Lazy::new(Default::default);
static TEST: Lazy<Arc<Mutex<bool>>> = Lazy::new(Default::default);
static ROUTER_SERVICE_RUNTIME: Lazy<Arc<tokio::runtime::Runtime>> = Lazy::new(|| {
    Arc::new(tokio::runtime::Runtime::new().expect("must be able to create tokio runtime"))
});
static CONFIG: OnceCell<serde_json::Value> = OnceCell::const_new();
static ROUTER_SERVICE: Lazy<Arc<Mutex<Option<BoxCloneService>>>> = Lazy::new(Default::default);
static MOCKED_ROUTER_SERVICE: Lazy<Arc<Mutex<Option<BoxCloneService>>>> =
    Lazy::new(Default::default);

async fn config() -> serde_json::Value {
    CONFIG
        .get_or_init(|| async {
            std::env::set_var("APOLLO_KEY", "test");
            std::env::set_var("APOLLO_GRAPH_REF", "test");

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let app = axum::Router::new()
                .route("/", post(report))
                .layer(DecompressionLayer::new())
                .layer(tower_http::add_extension::AddExtensionLayer::new(
                    (*REPORTS).clone(),
                ));

            drop(ROUTER_SERVICE_RUNTIME.spawn(async move {
                axum::Server::from_tcp(listener)
                    .expect("mut be able to crete report receiver")
                    .serve(app.into_make_service())
                    .await
                    .expect("could not start axum server")
            }));

            let mut config: serde_json::Value =
                serde_yaml::from_str(include_str!("fixtures/apollo_reports.router.yaml"))
                    .expect("apollo_reports.router.yaml was invalid");
            config = jsonpath_lib::replace_with(config, "$.telemetry.apollo.endpoint", &mut |_| {
                Some(serde_json::Value::String(format!("http://{addr}")))
            })
            .expect("Could not sub in endpoint");
            config
        })
        .await
        .clone()
}

async fn get_router_service(mocked: bool) -> BoxCloneService {
    let lazy = if mocked {
        &MOCKED_ROUTER_SERVICE
    } else {
        &ROUTER_SERVICE
    };
    let mut router_service = lazy.lock().await;
    if router_service.is_none() {
        let config = config().await;
        let service = ROUTER_SERVICE_RUNTIME
            .spawn(async move {
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
                builder
                    .build_router()
                    .await
                    .expect("could create router test harness")
            })
            .await
            .expect("must be able to create router");
        *router_service = Some(service);
    }
    router_service
        .clone()
        .expect("router service must have got created")
}

fn subgraph_mocks(subgraph: &str) -> subgraph::BoxService {
    let builder = MockSubgraph::builder();
    // base64 FTV1 blobs were manually captured from un-mocked responses
    if subgraph == "products" {
        let ftv1 = "GgwIqZ34nwYQgKDdjAMiDAipnfifBhDAm6CMA1jDrixy/AFi+QEKC3RvcFByb2R1Y3RzGglbUHJvZHVjdF1A1cQYSKv9HmJEEABiHwoDdXBjGgdTdHJpbmchQPq1I0jxnSRqB1Byb2R1Y3RiHwoEbmFtZRoGU3RyaW5nQPXjJEi1nSVqB1Byb2R1Y3RiRBABYh8KA3VwYxoHU3RyaW5nIUDBmyZIib0magdQcm9kdWN0Yh8KBG5hbWUaBlN0cmluZ0DI8CZI+4YnagdQcm9kdWN0YkQQAmIfCgN1cGMaB1N0cmluZyFAiOMnSJL8J2oHUHJvZHVjdGIfCgRuYW1lGgZTdHJpbmdA95YoSP+tKGoHUHJvZHVjdGoFUXVlcnn5AQAAAAAAAPA/";
        builder.with_json(
            json!({"query": "{topProducts{__typename upc name}}"}),
            json!({
                "data": {"topProducts": [
                    {"__typename": "Product", "upc": "1", "name": "Table"},
                    {"__typename": "Product", "upc": "2", "name": "Couch"},
                    {"__typename": "Product", "upc": "3", "name": "Chair"}
                ]},
                "extensions": {"ftv1": process_ftv1(subgraph, ftv1)}
            }),
        )
    } else if subgraph == "reviews" {
        let ftv1 = "GgwIqZ34nwYQwJ6htQMiDAipnfifBhDAlae0A1j4mWxynQNimgMKCV9lbnRpdGllcxoKW19FbnRpdHldIUDBjhBIh+4VYqMBEABingEKB3Jldmlld3MaCFtSZXZpZXddQLvDGEichl5iOxAAYjcKBmF1dGhvchoEVXNlckDqjF9IrIdnYhcKAmlkGgNJRCFAttxnSJCAaGoEVXNlcmoGUmV2aWV3YjsQAWI3CgZhdXRob3IaBFVzZXJAqLVhSMiXaGIXCgJpZBoDSUQhQPK2aEiKyGhqBFVzZXJqBlJldmlld2oHUHJvZHVjdGJlEAFiYQoHcmV2aWV3cxoIW1Jldmlld11A2ZYdSOrxYmI7EABiNwoGYXV0aG9yGgRVc2VyQPKiY0j40mhiFwoCaWQaA0lEIUCc9GhIiINpagRVc2VyagZSZXZpZXdqB1Byb2R1Y3RiZRACYmEKB3Jldmlld3MaCFtSZXZpZXddQLX6WEjO2GRiOxAAYjcKBmF1dGhvchoEVXNlckCmhWVI4o1pYhcKAmlkGgNJRCFAyq5pSJi9aWoEVXNlcmoGUmV2aWV3agdQcm9kdWN0agVRdWVyefkBAAAAAAAA8D8=";
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
                "extensions": {"ftv1": process_ftv1(subgraph, ftv1)}
            })
        )
    } else if subgraph == "accounts" {
        let ftv1 = "GgwIqZ34nwYQwOSeygMiDAipnfifBhDA5J7KA1jSozhybGJqCglfZW50aXRpZXMaCltfRW50aXR5XSFAoMgfSNWMKmIgEABiHAoEbmFtZRoGU3RyaW5nQL6OLkjelC9qBFVzZXJiIBABYhwKBG5hbWUaBlN0cmluZ0DMwTFIkpAyagRVc2VyagVRdWVyefkBAAAAAAAA8D8=";
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
                "extensions": {"ftv1": process_ftv1(subgraph, ftv1)}
            })
        )
    } else {
        builder
    }
    .build()
    .boxed()
}

fn process_ftv1(subgraph: &str, ftv1: &str) -> String {
    let mut trace = Trace::decode(Cursor::new(base64::decode(ftv1).unwrap())).unwrap();
    let root = trace.root.as_mut().unwrap();
    if subgraph == "products" {
        let top_products = &mut root.child[0];
        top_products.error.push(Default::default());
        top_products.error.push(Default::default());
    } else if subgraph == "accounts" {
        let entities = &mut root.child[0];
        let entities_0 = &mut entities.child[0];
        let entities_0_name = &mut entities_0.child[0];
        entities_0_name.error.push(Default::default());
        entities_0_name.start_time = 1_000_000;
        entities_0_name.end_time = 1_002_000;
    }
    base64::encode(trace.encode_to_vec())
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

async fn get_trace_report(request: supergraph::Request) -> Report {
    get_report(false, request, |r| {
        let r = !r
            .traces_per_query
            .values()
            .next()
            .expect("traces and stats required")
            .trace
            .is_empty();
        r
    })
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

async fn get_metrics_report(request: supergraph::Request) -> Report {
    get_report(false, request, has_metrics).await
}

async fn get_metrics_report_mocked(request: supergraph::Request) -> Report {
    get_report(true, request, has_metrics).await
}

async fn get_report<T: Fn(&&Report) -> bool + Send + Sync + Copy + 'static>(
    mocked: bool,
    request: supergraph::Request,
    filter: T,
) -> Report {
    ROUTER_SERVICE_RUNTIME
        .spawn(async move {
            let mut found_report;
            {
                let _test_guard = TEST.lock().await;
                {
                    REPORTS.clone().lock().await.clear();
                }
                let req: router::Request = request.try_into().expect("could not convert request");

                let response = get_router_service(mocked)
                    .await
                    .ready()
                    .await
                    .expect("router service was never ready")
                    .call(req)
                    .await
                    .expect("router service call failed");

                // Drain the response
                found_report = match hyper::body::to_bytes(response.response.into_body())
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
                    let reports = REPORTS.lock().await;
                    let report = reports.iter().find(filter);
                    if report.is_some() {
                        if matches!(found_report, Ok(None)) {
                            found_report = Ok(report.cloned());
                        }
                        break;
                    }
                    drop(reports);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }

            found_report
        })
        .await
        .expect("failed to get report")
        .expect("failed to get report")
        .expect("failed to find report")
}
#[tokio::test(flavor = "multi_thread")]
async fn non_defer() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_condition_if() {
    let request = supergraph::Request::fake_builder()
        .query("query($if: Boolean!) {topProducts {  name    ... @defer(if: $if) {  reviews {    author {      name    }  }  reviews {    author {      name    }  }    }}}")
        .variable("if", true)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_condition_else() {
    let request = supergraph::Request::fake_builder()
        .query("query($if: Boolean!) {topProducts {  name    ... @defer(if: $if) {  reviews {    author {      name    }  }  reviews {    author {      name    }  }    }}}")
        .variable("if", false)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_trace_id() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_client_name() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .header("apollographql-client-name", "my client")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_client_version() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .header("apollographql-client-version", "my client version")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_send_header() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .header("send-header", "Header value")
        .header("dont-send-header", "Header value")
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_send_variable_value() {
    let request = supergraph::Request::fake_builder()
        .query("query($sendValue:Boolean!, $dontSendValue: Boolean!){topProducts{name reviews @include(if: $sendValue) {author{name}} reviews @include(if: $dontSendValue){author{name}}}}")
        .variable("sendValue", true)
        .variable("dontSendValue", true)
        .build()
        .unwrap();
    let report = get_trace_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stats() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let report = get_metrics_report(request).await;
    assert_report!(report);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stats_mocked() {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let report = get_metrics_report_mocked(request).await;
    let per_query = report.traces_per_query.values().next().unwrap();
    let stats = per_query.stats_with_context.first().unwrap();
    insta::with_settings!({sort_maps => true}, {
        insta::assert_yaml_snapshot!(stats, {
            ".query_latency_stats.latency_count" => "[latency_count]"
        });
    });
}
