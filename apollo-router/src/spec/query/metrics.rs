use crate::spec::QueryHash;

pub(super) fn observe_query_recursion(
    recursion_reached: usize,
    query_hash: &QueryHash,
    operation_name: Option<&str>,
) {
    let hash = format!("{query_hash}");
    let operation = operation_name
        .map(|name| name.to_string())
        .unwrap_or("null".to_string());
    u64_counter_with_unit!(
        "apollo.router.operations.recursion",
        "Number of recursion operations performed",
        "count",
        recursion_reached as u64,
        "query.hash" = hash,
        "operation.name" = operation
    );
}

pub(super) fn observe_query_lexical_token(
    lexical_tokens_reached: usize,
    query_hash: &QueryHash,
    operation_name: Option<&str>,
) {
    let hash = format!("{query_hash}");
    let operation = operation_name
        .map(|name| name.to_string())
        .unwrap_or("null".to_string());
    u64_counter_with_unit!(
        "apollo.router.operations.lexical_tokens",
        "Number of lexical tokens processed",
        "count",
        lexical_tokens_reached as u64,
        "query.hash" = hash,
        "operation.name" = operation
    );
}

#[cfg(test)]
mod tests {
    use crate::spec::{Query, Schema};

    #[test]
    fn test_query_recursion() {
        let schema = include_str!("fixtures/metrics_test_schema.graphql");
        let query = include_str!("fixtures/metrics_test_query.graphql");
        let operation = "MetricsTestQuery";

        let schema = Schema::parse(schema, &Default::default()).unwrap();
        let query = Query::parse_document(query, Some(operation), &schema, &Default::default()).unwrap();
        let hash = format!("{}", query.hash);
        assert_counter!(
            "apollo.router.operations.recursion",
            2,
            "query.hash" = hash.clone(),
            "operation.name" = operation
        );
        assert_counter!(
            "apollo.router.operations.lexical_tokens",
            18,
            "query.hash" = hash,
            "operation.name" = operation
        );
    }
}
