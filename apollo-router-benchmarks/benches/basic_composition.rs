use criterion::criterion_group;
use criterion::criterion_main;
use criterion::Criterion;

include!("../src/shared.rs");

fn from_elem(c: &mut Criterion) {
    c.bench_function("basic_composition_benchmark", move |b| {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let builder = setup();

        let router = runtime.block_on(builder.build()).unwrap();
        b.to_async(runtime)
            .iter(|| basic_composition_benchmark(router.test_service()));
    });
}

criterion_group!(benches, from_elem);
criterion_main!(benches);
