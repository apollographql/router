use apollo_compiler::ExecutableDocument;

use super::query_plan_analysis::AnalysisContext;
use super::query_plan_analysis::interpret_query_plan;
use super::response_shape::ResponseShape;
use super::*;
use crate::query_plan::query_planner;

// The schema used in these tests.
const SCHEMA_STR: &str = r#"
schema
  @link(url: "https://specs.apollo.dev/link/v1.0")
  @link(url: "https://specs.apollo.dev/join/v0.3", for: EXECUTION)
{
  query: Query
}

directive @join__enumValue(graph: join__Graph!) repeatable on ENUM_VALUE

directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet, type: String, external: Boolean, override: String, usedOverridden: Boolean) repeatable on FIELD_DEFINITION | INPUT_FIELD_DEFINITION

directive @join__graph(name: String!, url: String!) on ENUM_VALUE

directive @join__implements(graph: join__Graph!, interface: String!) repeatable on OBJECT | INTERFACE

directive @join__type(graph: join__Graph!, key: join__FieldSet, extension: Boolean! = false, resolvable: Boolean! = true, isInterfaceObject: Boolean! = false) repeatable on OBJECT | INTERFACE | UNION | ENUM | INPUT_OBJECT | SCALAR

directive @join__unionMember(graph: join__Graph!, member: String!) repeatable on UNION

directive @link(url: String, as: String, for: link__Purpose, import: [link__Import]) repeatable on SCHEMA

interface I
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: S)
{
  id: ID!
  data_a(arg: Int!): String! @join__field(graph: A)
  data_b(arg: Int!): String! @join__field(graph: B)
  data(arg: Int!): Int! @join__field(graph: S)
}

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "A", url: "local-tests/correctness-issues/boolean-condition-overfetch.graphql?subgraph=A")
  B @join__graph(name: "B", url: "local-tests/correctness-issues/boolean-condition-overfetch.graphql?subgraph=B")
  S @join__graph(name: "S", url: "local-tests/correctness-issues/boolean-condition-overfetch.graphql?subgraph=S")
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

type P implements I
  @join__implements(graph: A, interface: "I")
  @join__implements(graph: B, interface: "I")
  @join__implements(graph: S, interface: "I")
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: S, key: "id")
{
  id: ID!
  data_a(arg: Int!): String! @join__field(graph: A)
  a_p: Int! @join__field(graph: A) @join__field(graph: S, external: true)
  data_b(arg: Int!): String! @join__field(graph: B)
  data(arg: Int!): Int! @join__field(graph: S)
  s_p: Int! @join__field(graph: S, requires: "a_p")
}

type Query
  @join__type(graph: A)
  @join__type(graph: B)
  @join__type(graph: S)
{
  test_i: I! @join__field(graph: S)
}

type T implements I
  @join__implements(graph: A, interface: "I")
  @join__implements(graph: B, interface: "I")
  @join__implements(graph: S, interface: "I")
  @join__type(graph: A, key: "id")
  @join__type(graph: B, key: "id")
  @join__type(graph: S, key: "id")
{
  id: ID!
  data_a(arg: Int!): String! @join__field(graph: A)
  nested: I! @join__field(graph: A)
  data_b(arg: Int!): String! @join__field(graph: B)
  data(arg: Int!): Int! @join__field(graph: S)
}
"#;

