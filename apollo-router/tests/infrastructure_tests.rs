#[test]
fn test_starstuff_supergraph_is_valid() {
    include_str!("../../examples/graphql/supergraph.graphql")
        .parse::<apollo_router::Schema>()
        .expect(
            r#"Couldn't parse the supergraph example.
This file is being used in the router documentation, as a quickstart example.
Make sure it is accessible, and the configuration is working with the router."#,
        );

    insta::assert_snapshot!(include_str!("../../examples/graphql/supergraph.graphql"));
}
