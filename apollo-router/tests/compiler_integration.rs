/// Just a "smoke test",
/// `compare_schema_parsing` is expected to be used with a wide variety of inputs,
/// such as fuzzing or a corpus production schemas.
#[test]
fn test_compare_schema_parsing() {
    let (with_ast, with_hir) = apollo_router::_private::compare_schema_parsing(include_str!(
        "../src/query_planner/testdata/schema.graphql"
    ));
    similar_asserts::assert_eq!(with_ast, with_hir)
}

/// Just a "smoke test",
/// `compare_query_parsing` is expected to be used with a wide variety of inputs,
/// such as fuzzing or a corpus production queries.
#[test]
fn test_compare_query_parsing() {
    let query = "
    query TopProducts($first: Int) { 
        topProducts(first: $first) { 
            upc 
            name 
            reviews { 
                id 
                product { name } 
                author { id name } 
            } 
        } 
    }
    ";
    let (with_ast, with_hir) = apollo_router::_private::compare_query_parsing(query);
    similar_asserts::assert_eq!(with_ast, with_hir)
}
