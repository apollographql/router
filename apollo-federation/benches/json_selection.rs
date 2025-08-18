use apollo_federation::connectors::{
    ConnectSpec, ConnectSpec::V0_2, ConnectSpec::V0_3, JSONSelection,
};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use serde_json_bytes::Value;
use serde_json_bytes::json;

fn json_selection(selection: &str, version: ConnectSpec) {
    let _ = JSONSelection::parse_with_spec(selection, version)
        .unwrap()
        .apply_to(&data());
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("select_field", |b| {
        b.iter(|| json_selection(black_box("rootField"), black_box(V0_2)))
    });
    c.bench_function("select_field_value", |b| {
        b.iter(|| json_selection(black_box("$.rootField"), black_box(V0_2)))
    });
    c.bench_function("basic_subselection", |b| {
        b.iter(|| json_selection(black_box("user { firstName lastName }"), black_box(V0_2)))
    });
    c.bench_function("array_subselection", |b| {
        b.iter(|| json_selection(black_box("results { name }"), black_box(V0_2)))
    });
    c.bench_function("array_value_subselection", |b| {
        b.iter(|| json_selection(black_box("$.results { name }"), black_box(V0_2)))
    });
    c.bench_function("arrow_method", |b| {
        b.iter(|| json_selection(black_box("results->first { name }"), black_box(V0_2)))
    });
    c.bench_function("arbitrary_spaces", |b| {
        b.iter(|| json_selection(black_box("results ->  first {    name }"), black_box(V0_2)))
    });
    c.bench_function("select_field_optional", |b| {
        b.iter(|| json_selection(black_box("rootField?"), black_box(V0_3)))
    });
    c.bench_function("select_null_optional", |b| {
        b.iter(|| json_selection(black_box("nullField?"), black_box(V0_3)))
    });
    c.bench_function("select_missing_optional", |b| {
        b.iter(|| json_selection(black_box("missingField?"), black_box(V0_3)))
    });
    c.bench_function("arrow_method_optional", |b| {
        b.iter(|| json_selection(black_box("results?->first { name }"), black_box(V0_3)))
    });
    c.bench_function("arrow_method_null_optional", |b| {
        b.iter(|| json_selection(black_box("nullField?->first { name }"), black_box(V0_3)))
    });
    c.bench_function("arrow_method_missing_optional", |b| {
        b.iter(|| json_selection(black_box("missingField?->first { name }"), black_box(V0_3)))
    });
    c.bench_function("optional_subselection", |b| {
        b.iter(|| {
            json_selection(
                black_box("user: user? { firstName lastName }"),
                black_box(V0_3),
            )
        })
    });
    c.bench_function("optional_subselection_short", |b| {
        b.iter(|| json_selection(black_box("user? { firstName lastName }"), black_box(V0_3)))
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);

fn data() -> Value {
    json!({
        "rootField": "hello",
        "nullField": null,
        "user": {
                "firstName": "Alice",
                "lastName": "InChains"
        },
        "results": [
            {
                "name": "Alice",
            },
            {
                "name": "John",
            },
        ]
    })
}
