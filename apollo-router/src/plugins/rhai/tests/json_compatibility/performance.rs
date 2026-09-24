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
//! `callback` rows run one script directly against the Rhai engine and report, per script, its
//! latency and the [`HeapUsage`] of running it. `field` rows report the live heap one `data`
//! read materializes as a native Rhai value. `pipeline` rows report requests per second through
//! the supergraph plugin service.

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

/// Heap activity on the current thread while a closure ran, in jemalloc's usable bytes.
///
/// Read from jemalloc's per-thread counters, which see every allocation, reallocation and free:
/// a reallocation counts as allocating the new block and freeing the old one, and freeing
/// storage allocated before the closure started counts as freed.
#[derive(Clone, Copy, Debug, Default)]
struct HeapUsage {
    allocated: u64,
    freed: u64,
    /// Highest live heap above the starting point. jemalloc samples it after every 64 KiB of
    /// allocation or deallocation and when it is read, so a shorter excursion can be missed.
    peak: u64,
}

impl HeapUsage {
    /// Change in live heap: negative when the closure freed more than it allocated.
    fn net(&self) -> i64 {
        self.allocated as i64 - self.freed as i64
    }
}

fn heap_usage<R>(f: impl FnOnce() -> R) -> (R, HeapUsage) {
    use tikv_jemalloc_ctl::thread;

    let allocated = thread::allocatedp::read().unwrap();
    let freed = thread::deallocatedp::read().unwrap();
    // SAFETY: `thread.peak.reset` takes and returns nothing.
    let reset = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"thread.peak.reset".as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(reset, 0, "thread.peak.reset failed");
    let (allocated_before, freed_before) = (allocated.get(), freed.get());
    let result = f();
    let usage = HeapUsage {
        allocated: allocated.get() - allocated_before,
        freed: freed.get() - freed_before,
        // SAFETY: `thread.peak.read` returns a `u64`.
        peak: unsafe { tikv_jemalloc_ctl::raw::read(b"thread.peak.read\0") }.unwrap(),
    };
    (result, usage)
}

fn measure_callbacks(bridge: Bridge) {
    let engine = new_rhai_test_engine();
    for (name, script) in WORKLOADS {
        let ast = engine.compile(script).unwrap();
        for (users, iterations) in SIZES {
            let template = response(users);
            let (mut elapsed, mut total, mut peak) = (Duration::ZERO, HeapUsage::default(), 0);
            for _ in 0..iterations {
                let mut scope = Scope::new();
                scope.push("response", template.clone());
                let start = Instant::now();
                let (result, usage) = heap_usage(|| {
                    engine
                        .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
                        .unwrap()
                });
                elapsed += start.elapsed();
                total.allocated += usage.allocated;
                total.freed += usage.freed;
                peak = peak.max(usage.peak);
                drop(black_box(result));
            }
            let per_script = |bytes: u64| bytes / u64::from(iterations);
            println!(
                "callback,{bridge:?},{name},{users},{:.2},{},{},{},{peak}",
                elapsed.as_secs_f64() * 1e6 / f64::from(iterations),
                per_script(total.allocated),
                per_script(total.freed),
                total.net() / i64::from(iterations),
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
        let (field, usage) = heap_usage(|| {
            engine
                .eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
                .unwrap()
        });
        drop(black_box(field));
        println!(
            "field,{bridge:?},data,{users},,{},{},{},{}",
            usage.allocated,
            usage.freed,
            usage.net(),
            usage.peak
        );
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
            println!("pipeline,{bridge:?},{name},{users},{throughput:.0},,,,");
        }
    }
}

#[tokio::test]
#[ignore = "measurement; run in release mode"]
async fn bridge_performance() {
    println!(
        "kind,bridge,workload,users,latency_us|requests_per_s,allocated_bytes,freed_bytes,net_bytes,peak_bytes"
    );
    for bridge in Bridge::ALL {
        let _selected = bridge.select();
        measure_callbacks(bridge);
        measure_field(bridge);
        measure_pipeline(bridge).await;
    }
}

#[test]
fn heap_usage_counts_reallocation() {
    let (buffer, usage) = heap_usage(|| {
        let mut buffer = Vec::<u8>::with_capacity(8);
        buffer.reserve_exact(64);
        buffer
    });
    assert_eq!(buffer.capacity(), 64);
    // 8 and 64 bytes are jemalloc size classes, so growing moves the buffer to a new block.
    assert_eq!((usage.allocated, usage.freed, usage.net()), (72, 8, 64));
    assert_eq!(usage.peak, 64);
}

#[test]
fn heap_usage_counts_freeing_earlier_storage() {
    let earlier = vec![0_u8; 4096];
    let ((), usage) = heap_usage(|| drop(earlier));
    assert_eq!(
        (usage.allocated, usage.freed, usage.net()),
        (0, 4096, -4096)
    );
    assert_eq!(usage.peak, 0);
}

#[test]
fn heap_usage_peak_includes_freed_storage() {
    let ((), usage) = heap_usage(|| drop(black_box(vec![0_u8; 1 << 20])));
    assert_eq!(usage.net(), 0);
    assert!(usage.peak >= 1 << 20, "{usage:?}");
}
