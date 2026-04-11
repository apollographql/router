//! Benchmark: Rhai string interner — direct engine, no router stack.
//!
//! Tests whether disabling Rhai's `RwLock<StringsInterner>` (via
//! `engine.set_max_strings_interned(0)`) improves throughput under concurrent
//! load for scripts representative of production request processing.
//!
//! The script is modelled on the real subgraph request callback work:
//!   - Lookups in a context map using long string keys
//!     (`"apollo::authentication::jwt_status"`, `"customer::client_name"`, ...)
//!   - `in` operator containment checks (string equality under the hood)
//!   - Template-string interpolation (`\`...\``)
//!   - String comparisons (`starts_with`, `==`)
//!   - Cookie / CSV building loops (string concatenation)
//!
//! Two configurations:
//!   - `default_256_interned` — `Engine::new()` default, 256-entry interner,
//!     every string op acquires `RwLock<StringsInterner>`
//!   - `disabled_0_interned` — `set_max_strings_interned(0)`, interner field is
//!     `None`, no lock is ever taken
//!
//! Two variants:
//!   - `sequential` — single thread, measures raw Rhai execution cost
//!   - `concurrent_N` — N OS threads sharing one `Arc<Engine>`, surfaces any
//!     RwLock write contention on the interner
//!
//! Run with:
//! ```
//! cargo bench -p apollo-router-benchmarks --bench rhai_string_interning
//! ```

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use criterion::criterion_group;
use criterion::criterion_main;
use criterion::BenchmarkId;
use criterion::Criterion;
use criterion::Throughput;
use rhai::Engine;
use rhai::Scope;

// How many OS threads share the engine in the concurrent variant.
const CONCURRENCY: usize = 8;

// A Rhai script representative of typical subgraph request callback work.
//
// Patterns exercised:
//   - Long context-key string literals (`"apollo::authentication::jwt_status"`, ...)
//   - Map construction and `[]`-key lookups
//   - `in` containment operator (string equality)
//   - `if` / `else if` chains comparing strings
//   - Template-string interpolation
//   - Cookie-building loop with string concatenation
//   - `starts_with` / `len` string methods
const SCRIPT: &str = r#"
    // ---------- context simulation ----------------------------------------
    // Mirrors a ~40-key context map accessed across typical request callbacks.
    let ctx = #{
        "apollo::authentication::jwt_status": (),
        "apollo::authentication::jwt_claims": (),
        "apollo::supergraph::operation_kind": "query",
        "apollo::supergraph::operation_name": "TopProducts",
        "customer::caller_tag": "test-client-service",
        "customer::caller_cred_kind": "oauth",
        "customer::caller_grants": ["role-a", "role-b"],
        "customer::has_override": false,
        "customer::format": "en_US",
        "customer::rg": "us",
        "customer::vault_gate_status": "present",
        "customer::vault_gate_bearer": "Bearer eyJ...",
        "customer::perms::level_resolved": (),
        "customer::entity_ref_status": "present",
        "customer::entity_ref": "acct-0123456789",
        "customer::entity_ref_resolver": "identity-service",
        "customer::tokens::validation_passed": #{ "TOKN": "abc123value", "SKEY": "xyz789value" },
        "customer::tokens::org_scope": (),
        "customer::tokens::emitted": (),
        "customer::net_origin": "203.0.113.42",
        "customer::req_epoch_ms": 1_700_000_000_000
    };

    // ---------- authentication check equivalent ---------------------------
    let has_auth = "customer::caller_cred_kind" in ctx;

    // ---------- retry header equivalent -----------------------------------
    let op_kind = ctx["apollo::supergraph::operation_kind"];
    let idempotency = if op_kind == "query" { "true" } else { "" };

    // ---------- ext auth forwarding equivalent ----------------------------
    let ext_auth_header = "";
    if "customer::vault_gate_status" in ctx {
        let status = ctx["customer::vault_gate_status"];
        if status == "failure" {
            ext_auth_header = "customer-vault-gate-failure: true";
        } else if status == "invalid" {
            ext_auth_header = "customer-vault-gate-invalid: true";
        } else if status == "present" && "customer::vault_gate_bearer" in ctx {
            ext_auth_header = `customer-vault-gate: ${ctx["customer::vault_gate_bearer"]}`;
        }
    }

    // ---------- entity ref forwarding equivalent --------------------------
    let acct_header = "";
    if "customer::entity_ref_status" in ctx {
        let ref_status = ctx["customer::entity_ref_status"];
        if ref_status == "failure" {
            acct_header = "customer-svc-entity-ref-failure: true";
        } else if ref_status == "invalid" {
            acct_header = "customer-svc-entity-ref-invalid: true";
        } else if ref_status == "present" {
            let ref_id = ctx["customer::entity_ref"];
            let ref_ns = ctx["customer::entity_ref_resolver"];
            acct_header = `customer-svc-entity-ref: ${ref_id} / ${ref_ns}`;
        }
    }

    // ---------- region/format query-param forwarding equivalent ----------
    let query_string = "";
    if "customer::rg" in ctx {
        query_string += `?rg=${ctx["customer::rg"]}`;
    }
    if "customer::format" in ctx {
        if query_string == "" {
            query_string += `?fmt=${ctx["customer::format"]}`;
        } else {
            query_string += `&fmt=${ctx["customer::format"]}`;
        }
    }
    let path = `/data-retrieval-service/graphql${query_string}`;

    // ---------- token forwarding equivalent (build loop) -----------------
    let valid_tokens = ctx["customer::tokens::validation_passed"];
    let cookie_string = "";
    if valid_tokens != () {
        for key in valid_tokens.keys() {
            let value = valid_tokens[key];
            if cookie_string != "" { cookie_string += "; "; }
            cookie_string += `${key}=${value}`;
        }
    }

    // ---------- token subject check equivalent ---------------------------
    // (starts_with + len — mirrors sub-claim validation pattern)
    let sub = "app:abcdefgh1234567890abcdefgh1234567890abcdefgh1234567890abcdefgh";
    let is_app_sub = sub.starts_with("app:") && sub.len() == 68;

    // ---------- result (prevents dead-code elimination) ------------------
    #{
        has_auth: has_auth,
        idempotency: idempotency,
        ext_auth_header: ext_auth_header,
        acct_header: acct_header,
        path: path,
        cookie_string: cookie_string,
        is_app_sub: is_app_sub
    }
