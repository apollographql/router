const GENERATED_QUERIES_COUNT: usize = 100;

fn main() {
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());

    // used by `benches/memory_use.rs`
    let queries_path = out_dir.join("queries.rs");

    println!("cargo:rerun-if-changed=benches/fixtures/supergraph.graphql");
    let schema = include_str!("benches/fixtures/supergraph.graphql");
    let schema: apollo_smith::Document = apollo_parser::Parser::new(schema)
        .parse()
        .document()
        .try_into()
        .unwrap();
    let mut state: u32 = 0;
    let mut prng = || {
        state = state.wrapping_mul(134775813).wrapping_add(1);
        (state >> 24) as u8
    };
    let mut bytes = vec![0; 128];
    let mut queries = Vec::<String>::new();
    queries.resize_with(GENERATED_QUERIES_COUNT, || {
        bytes.fill_with(&mut prng);
        let mut input = arbitrary::Unstructured::new(&bytes);
        apollo_smith::DocumentBuilder::with_document(&mut input, schema.clone())
            .unwrap()
            .operation_definition()
            .unwrap()
            .unwrap()
            .into()
    });
    let rust = format!("static QUERIES: &[&str] = &{queries:#?};\n");
    std::fs::write(queries_path, rust).unwrap()
}
