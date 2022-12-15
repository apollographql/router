use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use apollo_router::services::router;
use apollo_router::services::router::BoxService;
use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use axum::routing::post;
use axum::Extension;
use axum::Json;
use bytes::Bytes;
use flate2::read::GzDecoder;
use http::header::ACCEPT;
use prost::Message;
use tokio::sync::Mutex;
use tower::Service;
use tower::ServiceExt;
use tower_http::decompression::DecompressionLayer;

use crate::proto::reports::Report;

macro_rules! assert_report {
        ($report: expr)=> {
            insta::with_settings!({sort_maps => true}, {
                    insta::assert_yaml_snapshot!($report, {
                        ".header.hostname" => "[hostname]",
                        ".header.uname" => "[uname]",
                        ".**.seconds" => "[seconds]",
                        ".**.nanos" => "[nanos]",
                        ".**.duration_ns" => "[duration_ns]",
                        ".**.child[].start_time" => "[start_time]",
                        ".**.child[].end_time" => "[end_time]",
                        ".**.trace_id.value[]" => "[trace_id]",
                        ".**.sent_time_offset" => "[sent_time_offset]",
                        ".**.my_trace_id" => "[my_trace_id]"
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
    bytes: Bytes,
    Extension(state): Extension<Arc<Mutex<Vec<Report>>>>,
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
    router_service: &mut BoxService,
    reports: Arc<Mutex<Vec<Report>>>,
    request: supergraph::Request,
) -> Report {
    let req: router::Request = request.try_into().expect("could not convert request");

    let mut response = router_service
        .ready()
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();
    // Drain the response
    while response.next_response().await.is_some() {}

    let mut found_report = None;
    for _ in 0..100 {
        let reports = reports.lock().await;
        let report = reports.iter().find(|r| !r.traces_per_query.is_empty());
        if report.is_some() {
            found_report = report.cloned();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    reports.lock().await.clear();

    found_report.expect("could not find report")
}

#[tokio::test(flavor = "multi_thread")]
async fn test_all() {
    let reports: Arc<Mutex<Vec<Report>>> = Default::default();
    std::env::set_var("APOLLO_KEY", "test");
    std::env::set_var("APOLLO_GRAPH_REF", "test");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route("/", post(report))
        .layer(DecompressionLayer::new())
        .layer(tower_http::add_extension::AddExtensionLayer::new(
            reports.clone(),
        ));

    let mut config: serde_json::Value =
        serde_yaml::from_str(include_str!("fixtures/apollo_reports.router.yaml"))
            .expect("apollo_reports.router.yaml was invalid");
    config = jsonpath_lib::replace_with(config, "$.telemetry.apollo.endpoint", &mut |_| {
        Some(serde_json::Value::String(format!("http://{}", addr)))
    })
    .expect("Could not sub in endpoint");

    let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        axum::Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service())
            .with_graceful_shutdown(async {
                server_shutdown_rx.await.ok();
            })
            .await
            .expect("could not start axum server")
    });

    let mut router_service = TestHarness::builder()
        .configuration_json(config)
        .unwrap()
        .with_subgraph_network_requests()
        .build_router()
        .await
        .expect("could create router test harness")
        .boxed();

    non_defer(&mut router_service, reports.clone()).await;
    test_condition_if(&mut router_service, reports.clone()).await;
    test_condition_else(&mut router_service, reports.clone()).await;
    test_trace_id(&mut router_service, reports.clone()).await;
    test_client_name(&mut router_service, reports.clone()).await;
    test_client_version(&mut router_service, reports.clone()).await;
    test_send_header(&mut router_service, reports.clone()).await;
    test_send_variable_value(&mut router_service, reports.clone()).await;

    server_shutdown_tx.send(()).unwrap();
}

async fn non_defer(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;
    assert_report!(report);
}

async fn test_condition_if(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query($if: Boolean!) {topProducts {  name    ... @defer(if: $if) {  reviews {    author {      name    }  }  reviews {    author {      name    }  }    }}}")
        .variable("if", true)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;
    assert_report!(report);
}

async fn test_condition_else(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query($if: Boolean!) {topProducts {  name    ... @defer(if: $if) {  reviews {    author {      name    }  }  reviews {    author {      name    }  }    }}}")
        .variable("if", false)
        .header(ACCEPT, "multipart/mixed; deferSpec=20220824")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;
    assert_report!(report);
}

async fn test_trace_id(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;

    assert_report!(report);
}

async fn test_client_name(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .header("apollographql-client-name", "my client")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;

    assert_report!(report);
}

async fn test_client_version(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .header("apollographql-client-version", "my client version")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;

    assert_report!(report);
}

async fn test_send_header(router_service: &mut BoxService, reports: Arc<Mutex<Vec<Report>>>) {
    let request = supergraph::Request::fake_builder()
        .query("query{topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .header("send-header", "Header value")
        .header("dont-send-header", "Header value")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;

    assert_report!(report);
}

async fn test_send_variable_value(
    router_service: &mut BoxService,
    reports: Arc<Mutex<Vec<Report>>>,
) {
    let request = supergraph::Request::fake_builder()
        .query("query($send-variable-value: String!){topProducts{name reviews {author{name}} reviews{author{name}}}}")
        .variable("send-value", "Variable value")
        .variable("dont-send-value", "Variable value")
        .build()
        .unwrap();
    let report = get_trace_report(router_service, reports, request).await;

    assert_report!(report);
}
