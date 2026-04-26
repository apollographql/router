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
//! Run with: `cargo bench --bench query_plan_cache_concurrency`

use std::time::Instant;

use apollo_router::plugin::test::MockSubgraph;
use apollo_router::services::router;
use apollo_router::services::supergraph;
use apollo_router::MockedSubgraphs;
use apollo_router::TestHarness;
use futures::future::join_all;
use serde_json::json;
use tower::Service;
use tower::ServiceExt;

const QUERY: &str = r#"query TopProducts($first: Int) { topProducts(first: $first) { upc name reviews { id product { name } author { id name } } } }"#;

/// Concurrency levels to sweep. Powers of two give a clean log-scale curve.
const CONCURRENCY_LEVELS: &[usize] = &[1, 2, 4, 8, 16, 32, 64];

/// Minimum total requests per concurrency level. Low-concurrency levels (c=1,
/// c=2) get more waves to ensure enough wall time for a stable measurement.
/// High-concurrency levels saturate quickly, so the wave count is lower.
const MIN_REQUESTS: usize = 10_000;

/// Warmup waves before timing each concurrency level. Lets the tokio thread
/// pool and any per-level allocator state reach steady state before the clock
/// starts.
const WARMUP_WAVES: usize = 100;

/// Repetitions per concurrency level. The median req/s is reported, which
/// discards outlier runs caused by OS scheduler preemption.
const REPS: usize = 3;

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
        .unwrap()
}

async fn send_request(mut svc: router::BoxCloneService) {
    svc.ready()
        .await
        .unwrap()
        .call(make_request())
        .await
        .unwrap()
        .next_response()
        .await
        .unwrap()
        .unwrap();
}

struct Sample {
    concurrency: usize,
    rps: f64,
    elapsed_secs: f64,
    speedup: f64,
}

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let router = rt.block_on(async {
        let svc = build_harness().build_router().await.unwrap();
        // Prime the plan cache before any timing starts.
        send_request(svc.clone()).await;
        svc
    });

    // Establish the serial baseline at concurrency=1 first, then reuse it as
    // the denominator for all speedup calculations.
    let mut baseline_rps = 0.0_f64;
    let mut samples: Vec<Sample> = Vec::new();

    for &concurrency in CONCURRENCY_LEVELS.iter() {
        let waves = (MIN_REQUESTS + concurrency - 1) / concurrency; // ceil div
        let total = waves * concurrency;

        // Warmup: let the tokio pool and allocator reach steady state.
        rt.block_on(async {
            for _ in 0..WARMUP_WAVES {
                let tasks: Vec<_> = (0..concurrency)
                    .map(|_| tokio::spawn(send_request(router.clone())))
                    .collect();
                join_all(tasks).await;
            }
        });

        // Timed repetitions — take the median req/s.
        let mut rps_samples: Vec<f64> = (0..REPS)
            .map(|_| {
                let start = Instant::now();
                rt.block_on(async {
                    for _ in 0..waves {
                        let tasks: Vec<_> = (0..concurrency)
                            .map(|_| tokio::spawn(send_request(router.clone())))
                            .collect();
                        join_all(tasks).await;
                    }
                });
                total as f64 / start.elapsed().as_secs_f64()
            })
            .collect();
        rps_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rps = rps_samples[REPS / 2]; // median

        if concurrency == 1 {
            baseline_rps = rps;
            samples.push(Sample {
                concurrency,
                rps,
                elapsed_secs: total as f64 / rps,
                speedup: 1.0,
            });
        } else {
            samples.push(Sample {
                concurrency,
                rps,
                elapsed_secs: total as f64 / rps,
                speedup: rps / baseline_rps,
            });
        }
    }

    // ── Report ───────────────────────────────────────────────────────────────
    println!("Query plan cache — concurrency sweep");
    println!("  Same query repeated, all cache hits | min {MIN_REQUESTS} reqs/level, {REPS} reps (median reported), {WARMUP_WAVES} warmup waves");
    println!("  Serial fraction estimate uses Amdahl's law: S = (1/speedup - 1/N) / (1 - 1/N)");
    println!();
    println!(
        "  {:<12} {:>10} {:>12} {:>10} {:>14}",
        "concurrency", "req/s", "elapsed (s)", "speedup", "serial frac."
    );
    println!("  {}", "-".repeat(62));

    for s in &samples {
        // Amdahl serial fraction: S = (1/speedup - 1/N) / (1 - 1/N)
        let serial_frac = if s.concurrency == 1 {
            1.0
        } else {
            let n = s.concurrency as f64;
            (1.0 / s.speedup - 1.0 / n) / (1.0 - 1.0 / n)
        };
        println!(
            "  {:<12} {:>10.0} {:>12.3} {:>9.2}× {:>13.1}%",
            s.concurrency,
            s.rps,
            s.elapsed_secs,
            s.speedup,
            serial_frac * 100.0,
        );
    }

    println!();
    println!("  Interpretation");
    println!("  ──────────────");
    println!("  A flat serial-fraction column means the bottleneck is consistent");
    println!("  across concurrency levels (Amdahl's law holds cleanly).");
    println!("  Rising serial fraction at high concurrency signals a new contention");
    println!("  point emerging — e.g. the in-memory LRU lock or tokio scheduler");
    println!("  overhead — that didn't exist at lower concurrency.");
    println!();
    println!("  Under the old wait_map path, every cache hit serialized through");
    println!("  the wait_map mutex, so the speedup curve would have been flat near 1×");
    println!("  and the serial fraction near 100% regardless of concurrency level.");
}
