use crate::proto::reports::Report;
use apollo_router::services::{router, supergraph};
use apollo_router::TestHarness;
use axum::routing::post;
use axum::{Extension, Json};
use bytes::Bytes;
use flate2::read::GzDecoder;
use http::header::ACCEPT;
use http::HeaderValue;
use prost::Message;
use serde_json::json;
use serial_test::serial;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;
use tower_http::decompression::DecompressionLayer;
use tracing::{info_span, Instrument};

macro_rules! assert_report {
        ($report: expr)=> {
            insta::with_settings!({sort_maps => true}, {
                    insta::assert_yaml_snapshot!($report, {
                        ".**.seconds" => "[seconds]",
                        ".**.nanos" => "[nanos]",
                        ".**.duration_ns" => "[duration_ns]",
                        ".**.child[].start_time" => "[start_time]",
                        ".**.child[].end_time" => "[end_time]",
                        ".**.trace_id.value[]" => "[trace_id]",
                        ".**.sent_time_offset" => "[sent_time_offset]"
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
    pub(crate) mod server {
        tonic::include_proto!("server");
    }
}

async fn report(
    bytes: Bytes,
    Extension(state): Extension<Arc<Mutex<Vec<Report>>>>,
) -> Result<Json<()>, http::StatusCode> {
    let mut gz = GzDecoder::new(&*bytes);
    let mut buf = Vec::new();
    gz.read_to_end(&mut buf).unwrap();
    let report = Report::decode(&*buf).unwrap();
    state.lock().unwrap().push(report);
    Ok(Json(()))
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn test_condition_if() {
    let reports: Arc<Mutex<Vec<Report>>> = Arc::new(Mutex::new(Vec::new()));
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

    let _ = tokio::spawn(async move {
        axum::Server::from_tcp(listener)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap()
    });
    let config = json!({"telemetry":{"tracing":{"trace_config":{"sampler": "always_on"}},"apollo":{"endpoint":format!("http://{}", addr), "batch_processor":{"scheduled_delay": "1ms"}, "field_level_instrumentation_sampler": "always_on"}}});
    let request = supergraph::Request::fake_builder()
        .query("query($if: Boolean!) {\n  topProducts {\n    name\n      ... @defer(if: $if) {\n    reviews {\n      author {\n        name\n      }\n    }\n    reviews {\n      author {\n        name\n      }\n    }\n      }\n  }\n}")
        .variable("if", true)
        .header("Accept", "multipart/mixed; deferSpec=20220824")
        .build()
        .unwrap();
    let mut req: router::Request = request.try_into().unwrap();
    req.router_request.headers_mut().insert(
        ACCEPT,
        HeaderValue::from_static("multipart/mixed; deferSpec=20220824"),
    );

    {
        let router = TestHarness::builder()
            .configuration_json(config)
            .unwrap()
            .build_router()
            .await
            .unwrap();

        let mut response = router.oneshot(req).await.unwrap();
        while response.next_response().await.is_some() {
            println!("got chunk");
        }
    }

    tokio::time::sleep(Duration::from_millis(10000)).await;
    println!("{}", reports.lock().unwrap().len());
    assert_report!(reports.lock().unwrap().get(0).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_condition_else() {
    // The following curl request was used to generate this span data
    // curl --request POST \
    //     --header 'content-type: application/json' \
    //     --header 'accept: multipart/mixed; deferSpec=20220824, application/json' \
    //     --url http://localhost:4000/ \
    //     --data '{"query":"query($if: Boolean!) {\n  topProducts {\n    name\n      ... @defer(if: $if) {\n    reviews {\n      author {\n        name\n      }\n    }\n    reviews {\n      author {\n        name\n      }\n    }\n      }\n  }\n}","variables":{"if":false}}'
    // let spandata = include_str!("testdata/condition_else_spandata.yaml");
    // let exporter = Exporter::test_builder().build();
    // let report = report(exporter, spandata).await;
    // assert_report!(report);
}

#[tokio::test]
async fn test_trace_id() {
    // let spandata = include_str!("testdata/condition_if_spandata.yaml");
    // let exporter = Exporter::test_builder()
    //     .expose_trace_id_config(ExposeTraceId {
    //         enabled: true,
    //         header_name: Some(HeaderName::from_static("trace_id")),
    //     })
    //     .build();
    // let report = report(exporter, spandata).await;
    // assert_report!(report);
}
