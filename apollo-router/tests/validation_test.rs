use apollo_router::_private::create_test_service_factory_from_yaml;

#[tokio::test]
async fn test_supergraph_validation_errors_are_passed_on() {
    create_test_service_factory_from_yaml(
        include_str!("../src/testdata/invalid_supergraph.graphql"),
        r#"
    experimental_graphql_validation_mode: both
"#,
    )
    .await;
}
