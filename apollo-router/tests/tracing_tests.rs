use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use insta::_macro_support::Content;
use insta::_macro_support::Redaction;
use serde_json::json;
use test_span::prelude::test_span;
use tower_service::Service;

macro_rules! assert_trace_snapshot {
    ($spans:expr) => {
        insta::assert_json_snapshot!($spans, {
              ".**.children.*.record.entries[]" => redact_dynamic()
        });
    };
}

async fn make_request(request: supergraph::Request) {
    let mut router = TestHarness::builder()
        .with_subgraph_network_requests()
        .configuration_json(json!({"telemetry": {
            "apollo": {
                "field_level_instrumentation_sampler": "always_off"
            }
        }
        }))
        .expect("configuration must be valid")
        .build_router()
        .await
        .expect("router");
    let response = router
        .call(request.try_into().expect("valid router request"))
        .await
        .expect("request must succeed");
    let body = response.response.into_body();
    let _ = hyper::body::to_bytes(body)
        .await
        .expect("body must be returned");
}

#[test_span(tokio::test)]
#[level(tracing::Level::ERROR)]
#[target(apollo_router=tracing::Level::DEBUG)]
async fn traced_basic_request() {
    make_request(
        supergraph::Request::fake_builder()
            .query(r#"{ topProducts { name name2:name } }"#)
            .build()
            .expect("valid request"),
    )
    .await;
    assert_trace_snapshot!(get_spans());
}

#[test_span(tokio::test)]
#[level(tracing::Level::ERROR)]
#[target(apollo_router=tracing::Level::DEBUG)]
async fn traced_basic_composition() {
    make_request(
        supergraph::Request::fake_builder()
            .query(
                r#"{ topProducts { upc name reviews {id product { name } author { id name } } } }"#,
            )
            .build()
            .expect("valid request"),
    )
    .await;
    assert_trace_snapshot!(get_spans());
}

#[test_span(tokio::test(flavor = "multi_thread"))]
#[level(tracing::Level::ERROR)]
#[target(apollo_router=tracing::Level::INFO)]
async fn variables() {
    make_request(
        supergraph::Request::fake_builder()
            .query(
                r#"query ExampleQuery($topProductsFirst: Int, $reviewsForAuthorAuthorId: ID!) {
                topProducts(first: $topProductsFirst) {
                    name
                    reviewsForAuthor(authorID: $reviewsForAuthorAuthorId) {
                        body
                        author {
                            id
                            name
                        }
                    }
                }
            }"#,
            )
            .build()
            .expect("valid request"),
    )
    .await;
    assert_trace_snapshot!(get_spans());
}

#[allow(unused)]
// Useful to redact request_id in snapshot because it's not determinist
fn redact_dynamic() -> Redaction {
    insta::dynamic_redaction(|value, _path| {
        if let Some(value_slice) = value.as_slice() {
            if value_slice
                .get(0)
                .and_then(|v| {
                    v.as_str().map(|s| {
                        [
                            "request.id",
                            "response_headers",
                            "trace_id",
                            "histogram.apollo_router_cache_miss_time",
                            "histogram.apollo_router_cache_hit_time",
                            "histogram.apollo_router_query_planning_time",
                        ]
                        .contains(&s)
                    })
                })
                .unwrap_or_default()
            {
                return Content::Seq(vec![
                    value_slice.get(0).unwrap().clone(),
                    Content::String("[REDACTED]".to_string()),
                ]);
            }
            if value_slice
                .get(0)
                .and_then(|v| {
                    v.as_str().map(|s| {
                        [
                            "apollo_private.sent_time_offset",
                            "apollo_private.duration_ns",
                        ]
                        .contains(&s)
                    })
                })
                .unwrap_or_default()
            {
                return Content::Seq(vec![value_slice.get(0).unwrap().clone(), Content::I64(0)]);
            }
        }
        value
    })
}