"#;

fn make_engine(max_strings_interned: Option<usize>) -> Arc<Engine> {
    let mut engine = Engine::new();
    if let Some(n) = max_strings_interned {
        engine.set_max_strings_interned(n);
    }
    Arc::new(engine)
}

fn rhai_string_interning_benchmark(c: &mut Criterion) {
    let configs: &[(&str, Option<usize>)] = &[
        ("default_256_interned", None),
        ("disabled_0_interned", Some(0)),
    ];

    for &(label, max_strings_interned) in configs {
        let engine = make_engine(max_strings_interned);
        let ast = Arc::new(engine.compile(SCRIPT).expect("script compiles"));

        let mut group = c.benchmark_group("rhai_string_interning");
        group
            .measurement_time(Duration::from_secs(20))
            .sample_size(200)
            .throughput(Throughput::Elements(1));

        // --- Sequential: one scope per iteration, single thread ----------
        {
            let engine = engine.clone();
            let ast = ast.clone();
            group.bench_with_input(BenchmarkId::new("sequential", label), label, |b, _| {
                b.iter(|| {
                    let mut scope = Scope::new();
                    engine
                        .eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast)
                        .expect("eval ok")
                });
            });
        }

        // --- Concurrent: CONCURRENCY threads sharing one engine ----------
        // `iter_custom` lets us drive N threads per criterion iteration and
        // report wall-clock time for the whole batch.  Each thread runs
        // `per_thread` iterations so the reported time covers all of them.
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
                        let start = Instant::now();
                        std::thread::scope(|s| {
                            for _ in 0..CONCURRENCY {
                                let engine = engine.clone();
                                let ast = ast.clone();
                                s.spawn(move || {
                                    for _ in 0..per_thread {
                                        let mut scope = Scope::new();
                                        let _ = engine
                                            .eval_ast_with_scope::<rhai::Dynamic>(&mut scope, &ast)
                                            .expect("eval ok");
                                    }
                                });
                            }
                        });
                        start.elapsed()
                    });
                },
            );
        }

        group.finish();
    }
}

criterion_group!(benches, rhai_string_interning_benchmark);
criterion_main!(benches);
