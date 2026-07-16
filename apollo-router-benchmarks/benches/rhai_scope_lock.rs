//! Benchmark: Rhai `scope` mutex — direct engine, no router stack.
//!
//! The router's Rhai plugin (`apollo-router/src/plugins/rhai/mod.rs::execute`) dispatches
//! every registered callback one of two ways:
//!   - Curried (closure) callbacks: `FnPtr::call`, no shared state touched.
//!   - Non-curried (named function / `Fn("name")`) callbacks: `Engine::call_fn` against a
//!     single `Arc<Mutex<Scope<'static>>>` shared by every clone of the plugin's `RhaiService`
//!     for the lifetime of the router's Rhai instance (until the next hot-reload).
//!
//! This benchmark isolates that difference: it does not touch the string interner lock
//! (disabled via `set_max_strings_interned(0)` in both configurations, matching the
//! recommended `intern_strings: false` setting — see `rhai_string_interning.rs`), so any
//! throughput gap observed here is attributable to the `scope` mutex alone.
//!
//! Two configurations:
//!   - `non_curried` — one shared `Arc<Mutex<Scope>>`, `Engine::call_fn` locks it on every call,
//!     exactly mirroring `execute()`'s non-curried branch.
//!   - `curried` — a curried `FnPtr` (closure capturing a bound value, matching the docs'
//!     own curried example), `FnPtr::call` touches no shared scope at all.
//!
//! Two variants:
//!   - `sequential` — single thread, measures raw per-call cost with no contention.
//!   - `concurrent_N` — N OS threads sharing one `Arc<Engine>` (+ one shared `Arc<Mutex<Scope>>`
//!     for the non-curried case), surfacing scope-mutex serialization under concurrent load —
//!     the scenario that matters for a router handling concurrent requests, or fanning out to
//!     multiple subgraphs within a single request.
//!
//! Run with:
//! ```
//! cargo bench -p apollo-router-benchmarks --bench rhai_scope_lock
//! ```

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use criterion::criterion_group;
use criterion::criterion_main;
use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::Throughput;
use rhai::Dynamic;
use rhai::Engine;
use rhai::FnPtr;
use rhai::Scope;
use rhai::AST;

// How many OS threads share the engine (and, for non-curried, the scope mutex) in the
// concurrent variant. Matches rhai_string_interning.rs.
const CONCURRENCY: usize = 8;

// A script body representative of typical request-callback work (cookie/header parsing,
// map building, string ops) -- deliberately modest, not a worst case, since this is meant to
// model the router's own documented "sweet spot" for Rhai (simple header/context tweaks).
fn body() -> &'static str {
    r#"
        let out = #{};
        let cookies = request.split(";");
        for cookie in cookies {
            let kv = cookie.split("=");
            out[kv[0]] = kv[1];
        }
        out.len()
    "#
}

fn non_curried_script() -> String {
    format!("fn process_request(request) {{ {} }}", body())
}

// Mirrors the docs' own curried example (index.mdx "Curried vs. non-curried callback
// functions"): a local value is captured by the closure, so Rhai actually curries it.
fn curried_maker_script() -> String {
    format!(
        r#"
        fn make_handler() {{
            let bound_marker = "closure-captured";
            let handler = |request| {{
                let marker = bound_marker;
                {}
            }};
            handler
        }}
        "#,
        body()
    )
}

fn make_engine() -> Engine {
    let mut engine = Engine::new();
    // Isolate the scope-mutex effect from the (separately documented / benchmarked) string
    // interner lock -- see rhai_string_interning.rs.
    engine.set_max_strings_interned(0);
    engine
}

fn get_curried_fnptr(engine: &Engine, ast: &AST) -> FnPtr {
    let mut scope = Scope::new();
    let result: Dynamic = engine
        .call_fn(&mut scope, ast, "make_handler", ())
        .expect("make_handler failed");
    let fnptr = result.try_cast::<FnPtr>().expect("not a FnPtr");
    assert!(
        fnptr.is_curried(),
        "expected a curried FnPtr (closure with captured var)"
    );
    fnptr
}

const ARG: &str = "a=1;b=2;c=3";

