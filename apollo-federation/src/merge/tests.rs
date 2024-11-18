use apollo_compiler::Schema;
use insta::assert_snapshot;

use crate::merge::merge_federation_subgraphs;
use crate::schema::ValidFederationSchema;
use crate::ValidFederationSubgraph;
use crate::ValidFederationSubgraphs;

#[test]
fn test_steel_thread() {
    let one_sdl = include_str!("../sources/connect/expand/merge/connector_Query_users_0.graphql");
    let two_sdl = include_str!("../sources/connect/expand/merge/connector_Query_user_0.graphql");
    let three_sdl = include_str!("../sources/connect/expand/merge/connector_User_d_1.graphql");
    let graphql_sdl = include_str!("../sources/connect/expand/merge/graphql.graphql");

    let mut subgraphs = ValidFederationSubgraphs::new();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "connector_Query_users_0".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(one_sdl, "./connector_Query_users_0.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "connector_Query_user_0".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(two_sdl, "./connector_Query_user_0.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "connector_User_d_1".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(three_sdl, "./connector_User_d_1.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "graphql".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(graphql_sdl, "./graphql.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_basic() {
    let one_sdl = include_str!("../sources/connect/expand/merge/basic_1.graphql");
    let two_sdl = include_str!("../sources/connect/expand/merge/basic_2.graphql");

    let mut subgraphs = ValidFederationSubgraphs::new();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "basic_1".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(one_sdl, "./basic_1.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "basic_2".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(two_sdl, "./basic_2.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_inaccessible() {
    let one_sdl = include_str!("../sources/connect/expand/merge/inaccessible.graphql");
    let two_sdl = include_str!("../sources/connect/expand/merge/inaccessible_2.graphql");

    let mut subgraphs = ValidFederationSubgraphs::new();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "inaccessible".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(one_sdl, "./inaccessible.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "inaccessible_2".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(two_sdl, "./inaccessible_2.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_interface_object() {
    let one_sdl = include_str!("../sources/connect/expand/merge/interface_object_1.graphql");
    let two_sdl = include_str!("../sources/connect/expand/merge/interface_object_2.graphql");
    let three_sdl = include_str!("../sources/connect/expand/merge/interface_object_3.graphql");

    let mut subgraphs = ValidFederationSubgraphs::new();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "interface_object_1".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(one_sdl, "./interface_object_1.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "interface_object_2".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(two_sdl, "./interface_object_2.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();
    subgraphs
        .add(ValidFederationSubgraph {
            name: "interface_object_3".to_string(),
            url: "".to_string(),
            schema: ValidFederationSchema::new(
                Schema::parse_and_validate(three_sdl, "./interface_object_3.graphql").unwrap(),
            )
            .unwrap(),
        })
        .unwrap();

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}
