use criterion::{criterion_group, criterion_main, Criterion};

include!("../src/shared.rs");

fn from_elem(c: &mut Criterion) {
    c.bench_function("basic_composition_benchmark", move |b| {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let builder = setup();

        let (router, _) = runtime.block_on(builder.build()).unwrap();
        b.to_async(runtime)
            .iter(|| basic_composition_benchmark(router.clone()));
    });
}

criterion_group!(benches, from_elem);
criterion_main!(benches);
