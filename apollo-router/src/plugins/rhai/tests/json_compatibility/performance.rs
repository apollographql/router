//! Performance of each conversion bridge for the workloads the release gates name.
//!
//! This is a measurement, not an assertion: run it in release mode and compare the bridges.
//!
//! ```text
//! cargo test --release -p apollo-router --lib -- \
//!     plugins::rhai::tests::json_compatibility::performance --ignored --nocapture --test-threads 1
//! ```
//!
//! Workloads, each against a response whose `data` holds `users`:
//! - `no_access`: the callback never touches a JSON-bearing field;
//! - `read_only`: reads one user's name;
//! - `sparse`: edits one user's name through a property chain, which writes `data` back;
//! - `dense`: edits every user and writes `data` back.
//!
//! `callback` rows run one script directly against the Rhai engine and report latency, bytes
//! allocated, and bytes still held when the script finishes (the edited response). `field` rows
//! report the size of the native Rhai value one `data` read materializes, which bounds the extra
//! memory a callback needs while it holds that field. `pipeline` rows report requests per second
//! through the supergraph plugin service.

use std::hint::black_box;
use std::time::Duration;
use std::time::Instant;

use futures::StreamExt;
use rhai::Dynamic;
use rhai::Scope;
use serde_json_bytes::json;
use tower::Service;
use tower::ServiceExt;

use super::super::new_rhai_test_engine;
use crate::Context;
use crate::allocator;
use crate::graphql;
use crate::json_ext::Value;
use crate::plugin::DynPlugin;
use crate::plugins::rhai::engine::json_fields::Bridge;
use crate::plugins::rhai::engine::json_fields::take_conversions;
use crate::services::SupergraphRequest;
use crate::services::SupergraphResponse;

const WORKLOADS: [(&str, &str); 4] = [
    ("no_access", "let n = 1 + 1; n"),
    ("read_only", "response.data.users[0].name"),
    ("sparse", r#"response.data.users[0].name = "edited";"#),
    (
        "dense",
        r#"let data = response.data;
        for i in 0..data.users.len() {
            data.users[i].score += 1.0;
            data.users[i].tags.push("seen");
        }
        response.data = data;"#,
    ),
];

/// Number of users in `data`, and callback iterations for that size.
const SIZES: [(usize, u32); 2] = [(10, 20_000), (1_000, 200)];

const PIPELINE_REQUESTS: [(usize, u32); 2] = [(10, 5_000), (1_000, 100)];

fn data(users: usize) -> Value {
    json!({
        "users": (0..users).map(|id| json!({
            "id": id,
            "name": format!("user {id}"),
            "email": format!("user{id}@example.com"),
            "active": id % 2 == 0,
            "score": id as f64 / 3.0,
            "tags": ["a", "b", "c"],
            "address": { "city": "Somewhere", "zip": format!("{id:05}") },
        })).collect::<Vec<_>>(),
        "total": users,
    })
}

fn response(users: usize) -> graphql::Response {
    graphql::Response::builder().data(data(users)).build()
}

/// Bytes allocated by `f`, and bytes it allocated that are still live when it returns.
fn allocations<R>(f: impl FnOnce() -> R) -> (R, usize, usize) {
    allocator::with_memory_tracking("rhai_json_bridge", || {
        let stats = allocator::current().expect("inside memory tracking");
        let result = f();
        let allocated = stats.bytes_allocated() + stats.bytes_zeroed();
        (result, allocated, stats.net_allocated())
    })
}

fn per_iteration(total: usize, iterations: u32) -> usize {
    total / iterations as usize
}

fn measure_callbacks(bridge: Bridge) {
    let engine = new_rhai_test_engine();
    for (name, script) in WORKLOADS {
        let ast = engine.compile(script).unwrap();
        for (users, iterations) in SIZES {
            let template = response(users);
            let (mut elapsed, mut allocated, mut retained) = (Duration::ZERO, 0, 0);
            for _ in 0..iterations {
                let mut scope = Scope::new();
                scope.push("response", template.clone());
                let start = Instant::now();
                let (result, bytes, live) = allocations(|| {
                    engine
                        .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
                        .unwrap()
                });
                elapsed += start.elapsed();
                allocated += bytes;
                retained += live;
                drop(black_box(result));
            }
            println!(
                "callback,{bridge:?},{name},{users},{:.2},{},{}",
                elapsed.as_secs_f64() * 1e6 / iterations as f64,
                per_iteration(allocated, iterations),
                per_iteration(retained, iterations),
            );
        }
    }
}

fn measure_field(bridge: Bridge) {
    let engine = new_rhai_test_engine();
    let ast = engine.compile("response.data").unwrap();
    for (users, _) in SIZES {
        let mut scope = Scope::new();
        scope.push("response", response(users));
        let (field, _, live) = allocations(|| {
            engine
                .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
                .unwrap()
        });
        drop(black_box(field));
        println!("field,{bridge:?},data,{users},,,{live}");
    }
}

async fn measure_pipeline(bridge: Bridge) {
    let plugin: Box<dyn DynPlugin> = crate::plugin::plugins()
        .find(|factory| factory.name == "apollo.rhai")
        .expect("Plugin not found")
        .create_instance_without_schema(&serde_json::json!({
            "scripts": "tests/fixtures/rhai_json_compatibility",
            "main": "performance.rhai",
        }))
        .await
        .unwrap();
    for (name, _) in WORKLOADS {
        for (users, requests) in PIPELINE_REQUESTS {
            let chunk = response(users);
            let inner = tower::service_fn(move |request: SupergraphRequest| {
                let chunk = chunk.clone();
                async move {
                    SupergraphResponse::fake_stream_builder()
                        .responses(vec![chunk])
                        .context(request.context)
                        .build()
                }
            });
            let mut service = plugin.supergraph_service(inner.boxed());
            take_conversions();
            let start = Instant::now();
            for _ in 0..requests {
                let context = Context::new();
                context.insert("workload", name.to_string()).unwrap();
                let request = SupergraphRequest::fake_builder()
                    .query("{ users { name } }")
                    .context(context)
                    .build()
                    .unwrap();
                let response = service.ready().await.unwrap().call(request).await.unwrap();
                black_box(response.response.into_body().collect::<Vec<_>>().await);
            }
            let throughput = requests as f64 / start.elapsed().as_secs_f64();
            // Guards against a fixture that silently stops exercising its workload.
            let conversions = take_conversions().to_rhai.response_data;
            assert_eq!(
                conversions == 0,
                name == "no_access",
                "{name}: {conversions}"
            );
            println!("pipeline,{bridge:?},{name},{users},{throughput:.0},,");
        }
    }
}

#[tokio::test]
#[ignore = "measurement; run in release mode"]
async fn bridge_performance() {
    println!("kind,bridge,workload,users,latency_us|requests_per_s,allocated_bytes,retained_bytes");
    for bridge in Bridge::ALL {
        let _selected = bridge.select();
        measure_callbacks(bridge);
        measure_field(bridge);
        measure_pipeline(bridge).await;
    }
}
