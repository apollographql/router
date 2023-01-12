/// Just a "smoke test",
/// `fuzz_schema_parsing` is expected to be used with a wide variety of inputs,
/// such as fuzzing or a corpus production schemas.
#[test]
#[should_panic] // parse_with_hir is not yet implemented
fn compare_schema_parsing() {
    let (with_ast, with_hir) = apollo_router::_private::compare_schema_parsing(include_str!(
        "../src/query_planner/testdata/schema.graphql"
    ));
    similar_asserts::assert_eq!(with_ast, with_hir)
}
