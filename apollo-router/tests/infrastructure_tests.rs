#[tokio::test]
#[tracing_test::traced_test]
async fn test_starstuff_supergraph_is_valid() {
    let schema = include_str!("../../examples/graphql/supergraph.graphql");
    apollo_router::TestHarness::builder()
        .schema(schema)
        .build_router()
        .await
        .expect(
            r#"Couldn't parse the supergraph example.
This file is being used in the router documentation, as a quickstart example.
Make sure it is accessible, and the configuration is working with the router."#,
        );

    insta::assert_snapshot!(include_str!("../../examples/graphql/supergraph.graphql"));
}
