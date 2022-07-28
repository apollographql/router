#[test]
fn test_starstuff_supergraph_is_valid() {
    let schema = include_str!("../../examples/graphql/supergraph.graphql");
    apollo_router::Schema::parse(schema, &Default::default()).expect(
        r#"Couldn't parse the supergraph example.
This file is being used in the router documentation, as a quickstart example.
Make sure it is accessible, and the configuration is working with the router."#,
    );

    insta::assert_snapshot!(include_str!("../../examples/graphql/supergraph.graphql"));
}