fn rhai_scope_lock_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("rhai_scope_lock");
    group
        .measurement_time(Duration::from_secs(20))
        .sample_size(200)
        .throughput(Throughput::Elements(1));

    // ---------------- non_curried: shared Arc<Mutex<Scope>>, locked every call ----------------
    {
        let engine = Arc::new(make_engine());
        let ast = Arc::new(engine.compile(non_curried_script()).expect("compiles"));

        // Sequential: one call at a time, still through the same lock/call_fn path.
        {
            let engine = engine.clone();
            let ast = ast.clone();
            let scope = Arc::new(Mutex::new(Scope::new()));
            group.bench_with_input(
                BenchmarkId::new("sequential", "non_curried"),
                "non_curried",
                |b, _| {
                    b.iter(|| {
                        let mut guard = scope.lock().unwrap();
                        let _: Dynamic = engine
                            .call_fn(&mut guard, &ast, "process_request", (ARG.to_string(),))
                            .expect("call_fn ok");
                    });
                },
            );
        }

        // Concurrent: CONCURRENCY threads sharing one Arc<Mutex<Scope>>, exactly mirroring
        // every request-time invocation of a non-curried callback in execute().
        {
            let engine = engine.clone();
            let ast = ast.clone();
            group.bench_with_input(
                BenchmarkId::new(format!("concurrent_{CONCURRENCY}"), "non_curried"),
                "non_curried",
                |b, _| {
                    b.iter_custom(|iters| {
                        let per_thread = (iters as usize).max(1).div_ceil(CONCURRENCY);
                        let engine = engine.clone();
                        let ast = ast.clone();
                        let scope = Arc::new(Mutex::new(Scope::new()));
                        let start = Instant::now();
                        std::thread::scope(|s| {
                            for _ in 0..CONCURRENCY {
                                let engine = engine.clone();
                                let ast = ast.clone();
                                let scope = scope.clone();
                                s.spawn(move || {
                                    for _ in 0..per_thread {
                                        let mut guard = scope.lock().unwrap();
                                        let _: Dynamic = engine
                                            .call_fn(
                                                &mut guard,
                                                &ast,
                                                "process_request",
                                                (ARG.to_string(),),
                                            )
                                            .expect("call_fn ok");
                                    }
                                });
                            }
                        });
                        start.elapsed()
                    });
                },
            );
        }
    }

    // ---------------- curried: no shared scope, FnPtr::call ----------------
    {
        let engine = Arc::new(make_engine());
        let ast = Arc::new(engine.compile(curried_maker_script()).expect("compiles"));
        let fnptr = get_curried_fnptr(&engine, &ast);

        // Sequential
        {
            let engine = engine.clone();
            let ast = ast.clone();
            let fnptr = fnptr.clone();
            group.bench_with_input(
                BenchmarkId::new("sequential", "curried"),
                "curried",
                |b, _| {
                    b.iter(|| {
                        let _: Dynamic = fnptr
                            .call(&engine, &ast, (ARG.to_string(),))
                            .expect("curried call ok");
                    });
                },
            );
        }

        // Concurrent: CONCURRENCY threads, no shared mutable state at all.
        {
            let engine = engine.clone();
            let ast = ast.clone();
            let fnptr = fnptr.clone();
            group.bench_with_input(
                BenchmarkId::new(format!("concurrent_{CONCURRENCY}"), "curried"),
                "curried",
                |b, _| {
                    b.iter_custom(|iters| {
                        let per_thread = (iters as usize).max(1).div_ceil(CONCURRENCY);
                        let engine = engine.clone();
                        let ast = ast.clone();
                        let fnptr = fnptr.clone();
                        let start = Instant::now();
                        std::thread::scope(|s| {
                            for _ in 0..CONCURRENCY {
                                let engine = engine.clone();
                                let ast = ast.clone();
                                let fnptr = fnptr.clone();
                                s.spawn(move || {
                                    for _ in 0..per_thread {
                                        let _: Dynamic = fnptr
                                            .call(&engine, &ast, (ARG.to_string(),))
                                            .expect("curried call ok");
                                    }
                                });
                            }
                        });
                        start.elapsed()
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, rhai_scope_lock_benchmark);
criterion_main!(benches);
