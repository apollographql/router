//! Execute a query plan with a `Sequence` nested under a `Parallel`, over a large response.
//!
//! The query below plans as `Sequence[products, Parallel[Sequence[reviews, users], prices]]`:
//! each product's review comes from `reviews`, the review's author from `users` (which needs the
//! review ids), and the price from `prices`, next to them.
//!
//! Run with `cargo bench -p apollo-router-benchmarks --bench nested_sequence`

use apollo_router::services::router;
use apollo_router::services::subgraph;
use apollo_router::services::supergraph;
use apollo_router::TestHarness;
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::Criterion;
use serde_json_bytes::json;
use serde_json_bytes::Value;
use tower::ServiceExt;

const PRODUCTS: usize = 1000;

static SCHEMA: &str = include_str!("fixtures/nested_sequence.graphql");

static QUERY: &str = "{ topProducts { upc name price review { id author } } }";

/// Answers each `_entities` representation with `entity(representation)`.
fn entities(request: &subgraph::Request, entity: impl Fn(&Value) -> Value) -> Value {
    let representations = request
        .subgraph_request
        .body()
        .variables
        .get("representations")
        .and_then(Value::as_array)
        .expect("an entities fetch");
    json!({ "_entities": representations.iter().map(entity).collect::<Vec<_>>() })
}

fn subgraph_service(name: &str) -> subgraph::BoxCloneService {
    let name = name.to_string();
    tower::service_fn(move |request: subgraph::Request| {
        let data = match name.as_str() {
            "products" => json!({
                "topProducts": (0..PRODUCTS)
                    .map(|i| json!({
                        "__typename": "Product",
                        "upc": i.to_string(),
                        "name": format!("Product {i}"),
                    }))
                    .collect::<Vec<_>>(),
            }),
            "reviews" => entities(&request, |product| {
                json!({ "review": { "__typename": "Review", "id": product["upc"].clone() } })
            }),
            "users" => entities(&request, |review| {
                json!({ "author": format!("Author {}", review["id"].as_str().unwrap()) })
            }),
            "prices" => entities(&request, |product| {
                json!({ "price": product["upc"].as_str().unwrap().len() })
            }),
            other => panic!("unexpected subgraph {other}"),
        };
        std::future::ready(Ok(subgraph::Response::fake_builder()
            .data(data)
            .context(request.context)
            .build()))
    })
    .boxed_clone()
}

async fn router() -> router::BoxCloneService {
    TestHarness::builder()
        .schema(SCHEMA)
        .subgraph_hook(|name, _| subgraph_service(name))
        .build_router()
        .await
        .unwrap()
}

async fn request(router: router::BoxCloneService) -> impl AsRef<[u8]> {
    let request = supergraph::Request::fake_builder()
        .query(QUERY)
        .build()
        .unwrap()
        .try_into()
        .unwrap();
    router
        .oneshot(request)
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap()
}

fn nested_sequence(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let router = runtime.block_on(router());

    let response = runtime.block_on(request(router.clone()));
    let response: serde_json::Value = serde_json::from_slice(response.as_ref()).unwrap();
    assert!(response.get("errors").is_none(), "{response}");
    assert_eq!(
        response["data"]["topProducts"][PRODUCTS - 1],
        serde_json::json!({
            "upc": (PRODUCTS - 1).to_string(),
            "name": format!("Product {}", PRODUCTS - 1),
            "price": (PRODUCTS - 1).to_string().len(),
            "review": {
                "id": (PRODUCTS - 1).to_string(),
                "author": format!("Author {}", PRODUCTS - 1),
            },
        })
    );

    c.bench_function("nested_sequence", |b| {
        b.to_async(&runtime).iter(|| request(router.clone()));
    });
}

criterion_group!(benches, nested_sequence);
criterion_main!(benches);
