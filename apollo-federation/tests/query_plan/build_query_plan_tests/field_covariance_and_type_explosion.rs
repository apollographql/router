//! This tests the issue from https://github.com/apollographql/federation/issues/1858.
//! That issue, which was a bug in the handling of selection sets, was concretely triggered with
//! a mix of an interface field implemented with some covariance and the query plan using
//! type-explosion.
//! We include a test using a federation 1 supergraph as this is how the issue was discovered
//! and it is the simplest way to reproduce since type-explosion is always triggered when we
//! have federation 1 supergraph (due to those lacking information on interfaces). The 2nd
//! test shows that error can be reproduced on a pure fed2 example, it's just a bit more
//! complex as we need to involve a @provide just to force the query planner to type explode
//! (more precisely, this force the query planner to _consider_ type explosion; the generated
//! query plan still ends up not type-exploding in practice since as it's not necessary).

use apollo_federation::query_plan::query_planner::QueryPlanner;

#[test]
#[should_panic(expected = "not yet implemented")] // TODO: does this test make sense if we don’t support fed1 supergraphs?
fn with_federation_1_supergraphs() {
    let supergraph = r#"
        schema @core(feature: "https://specs.apollo.dev/core/v0.1") @core(feature: "https://specs.apollo.dev/join/v0.1") {
            query: Query
        }
    
        directive @core(feature: String!) repeatable on SCHEMA
        directive @join__field(graph: join__Graph, requires: join__FieldSet, provides: join__FieldSet) on FIELD_DEFINITION
        directive @join__type(graph: join__Graph!, key: join__FieldSet) repeatable on OBJECT | INTERFACE
        directive @join__owner(graph: join__Graph!) on OBJECT | INTERFACE
        directive @join__graph(name: String!, url: String!) on ENUM_VALUE
    
        interface Interface {
            field: Interface
        }
    
        scalar join__FieldSet
    
        enum join__Graph {
            SUBGRAPH @join__graph(name: "subgraph", url: "http://localhost:4001/")
        }
    
        type Object implements Interface {
            field: Object
        }
    
        type Query {
            dummy: Interface @join__field(graph: SUBGRAPH)
        }
    "#;

    let supergraph = apollo_federation::Supergraph::new(supergraph).unwrap();
    let planner = QueryPlanner::new(&supergraph, Default::default()).unwrap();
    let api_schema = supergraph.to_api_schema(Default::default()).unwrap();

    assert_plan!(
        &(api_schema, planner),
        r#"
        {
          dummy {
            field {
              ... on Object {
                field {
                  __typename
                }
              }
            }
          }
        }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "subgraph") {
              {
                dummy {
                  __typename
                  field {
                    __typename
                    ... on Object {
                      field {
                        __typename
                      }
                    }
                  }
                }
              }
            },
          }
        "###
    );
}

#[test]
#[should_panic(expected = "snapshot assertion")]
// TODO: investigate this failure
fn with_federation_2_subgraphs() {
    let planner = planner!(
        Subgraph1: r#"
        type Query {
          dummy: Interface
        }

        interface Interface {
          field: Interface
        }

        type Object implements Interface @key(fields: "id") {
          id: ID!
          field: Object @provides(fields: "x")
          x: Int @external
        }
        "#,
        Subgraph2: r#"
        type Object @key(fields: "id") {
          id: ID!
          x: Int @shareable
        }
        "#,
    );
    assert_plan!(
        &planner,
        r#"
        {
          dummy {
            field {
              ... on Object {
                field {
                  __typename
                }
              }
            }
          }
        }
        "#,
        @r###"
          QueryPlan {
            Fetch(service: "Subgraph1") {
              {
                dummy {
                  __typename
                  field {
                    __typename
                    ... on Object {
                      field {
                        __typename
                      }
                    }
                  }
                }
              }
            },
          }
        "###
    );
}
