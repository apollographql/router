use apollo_router::benchmarking::with_supergraph_boilerplate;
use apollo_router::benchmarking::FormatResponseBench;
use criterion::criterion_group;
use criterion::criterion_main;
use criterion::Criterion;
use serde_json_bytes::json;

/// Pre-parsed schema + query pair that can run `format_response` repeatedly.
const SCHEMA: &str = "
type Query {
    a: T  b: T  c: T  d: T  e: T
    f: T  g: T  h: T  i: T  j: T
}
type T {
    w: String  x: String  y: String  z: String
}
";

/// Schema for the nested benchmarks, which exercise `apply_selection_set`
/// rather than `apply_root_selection_set`.
const NESTED_SCHEMA: &str = "
type Query {
    node: N
    nodes: [N]
}
type N {
    w: String  x: String  y: String  z: String
}
";

const LIST_LEN: usize = 1000;

fn nested_object() -> serde_json_bytes::Value {
    json!({"w": "val_w", "x": "val_x", "y": "val_y", "z": "val_z"})
}

fn nested_list_data() -> serde_json_bytes::Value {
    json!({ "nodes": vec![nested_object(); LIST_LEN] })
}

fn response_data() -> serde_json_bytes::Value {
    let obj = json!({"w": "val_w", "x": "val_x", "y": "val_y", "z": "val_z"});
    json!({
        "a": obj, "b": obj, "c": obj, "d": obj, "e": obj,
        "f": obj, "g": obj, "h": obj, "i": obj, "j": obj,
    })
}

fn bench_no_fragments(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "query {
            a { w x y z }  b { w x y z }  c { w x y z }
            d { w x y z }  e { w x y z }  f { w x y z }
            g { w x y z }  h { w x y z }  i { w x y z }
            j { w x y z }
        }",
        response_data(),
    );
    c.bench_function("format_response/no_fragments", |b| {
        b.iter(|| fixture.run());
    });
}

fn bench_single_fragment_reused(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "query {
            ...AllFields
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
            ... on Query { ...AllFields }
        }
        fragment AllFields on Query {
            a { w x y z }  b { w x y z }  c { w x y z }
            d { w x y z }  e { w x y z }  f { w x y z }
            g { w x y z }  h { w x y z }  i { w x y z }
            j { w x y z }
        }",
        response_data(),
    );
    c.bench_function("format_response/single_fragment_reused_10x", |b| {
        b.iter(|| fixture.run());
    });
}

fn bench_many_fragments_reused(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "query {
            ...FA ...FB ...FC ...FD ...FE
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
            ... on Query { ...FA ...FB ...FC ...FD ...FE }
        }
        fragment FA on Query { a { w x y z }  b { w x y z } }
        fragment FB on Query { c { w x y z }  d { w x y z } }
        fragment FC on Query { e { w x y z }  f { w x y z } }
        fragment FD on Query { g { w x y z }  h { w x y z } }
        fragment FE on Query { i { w x y z }  j { w x y z } }",
        response_data(),
    );
    c.bench_function("format_response/many_fragments_reused_10x", |b| {
        b.iter(|| fixture.run());
    });
}

fn bench_unique_fragments_no_reuse(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "query {
            ...F1 ...F2 ...F3 ...F4 ...F5
            ...F6 ...F7 ...F8 ...F9 ...F10
        }
        fragment F1 on Query { a { w x y z } }
        fragment F2 on Query { b { w x y z } }
        fragment F3 on Query { c { w x y z } }
        fragment F4 on Query { d { w x y z } }
        fragment F5 on Query { e { w x y z } }
        fragment F6 on Query { f { w x y z } }
        fragment F7 on Query { g { w x y z } }
        fragment F8 on Query { h { w x y z } }
        fragment F9 on Query { i { w x y z } }
        fragment F10 on Query { j { w x y z } }",
        response_data(),
    );
    c.bench_function("format_response/unique_fragments_no_reuse", |b| {
        b.iter(|| fixture.run());
    });
}