pub(crate) fn plan_response_shape_with_schema(schema_str: &str, op_str: &str) -> ResponseShape {
    // Initialization
    let config = query_planner::QueryPlannerConfig {
        generate_query_fragments: false,
        type_conditioned_fetching: false,
        incremental_delivery: query_planner::QueryPlanIncrementalDeliveryConfig {
            enable_defer: true,
        },
        ..Default::default()
    };
    let supergraph = crate::Supergraph::new(schema_str).unwrap();
    let planner = query_planner::QueryPlanner::new(&supergraph, config).unwrap();

    // Parse the schema and operation
    let api_schema = planner.api_schema();
    let op =
        ExecutableDocument::parse_and_validate(api_schema.schema(), op_str, "op.graphql").unwrap();

    // Plan the query
    let query_plan = planner
        .build_query_plan(&op, None, Default::default())
        .unwrap();

    // Compare response shapes
    let op_rs = response_shape::compute_response_shape_for_operation(&op, api_schema).unwrap();
    let root_type = response_shape::compute_the_root_type_condition_for_operation(&op).unwrap();
    let supergraph_schema = planner.supergraph_schema();
    let subgraphs_by_name = supergraph
        .extract_subgraphs()
        .unwrap()
        .into_iter()
        .map(|(name, subgraph)| (name, subgraph.schema))
        .collect();
    let context = AnalysisContext::new(supergraph_schema.clone(), &subgraphs_by_name);
    let plan_rs = interpret_query_plan(&context, &root_type, &query_plan).unwrap();
    let path_constraint = subgraph_constraint::SubgraphConstraint::at_root(&subgraphs_by_name);
    let assumption = response_shape::Clause::default(); // empty assumption at the top level
    assert!(
        compare_response_shapes_with_constraint(&path_constraint, &assumption, &op_rs, &plan_rs)
            .is_ok()
    );

    plan_rs
}

fn plan_response_shape(op_str: &str) -> ResponseShape {
    plan_response_shape_with_schema(SCHEMA_STR, op_str)
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

#[test]
fn test_defer_node() {
    let op_str = r#"
        query($v1: Boolean!) {
            test_i @include(if: $v1) {
                data(arg: 0)
                ... @defer {
                  data_a(arg: 0)
                }
                ... @defer {
                  data_b(arg: 0)
                }
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

#[test]
fn test_defer_node_nested() {
    let op_str = r#"
        query($v1: Boolean!) {
            test_i @include(if: $v1) {
                data(arg: 0)
                ... on T @defer {
                    nested {
                        ... @defer {
                            data_b(arg: 1)
                        }
                    }
                }
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      test_i -may-> test_i if v1 {
        __typename -may-> __typename on I
        __typename -may-> __typename on T
        data -----> data(arg: 0)
        id -may-> id on T
        nested -may-> nested on T {
          __typename -----> __typename
          id -----> id
          data_b -----> data_b(arg: 1)
        }
      }
    }
    "###);
}

// QP missing ConditionNode bug (FED-505).
// - Note: The correctness checker won't report this, since it's an over-fetching issue.
#[test]
fn test_missing_boolean_condition_over_fetch() {
    let op_str = r#"
      query($v0: Boolean!) {
        test_i {
          ... on P @include(if: $v0) {
              s_p
          }
          ... on P @skip(if: $v0) {
              a_p
          }
        }
      }
    "#;
    // Note: `s_p -may-> s_p on P` is supposed to have `if v0` condition.
    let rs = plan_response_shape(op_str);
    insta::assert_snapshot!(rs, @r###"
    {
      test_i -----> test_i {
        __typename -may-> __typename on I
        __typename -may-> __typename on P if v0
        __typename -may-> __typename on P if ¬v0
        id -may-> id on P if v0
        id -may-> id on P if ¬v0
        a_p -may-> a_p on P if v0
        a_p -may-> a_p on P if ¬v0
        s_p -may-> s_p on P
      }
    }
    "###);
}

// Related to FED-505, but QP is still correct in this case.
#[test]
fn test_missing_boolean_condition_still_correct() {
    let op_str = r#"
      query($v0: Boolean!) {
        test_i {
          ... on P @include(if: $v0) {
              s_p
          }
          ... on P @skip(if: $v0) {
              s_p
          }
        }
      }
    "#;
    // Note: `s_p -may-> s_p on P` below is missing Boolean conditions, but still correct.
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      test_i -----> test_i {
        __typename -may-> __typename on I
        __typename -may-> __typename on P if v0
        __typename -may-> __typename on P if ¬v0
        id -may-> id on P if v0
        id -may-> id on P if ¬v0
        a_p -may-> a_p on P if v0
        a_p -may-> a_p on P if ¬v0
        s_p -may-> s_p on P
      }
    }
    "###);
}
