pub(super) fn observe_query_recursion(recursion_reached: usize) {
    u64_histogram_with_unit!(
        "apollo.router.operations.recursion",
        "Recursion depth per operation",
        "{count}",
        recursion_reached as u64
    );
}

pub(super) fn observe_query_lexical_token(lexical_tokens_reached: usize) {
    u64_histogram_with_unit!(
        "apollo.router.operations.lexical_tokens",
        "Lexical tokens processed per operation",
        "{count}",
        lexical_tokens_reached as u64
    );
}

#[cfg(test)]
mod tests {
    use crate::spec::Query;
    use crate::spec::Schema;

    #[test]
    fn test_query_recursion_and_tokens() {
        let schema = include_str!("fixtures/metrics_test_schema.graphql");
        let query = include_str!("fixtures/metrics_test_query.graphql");
        let operation = "MetricsTestQuery";

        let schema = Schema::parse(schema, &Default::default()).unwrap();
        Query::parse_document(query, Some(operation), &schema, &Default::default()).unwrap();

        assert_histogram_sum!("apollo.router.operations.recursion", 2);
        assert_histogram_sum!("apollo.router.operations.lexical_tokens", 19);
    }
}
