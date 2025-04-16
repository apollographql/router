use super::query_plan_analysis_test::plan_response_shape_with_schema;
use super::response_shape::ResponseShape;

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

scalar join__FieldSet

enum join__Graph {
  A @join__graph(name: "A", url: "test-template.graphql?subgraph=A")
  S @join__graph(name: "S", url: "test-template.graphql?subgraph=S")
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

type P
  @join__type(graph: A)
  @join__type(graph: S, key: "id")
{
  id: ID!
  p_data(arg: Int!): Int! @join__field(graph: A, external: true) @join__field(graph: S)
}

type Query
  @join__type(graph: A)
  @join__type(graph: S)
{
  start_t: T! @join__field(graph: S)
}

type T
  @join__type(graph: A, key: "id")
  @join__type(graph: S, key: "id")
{
  id: ID!
  data: Int! @join__field(graph: A, requires: "pre(arg: 0)")
  data2: Int! @join__field(graph: A, requires: "pre2(arg: 2)")
  data3: Int! @join__field(graph: A, requires: "p { p_data(arg: 1) }")
  pre(arg: Int!): Int! @join__field(graph: A, external: true) @join__field(graph: S)
  pre2(arg: Int!): Int! @join__field(graph: A, external: true) @join__field(graph: S)
  p: P! @join__field(graph: A)
}
"#;

fn plan_response_shape(op_str: &str) -> ResponseShape {
    plan_response_shape_with_schema(SCHEMA_STR, op_str)
}

//=================================================================================================
// Basic tests

#[test]
fn test_requires_basic() {
    let op_str = r#"
        query {
            start_t {
                data
                data2
                data3
            }
        }
    "#;
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      start_t -----> start_t {
        __typename -----> __typename
        id -----> id
        pre2 -----> pre2(arg: 2)
        pre -----> pre(arg: 0)
        p -----> p {
          __typename -----> __typename
          id -----> id
          p_data -----> p_data(arg: 1)
        }
        data -----> data
        data2 -----> data2
        data3 -----> data3
      }
    }
    "###);
}

#[test]
fn test_requires_conditional() {
    let op_str = r#"
        query($v0: Boolean!) {
            start_t {
                data
                data2 @include(if: $v0)
            }
        }
    "#;
    // Note: `pre2` is conditional just like `data2` is conditional.
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      start_t -----> start_t {
        __typename -----> __typename
        id -----> id
        pre2 -may-> pre2(arg: 2) if v0
        pre -----> pre(arg: 0)
        data -----> data
        data2 -may-> data2 if v0
      }
    }
    "###);
}

#[test]
fn test_requires_conditional_multiple_variants() {
    let op_str = r#"
        query($v0: Boolean!) {
            start_t {
                data
                data @include(if: $v0) # creates multi variant requires
            }
        }
    "#;
    // Note: `pre` has two conditional variants just like the `data`.
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      start_t -----> start_t {
        __typename -----> __typename
        id -----> id
        pre -may-> pre(arg: 0) if v0
        pre -may-> pre(arg: 0)
        data -may-> data
        data -may-> data if v0
      }
    }
    "###);
}

#[test]
fn test_requires_external_under_non_external() {
    let op_str = r#"
        query {
            start_t {
                data3
            }
        }
    "#;
    // Note: `p` is a nested selection set from a `@requires` directive.
    insta::assert_snapshot!(plan_response_shape(op_str), @r###"
    {
      start_t -----> start_t {
        __typename -----> __typename
        id -----> id
        p -----> p {
          __typename -----> __typename
          id -----> id
          p_data -----> p_data(arg: 1)
        }
        data3 -----> data3
      }
    }
    "###);
}
