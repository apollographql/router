use apollo_compiler::Schema;
use insta::assert_snapshot;

use crate::ValidFederationSubgraph;
use crate::ValidFederationSubgraphs;
use crate::merge::merge_federation_subgraphs;
use crate::schema::ValidFederationSchema;

macro_rules! subgraphs {
    ($($name:expr => $file:expr),* $(,)?) => {{
        let mut subgraphs = ValidFederationSubgraphs::new();

        $(
            subgraphs.add(ValidFederationSubgraph {
                name: $name.to_string(),
                url: "".to_string(),
                schema: ValidFederationSchema::new(
                    Schema::parse_and_validate(include_str!($file), $file).unwrap(),
                )
                .unwrap(),
            }).unwrap();
        )*

        subgraphs
    }};
}

#[test]
fn test_steel_thread() {
    let subgraphs = subgraphs! {
      "connector_Query_users_0" => "../sources/connect/expand/merge/connector_Query_users_0.graphql",
      "connector_Query_user_0" => "../sources/connect/expand/merge/connector_Query_user_0.graphql",
      "connector_User_d_1" => "../sources/connect/expand/merge/connector_User_d_1.graphql",
      "graphql" => "../sources/connect/expand/merge/graphql.graphql",
    };

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_basic() {
    let subgraphs = subgraphs! {
      "basic_1" => "../sources/connect/expand/merge/basic_1.graphql",
      "basic_2" => "../sources/connect/expand/merge/basic_2.graphql",
    };

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_inaccessible() {
    let subgraphs = subgraphs! {
      "inaccessible" => "../sources/connect/expand/merge/inaccessible.graphql",
      "inaccessible_2" => "../sources/connect/expand/merge/inaccessible_2.graphql",
    };

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_interface_object() {
    let subgraphs = subgraphs! {
      "interface_object_1" => "./testdata/interface_object/one.graphql",
      "interface_object_2" => "./testdata/interface_object/two.graphql",
      "interface_object_3" => "./testdata/interface_object/three.graphql",
    };

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_input_types() {
    let subgraphs = subgraphs! {
      "one" => "./testdata/input_types/one.graphql",
    };

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}

#[test]
fn test_interface_implementing_interface() {
    let subgraphs = subgraphs! {
      "one" => "./testdata/interface_implementing_interface/one.graphql",
    };

    let result = merge_federation_subgraphs(subgraphs).unwrap();

    let schema = result.schema.into_inner();
    let validation = schema.clone().validate();
    assert!(validation.is_ok(), "{:?}", validation);

    assert_snapshot!(schema.serialize());
}
