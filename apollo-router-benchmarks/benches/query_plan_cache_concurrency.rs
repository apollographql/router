//! Benchmark: concurrent cache-hit throughput through the query plan cache.
//!
//! Sweeps concurrency from 1 to 64 to produce an Amdahl's-law curve. Each
//! data point shows how much of the per-request work is parallelisable when
//! the plan is already cached.
//!
//! - Old implementation: every cache hit acquired the `wait_map` mutex, allocated
//!   a `broadcast::Sender`, and spawned a cleanup task — all immediately discarded.
//!   The wait_map contention serializes concurrent hits, flattening the speedup curve.
//! - New implementation: a fast path checks the in-memory LRU before acquiring the
//!   wait_map mutex. On a hit the value is returned immediately; the mutex is only
//!   entered on a miss where deduplication is actually needed.
//!
//! Run with: `cargo bench -p apollo-router-benchmarks --bench query_plan_cache_concurrency`

use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::Throughput;
use futures::future::join_all;
use serde_json::json;
use tower::Service;
use tower::ServiceExt;

const QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

const CONCURRENCY_LEVELS: &[usize] = &[1, 2, 4, 8, 16, 32, 64];

fn build_harness() -> TestHarness<'static> {
    let account_service = MockSubgraph::builder()
        .with_json(
            json!{{
                "query": "query TopProducts__accounts__3($representations:[_Any!]!){_entities(representations:$representations){...on User{name}}}",
                "operationName": "TopProducts__accounts__3",
                "variables": {
                    "representations": [
                        { "__typename": "User", "id": "1" },
                        { "__typename": "User", "id": "2" }
                    ]
                }
            }},
            json!{{ "data": { "_entities": [{ "name": "Ada Lovelace" }, { "name": "Alan Turing" }] } }},
        )
        .build();

    let review_service = MockSubgraph::builder()
        .with_json(
            json!{{
                "query": "query($representations: [_Any!]!) { _entities(representations: $representations) { ... on Product { reviews { id product { __typename upc } author { __typename id } } } } }",
                "variables": {
                    "representations": [
                        { "__typename": "Product", "upc": "1" },
                        { "__typename": "Product", "upc": "2" }
                    ]
                }
            }},
            json!{{
                "data": {
                    "_entities": [
                        { "reviews": [
                            { "id": "1", "product": { "__typename": "Product", "upc": "1" }, "author": { "__typename": "User", "id": "1" } },
                            { "id": "4", "product": { "__typename": "Product", "upc": "1" }, "author": { "__typename": "User", "id": "2" } }
                        ]},
                        { "reviews": [
                            { "id": "2", "product": { "__typename": "Product", "upc": "2" }, "author": { "__typename": "User", "id": "1" } }
                        ]}
                    ]
                }
            }},
        )
        .build();

    let product_service = MockSubgraph::builder()
        .with_json(
            json!{{
                "query": "query TopProducts__products__0($first:Int){topProducts(first:$first){__typename upc name}}",
                "operationName": "TopProducts__products__0",
                "variables": { "first": 2u8 }
            }},
            json!{{
                "data": {
                    "topProducts": [
                        { "__typename": "Product", "upc": "1", "name": "Table" },
                        { "__typename": "Product", "upc": "2", "name": "Couch" }
                    ]
                }
            }},
        )
        .with_json(
            json!{{
                "query": "query TopProducts__products__2($representations:[_Any!]!){_entities(representations:$representations){...on Product{name}}}",
                "operationName": "TopProducts__products__2",
                "variables": {
                    "representations": [
                        { "__typename": "Product", "upc": "1" },
                        { "__typename": "Product", "upc": "2" }
                    ]
                }
            }},
            json!{{ "data": { "_entities": [{ "name": "Table" }, { "name": "Couch" }] } }},
        )
        .build();

    let mut mocks = MockedSubgraphs::default();
    mocks.insert("accounts", account_service);
    mocks.insert("reviews", review_service);
    mocks.insert("products", product_service);

    TestHarness::builder()
        .try_log_level("warn")
        .schema(include_str!("fixtures/supergraph.graphql"))
        .extra_plugin(mocks)
}

fn make_request() -> router::Request {
    supergraph::Request::fake_builder()
        .query(QUERY.to_string())
        .variable("first", 2usize)
        .build()
        .expect("valid request")
        .try_into()
        .expect("valid router request")
}

async fn send_request(mut svc: router::BoxCloneService) {
    svc.ready()
        .await
        .expect("service ready")
        .call(make_request())
        .await
        .expect("request succeeded")
        .next_response()
        .await
        .expect("response body")
        .expect("valid response");
}

fn bench_cache_hit_concurrency(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let router = rt.block_on(async {
        let svc = build_harness().build_router().await.expect("router built");
        // Prime the plan cache before any timing starts.
        send_request(svc.clone()).await;
        svc
    });

    let mut group = c.benchmark_group("query_plan_cache_concurrency");
    for &concurrency in CONCURRENCY_LEVELS {
        group.throughput(Throughput::Elements(concurrency as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(concurrency),
            &concurrency,
            |b, &concurrency| {
                b.to_async(&rt).iter(|| async {
                    let tasks: Vec<_> = (0..concurrency)
                        .map(|_| tokio::spawn(send_request(router.clone())))
                        .collect();
                    join_all(tasks).await;
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_cache_hit_concurrency);
criterion_main!(benches);
