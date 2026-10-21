//! Exercises response handling (merging subgraph responses, formatting and serializing the final
//! response) with large synthetic subgraph responses.
//!
//! Run with `cargo bench -p apollo-router-benchmarks --bench response_handling`

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use apollo_router::graphql;
use apollo_router::services::router;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::BenchmarkId;
use criterion::Criterion;
use serde_json::json;
use serde_json::Value;
use tower::BoxError;
use tower::ServiceExt;

// The plan for this query nests a `Sequence` (reviews -> accounts/products) inside a `Parallel`
// (next to the inventory fetch that `@requires` product fields).
static QUERY: &str = r#"query Bench($first: Int) { topProducts(first: $first) { upc name price inStock shippingEstimate reviews { id body author { id name username } product { upc name } } } }"#;

const REVIEWS_PER_PRODUCT: usize = 10;
const USERS: usize = 100;

fn entity(subgraph: &str, representation: &Value) -> Value {
    let id = representation
        .get("upc")
        .or_else(|| representation.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match subgraph {
        "products" => json!({ "name": format!("Product {id}") }),
        "inventory" => json!({ "inStock": true, "shippingEstimate": 42 }),
        "accounts" => json!({ "name": format!("User {id}") }),
        "reviews" => {
            let upc: usize = id.parse().unwrap_or_default();
            let reviews: Vec<Value> = (0..REVIEWS_PER_PRODUCT)
                .map(|r| {
                    let review_id = upc * REVIEWS_PER_PRODUCT + r;
                    json!({
                        "id": review_id.to_string(),
                        "body": format!("Review body number {review_id} with some extra text to make it longer"),
                        "author": {
                            "__typename": "User",
                            "id": (review_id % USERS).to_string(),
                            "username": format!("user{}", review_id % USERS),
                        },
                        "product": { "__typename": "Product", "upc": id },
                    })
                })
                .collect();
            json!({ "reviews": reviews })
        }
        other => panic!("unexpected subgraph {other}"),
    }
}

fn subgraph_data(subgraph: &str, request: &graphql::Request) -> Value {
    if let Some(representations) = request.variables.get("representations") {
        let representations: Vec<Value> = serde_json::to_value(representations)
            .and_then(serde_json::from_value)
            .expect("representations is an array");
        let entities: Vec<Value> = representations
            .iter()
            .map(|r| entity(subgraph, r))
            .collect();
        return json!({ "_entities": entities });
    }
    assert_eq!(subgraph, "products");
    let first = request
        .variables
        .get("first")
        .and_then(|v| v.as_u64())
        .expect("first is set") as usize;
    let products: Vec<Value> = (0..first)
        .map(|upc| {
            json!({
                "__typename": "Product",
                "upc": upc.to_string(),
                "name": format!("Product {upc}"),
                "price": 100 + upc,
                "weight": 10 + upc,
            })
        })
        .collect();
    json!({ "topProducts": products })
}

async fn build_router() -> router::BoxCloneService {
    let schema = include_str!("fixtures/supergraph.graphql");
    // Generating the synthetic data is not what we want to measure, so it is done once per
    // distinct subgraph request and cloned afterwards.
    let cache: Arc<Mutex<HashMap<String, serde_json_bytes::Value>>> = Default::default();
    TestHarness::builder()
        .try_log_level("error")
        .schema(schema)
        .subgraph_hook(move |name, _service| {
            let name: Arc<str> = name.into();
            let cache = cache.clone();
            tower::service_fn(move |request: subgraph::Request| {
                let name = name.clone();
                let cache = cache.clone();
                async move {
                    let body = request.subgraph_request.body();
                    let key = format!("{name}:{}", serde_json::to_string(body).unwrap());
                    let data = cache
                        .lock()
                        .unwrap()
                        .entry(key)
                        .or_insert_with(|| subgraph_data(&name, body).into())
                        .clone();
                    Ok::<_, BoxError>(
                        subgraph::Response::fake_builder()
                            .data(data)
                            .context(request.context)
                            .subgraph_name(name.to_string())
                            .build(),
                    )
                }
            })
            .boxed_clone()
        })
        .build_router()
        .await
        .expect("router builds")
}

async fn run(router_service: router::BoxCloneService, first: usize) -> usize {
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .variable("first", first)
        .build()
        .expect("expecting valid request")
        .try_into()
        .unwrap();
    let body = router_service
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap();
    body.len()
}

fn check_response(runtime: &tokio::runtime::Runtime, router_service: &router::BoxCloneService) {
    let request = supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .variable("first", 3)
        .build()
        .unwrap()
        .try_into()
        .unwrap();
    let body = runtime.block_on(async {
        router_service
            .clone()
            .oneshot(request)
            .await
            .unwrap()
            .next_response()
            .await
            .unwrap()
            .unwrap()
    });
    let response: Value = serde_json::from_slice(&body).unwrap();
    assert!(response.get("errors").is_none(), "{response:#}");
    let products = response["data"]["topProducts"].as_array().unwrap();
    assert_eq!(products.len(), 3);
    for (upc, product) in products.iter().enumerate() {
        assert_eq!(product["shippingEstimate"], 42, "{product:#}");
        assert_eq!(product["inStock"], true, "{product:#}");
        let reviews = product["reviews"].as_array().unwrap();
        assert_eq!(reviews.len(), REVIEWS_PER_PRODUCT);
        for review in reviews {
            assert_eq!(review["product"]["name"], format!("Product {upc}"));
            let author_id = review["author"]["id"].as_str().unwrap();
            assert_eq!(review["author"]["name"], format!("User {author_id}"));
        }
    }
}

fn response_handling(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let router_service = runtime.block_on(build_router());
    check_response(&runtime, &router_service);

    let mut group = c.benchmark_group("response_handling");
    for first in [10usize, 100, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(first), &first, |b, &first| {
            b.to_async(&runtime)
                .iter(|| run(router_service.clone(), first));
        });
    }
    group.finish();
}

criterion_group!(benches, response_handling);
criterion_main!(benches);
