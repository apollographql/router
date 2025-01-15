use apollo_compiler::ExecutableDocument;

use super::query_plan_analysis::interpret_query_plan;
use super::response_shape::ResponseShape;
use super::*;
use crate::query_plan::query_planner;

// The schema used in these tests.
const SCHEMA_STR: &str = r#"
schema @link(url: "https://specs.apollo.dev/link/v1.0") @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION) {
  query: Query
}

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

interface I @join__type(graph: A, key: "id") @join__type(graph: B, key: "id") @join__type(graph: S) {
  id: ID!
  data_a(arg: Int!): String! @join__field(graph: A)
  data_b(arg: Int!): String! @join__field(graph: B)
  data(arg: Int!): Int! @join__field(graph: S)
}

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "A", url: "query-plan-response-shape/test.graphql?subgraph=A")
  B @join__graph(name: "B", url: "query-plan-response-shape/test.graphql?subgraph=B")
  S @join__graph(name: "S", url: "query-plan-response-shape/test.graphql?subgraph=S")
}

scalar link__Import

enum link__Purpose {
  """
  `SECURITY` features provide metadata necessary to securely resolve fields.
  """
  SECURITY
  """
  `EXECUTION` features provide metadata necessary for operation execution.
  """
  EXECUTION
}

type Query @join__type(graph: A) @join__type(graph: B) @join__type(graph: S) {
  test_i: I! @join__field(graph: S)
}

type T implements I @join__implements(graph: A, interface: "I") @join__implements(graph: B, interface: "I") @join__implements(graph: S, interface: "I") @join__type(graph: A, key: "id") @join__type(graph: B, key: "id") @join__type(graph: S, key: "id") {
  id: ID!
  data_a(arg: Int!): String! @join__field(graph: A)
  data_b(arg: Int!): String! @join__field(graph: B)
  data(arg: Int!): Int! @join__field(graph: S)
}
"#;

fn plan_response_shape(op_str: &str) -> ResponseShape {
    // Parse the schema and operation
    let supergraph = crate::Supergraph::new(SCHEMA_STR).unwrap();
    let api_schema = supergraph.to_api_schema(Default::default()).unwrap();
    let op =
        ExecutableDocument::parse_and_validate(api_schema.schema(), op_str, "op.graphql").unwrap();

    // Plan the query
    let config = query_planner::QueryPlannerConfig {
        generate_query_fragments: false,
        type_conditioned_fetching: false,
        ..Default::default()
    };
    let planner = query_planner::QueryPlanner::new(&supergraph, config).unwrap();
    let query_plan = planner
        .build_query_plan(&op, None, Default::default())
        .unwrap();

    // Compare response shapes
    let correctness_schema = planner.supergraph_schema();
    let op_rs =
        response_shape::compute_response_shape_for_operation(&op, correctness_schema).unwrap();
    let root_type = response_shape::compute_the_root_type_condition_for_operation(&op).unwrap();
    let plan_rs = interpret_query_plan(correctness_schema, &root_type, &query_plan).unwrap();
    let subgraphs_by_name = supergraph
        .extract_subgraphs()
        .unwrap()
        .into_iter()
        .map(|(name, subgraph)| (name, subgraph.schema))
        .collect();
    let root_constraint = subgraph_constraint::SubgraphConstraint::at_root(&subgraphs_by_name);
    assert!(
        response_shape_compare::compare_response_shapes(&root_constraint, &op_rs, &plan_rs).is_ok()
    );

    plan_rs
}

//=================================================================================================
// Basic tests

#[test]
fn test_single_fetch() {
    let op_str = r#"
        query {
            test_i {
                data(arg: 0)
                alias1: data(arg: 1)
                alias2: data(arg: 1)
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      test_i -----> test_i {
        __typename -----> __typename
        data -----> data(arg: 0)
        alias1 -----> data(arg: 1)
        alias2 -----> data(arg: 1)
      }
    }
    "###);
}

#[test]
fn test_empty_plan() {
    let op_str = r#"
        query($v0: Boolean!) {
            test_i @include(if: $v0) @skip(if:true) {
                data(arg: 0)
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
    }
    "###);
}

#[test]
fn test_condition_node() {
    let op_str = r#"
        query($v1: Boolean!) {
            test_i @include(if: $v1) {
                data(arg: 0)
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v1 {
        __typename -----> __typename
        data -----> data(arg: 0)
      }
    }
    "###);
}

#[test]
fn test_sequence_node() {
    let op_str = r#"
        query($v1: Boolean!) {
            test_i @include(if: $v1) {
                data(arg: 0)
                data_a(arg: 0)
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v1 {
        __typename -----> __typename
        data -----> data(arg: 0)
        id -----> id
        data_a -----> data_a(arg: 0)
      }
    }
    "###);
}

#[test]
fn test_parallel_node() {
    let op_str = r#"
        query($v1: Boolean!) {
            test_i @include(if: $v1) {
                data(arg: 0)
                data_a(arg: 0)
                data_b(arg: 0)
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v1 {
        __typename -----> __typename
        data -----> data(arg: 0)
        id -----> id
        data_b -----> data_b(arg: 0)
        data_a -----> data_a(arg: 0)
      }
    }
    "###);
}