fn bench_fragments_attack_scenario(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "
        fragment L0 on Query { a { w x y z }  b { w x y z }  c { w x y z } }
        fragment L1 on Query { ...L0 ...L0 }
        fragment L2 on Query { ...L1 ...L1 }
        fragment L3 on Query { ...L2 ...L2 }
        fragment L4 on Query { ...L3 ...L3 }
        fragment L5 on Query { ...L4 ...L4 }
        fragment L6 on Query { ...L5 ...L5 }
        fragment L7 on Query { ...L6 ...L6 }
        fragment L8 on Query { ...L7 ...L7 }
        fragment L9 on Query { ...L8 ...L8 }
        fragment L10 on Query { ...L9 ...L9 }
        fragment L11 on Query { ...L10 ...L10 }
        fragment L12 on Query { ...L11 ...L11 }
        fragment L13 on Query { ...L12 ...L12 }
        fragment L14 on Query { ...L13 ...L13 }
        fragment L15 on Query { ...L14 ...L14 }
        fragment L16 on Query { ...L15 ...L15 }
        fragment L17 on Query { ...L16 ...L16 }
        fragment L18 on Query { ...L17 ...L17 }
        fragment L19 on Query { ...L18 ...L18 }
        fragment L20 on Query { ...L19 ...L19 }
        fragment L21 on Query { ...L20 ...L20 }
        query Attack { ...L21 }
        ",
        response_data(),
    );
    c.bench_function("format_response/fragments_attack_scenario", |b| {
        b.iter(|| fixture.run());
    });
}

// The benchmarks above all spread fragments at the *root*, where a single
// dedup set covers the whole traversal. The ones below spread them on nested
// objects, where each object gets its own frame — the case where a per-frame
// dedup set could cost more than it saves.

/// Baseline for `bench_nested_list_single_fragment`: identical output, no
/// fragments, so no dedup set is ever touched.
fn bench_nested_list_no_fragments(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(NESTED_SCHEMA);
    let fixture = FormatResponseBench::new(&sdl, "query { nodes { w x y z } }", nested_list_data());
    c.bench_function("format_response/nested_list_no_fragments", |b| {
        b.iter(|| fixture.run());
    });
}

/// The typical workload: one fragment, spread once, per list element. There is
/// nothing to deduplicate here, so this measures the pure overhead the cache
/// adds — compare against `nested_list_no_fragments`.
fn bench_nested_list_single_fragment(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(NESTED_SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "query { nodes { ...Fields } }
        fragment Fields on N { w x y z }",
        nested_list_data(),
    );
    c.bench_function("format_response/nested_list_single_fragment", |b| {
        b.iter(|| fixture.run());
    });
}

/// Single fragment on a single nested object — the same overhead question
/// without the list amortising anything.
fn bench_nested_single_fragment(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(NESTED_SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "query { node { ...Fields } }
        fragment Fields on N { w x y z }",
        json!({ "node": nested_object() }),
    );
    c.bench_function("format_response/nested_single_fragment", |b| {
        b.iter(|| fixture.run());
    });
}

/// The payoff case for this branch: an exponential fragment chain spread on a
/// nested object, which `apply_root_selection_set`'s cache never covered.
fn bench_nested_fragments_attack_scenario(c: &mut Criterion) {
    let sdl = with_supergraph_boilerplate(NESTED_SCHEMA);
    let fixture = FormatResponseBench::new(
        &sdl,
        "
        fragment L0 on N { w x y z }
        fragment L1 on N { ...L0 ...L0 }
        fragment L2 on N { ...L1 ...L1 }
        fragment L3 on N { ...L2 ...L2 }
        fragment L4 on N { ...L3 ...L3 }
        fragment L5 on N { ...L4 ...L4 }
        fragment L6 on N { ...L5 ...L5 }
        fragment L7 on N { ...L6 ...L6 }
        fragment L8 on N { ...L7 ...L7 }
        fragment L9 on N { ...L8 ...L8 }
        fragment L10 on N { ...L9 ...L9 }
        fragment L11 on N { ...L10 ...L10 }
        fragment L12 on N { ...L11 ...L11 }
        fragment L13 on N { ...L12 ...L12 }
        fragment L14 on N { ...L13 ...L13 }
        fragment L15 on N { ...L14 ...L14 }
        fragment L16 on N { ...L15 ...L15 }
        fragment L17 on N { ...L16 ...L16 }
        fragment L18 on N { ...L17 ...L17 }
        fragment L19 on N { ...L18 ...L18 }
        fragment L20 on N { ...L19 ...L19 }
        fragment L21 on N { ...L20 ...L20 }
        query Attack { node { ...L21 } }
        ",
        json!({ "node": nested_object() }),
    );
    c.bench_function("format_response/nested_fragments_attack_scenario", |b| {
        b.iter(|| fixture.run());
    });
}

criterion_group!(
    benches,
    bench_no_fragments,
    bench_single_fragment_reused,
    bench_many_fragments_reused,
    bench_unique_fragments_no_reuse,
    bench_fragments_attack_scenario,
    bench_nested_list_no_fragments,
    bench_nested_list_single_fragment,
    bench_nested_single_fragment,
    bench_nested_fragments_attack_scenario,
);
criterion_main!(benches);
