#[cfg(test)]
pub mod tests {
    include!("shared.rs");

    #[test]
    fn test() {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        let builder = setup();
        let (router, _) = runtime.block_on(builder.build()).unwrap();
        runtime.block_on(basic_composition_benchmark(router.clone()));
    }
}
