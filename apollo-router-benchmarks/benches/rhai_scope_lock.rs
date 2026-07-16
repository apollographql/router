//! Benchmark: Rhai `scope` mutex — direct engine, no router stack.
//!
//! The router's Rhai plugin (`apollo-router/src/plugins/rhai/mod.rs::execute`) dispatches
//! every registered callback one of two ways:
//!   - Curried (closure) callbacks: `FnPtr::call`, no shared state touched.
//!   - Non-curried (named function / `Fn("name")`) callbacks: `Engine::call_fn` against a
//!     single `Arc<Mutex<Scope<'static>>>` shared by every clone of the plugin's `RhaiService`
//!     for the lifetime of the router's Rhai instance (until the next hot-reload). `execute()`
//!     clones the scope under the lock and calls against the private clone, so the lock is
//!     only ever held for the clone, not the call.
//!
//! This benchmark isolates that difference: it does not touch the string interner lock
//! (disabled via `set_max_strings_interned(0)` in every configuration, matching the
//! recommended `intern_strings: false` setting — see `rhai_string_interning.rs`), so any
//! throughput gap observed here is attributable to the `scope` mutex alone.
//!
//! Three configurations:
//!   - `curried` — a curried `FnPtr` (closure capturing a bound value, matching the docs'
//!     own curried example), `FnPtr::call` touches no shared scope at all.
//!   - `non_curried_cloned` — one shared `Arc<Mutex<Scope>>`, locked only to `Scope::clone()`
//!     it, then `Engine::call_fn` against the owned clone. Mirrors `execute()`'s current
//!     non-curried branch.
//!   - `non_curried_locked` — the pre-fix shape: the same shared `Arc<Mutex<Scope>>` held
//!     for the full `call_fn`. Kept as a baseline to show the size of the improvement.
//!
//! Two variants:
//!   - `sequential` — single thread, measures raw per-call cost with no contention.
//!   - `concurrent_N` — N OS threads sharing one `Arc<Engine>` (+ one shared `Arc<Mutex<Scope>>`
//!     for the non-curried cases), surfacing scope-mutex serialization under concurrent load —
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

const ARG: &str = "a=1;b=2;c=3";

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

/// One non-curried call, current (post-fix) shape: lock only to clone the scope.
fn call_non_curried_cloned(engine: &Engine, ast: &AST, scope: &Mutex<Scope<'static>>) {
    let mut scope = scope.lock().unwrap().clone();
    let _: Dynamic = engine
        .call_fn(&mut scope, ast, "process_request", (ARG.to_string(),))
        .expect("call_fn ok");
}

/// One non-curried call, pre-fix shape: lock held for the whole call_fn. Baseline only.
fn call_non_curried_locked(engine: &Engine, ast: &AST, scope: &Mutex<Scope<'static>>) {
    let mut guard = scope.lock().unwrap();
    let _: Dynamic = engine
        .call_fn(&mut guard, ast, "process_request", (ARG.to_string(),))
        .expect("call_fn ok");
}

fn bench_variant(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    label: &'static str,
    call: fn(&Engine, &AST, &Mutex<Scope<'static>>),
) {
    let engine = Arc::new(make_engine());
    let ast = Arc::new(engine.compile(non_curried_script()).expect("compiles"));

    // Sequential: one call at a time, still through the same lock/clone/call_fn path.
    {
        let engine = engine.clone();
        let ast = ast.clone();
        let scope = Arc::new(Mutex::new(Scope::new()));
        group.bench_with_input(BenchmarkId::new("sequential", label), label, |b, _| {
            b.iter(|| call(&engine, &ast, &scope));
        });
    }

    // Concurrent: CONCURRENCY threads sharing one Arc<Mutex<Scope>>, exactly mirroring
    // every request-time invocation of a non-curried callback in execute().
    {
        let engine = engine.clone();
        let ast = ast.clone();
        group.bench_with_input(
            BenchmarkId::new(format!("concurrent_{CONCURRENCY}"), label),
            label,
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
                                    call(&engine, &ast, &scope);
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

fn rhai_scope_lock_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("rhai_scope_lock");
    group
        .measurement_time(Duration::from_secs(20))
        .sample_size(200)
        .throughput(Throughput::Elements(1));

    bench_variant(&mut group, "non_curried_locked", call_non_curried_locked);
    bench_variant(&mut group, "non_curried_cloned", call_non_curried_cloned);

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
