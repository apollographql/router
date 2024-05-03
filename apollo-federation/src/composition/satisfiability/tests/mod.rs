use std::sync::Arc;

use apollo_compiler::{execution::GraphQLError, Schema};

use crate::{
    api_schema::to_api_schema,
    query_graph::{build_federated_query_graph, build_query_graph::build_query_graph},
    schema::ValidFederationSchema,
    ApiSchemaOptions,
};

use super::{diagnostics::CompositionHint, validate_graph_composition};

fn run_validation(
    supergraph_sdl: &str,
) -> Result<Vec<CompositionHint>, (Vec<GraphQLError>, Vec<CompositionHint>)> {
    let schema = Schema::parse_and_validate(supergraph_sdl, "supergraph.graphql").unwrap();

    let supergraph_schema = ValidFederationSchema::new(schema).unwrap();
    let api_schema = to_api_schema(supergraph_schema.clone(), ApiSchemaOptions::default()).unwrap();

    let api_query_graph = build_query_graph("api_schema".into(), api_schema.clone()).unwrap();

    let federated_query_graph =
        build_federated_query_graph(supergraph_schema.clone(), api_schema.clone(), None, None)
            .unwrap();

    validate_graph_composition(
        Arc::new(supergraph_schema),
        Arc::new(api_query_graph),
        Arc::new(federated_query_graph),
    )
}

static LIST_DETAIL_RESOLVABLE_FALSE: &str =
    include_str!("./schemas/list_detail_resolvable_false.graphql");

#[test]
fn list_detail_resolvable_false() {
    let result = run_validation(LIST_DETAIL_RESOLVABLE_FALSE);

    assert!(result.is_err());
    let messages = result
        .unwrap_err()
        .0
        .iter()
        .map(|e| e.message.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        messages,
        vec![
            r#"The following supergraph API query:
{
  a {
    y
  }
}
cannot be satisfied by the subgraphs because:
- from subgraph "B":
  - cannot find field "A.y".
  - cannot move to subgraph "A", which has field "A.y", because none of the @key defined on type "A" in subgraph "A" are resolvable (they are all declared with their "resolvable" argument set to false)."#,
        ]
    );
}

static REQUIRES_FAILS_IF_IT_CANNOT_SATISFY_A_AT_REQUIRES: &str =
    include_str!("./schemas/requires_fails_if_it_cannot_satisfy_a_at_requires.graphql");

#[test]
fn requires_fails_if_it_cannot_satisfy_a_at_requires() {
    let _ = run_validation(REQUIRES_FAILS_IF_IT_CANNOT_SATISFY_A_AT_REQUIRES);

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      The following supergraph API query:
      {
        a {
          y
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "A": cannot find field "A.y".
      - from subgraph "B": cannot satisfy @require conditions on field "A.y" (please ensure that this is not due to key field "id" being accidentally marked @external).
      `,
      `
      The following supergraph API query:
      {
        a {
          z
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "A": cannot find field "A.z".
      - from subgraph "B": cannot satisfy @require conditions on field "A.z" (please ensure that this is not due to key field "id" being accidentally marked @external).
      `
    ]);
    */
}

static REQUIRES_FAILS_IF_IT_NO_USABLE_POST_AT_REQUIRES_KEYS: &str =
    include_str!("./schemas/requires_fails_if_it_no_usable_post_at_requires_keys.graphql");

#[test]
fn requires_fails_if_it_no_usable_post_at_requires_keys() {
    let _ = run_validation(REQUIRES_FAILS_IF_IT_NO_USABLE_POST_AT_REQUIRES_KEYS);

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      The following supergraph API query:
      {
        getT1s {
          f2 {
            ...
          }
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "B": @require condition on field "T1.f2" can be satisfied but missing usable key on "T1" in subgraph "B" to resume query.
      - from subgraph "A": cannot find field "T1.f2".
      `
    ]);
    */
}

static NON_RESOLVABLE_KEYS_FAILS_IF_KEY_IS_DECLARED_NON_RESOLVABLE_BUT_WOULD_BE_NEEDED: &str =
    include_str!("./schemas/non_resolvable_keys_fails_if_key_is_declared_non_resolvable_but_would_be_needed.graphql");

#[test]
fn non_resolvable_keys_fails_if_key_is_declared_non_resolvable_but_would_be_needed() {
    let _ = run_validation(
        NON_RESOLVABLE_KEYS_FAILS_IF_KEY_IS_DECLARED_NON_RESOLVABLE_BUT_WOULD_BE_NEEDED,
    );

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      The following supergraph API query:
      {
        getTs {
          f
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "B":
        - cannot find field "T.f".
        - cannot move to subgraph "A", which has field "T.f", because none of the @key defined on type "T" in subgraph "A" are resolvable (they are all declared with their "resolvable" argument set to false).
      `
    ]);
    */
}

static INTERFACEOBJECT_FAILS_ON_INTERFACEOBJECT_USAGE_WITH_MISSING_KEY_ON_INTERFACE: &str =
    include_str!("./schemas/interfaceObject_fails_on_interfaceObject_usage_with_missing_key_on_interface.graphql");

#[test]
fn interface_object_fails_on_interface_object_usage_with_missing_key_on_interface() {
    let _ = run_validation(
        INTERFACEOBJECT_FAILS_ON_INTERFACEOBJECT_USAGE_WITH_MISSING_KEY_ON_INTERFACE,
    );

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      The following supergraph API query:
      {
        iFromB {
          ... on A {
            ...
          }
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "subgraphB": no subgraph can be reached to resolve the implementation type of @interfaceObject type "I".
      `,
      `
      The following supergraph API query:
      {
        iFromB {
          ... on B {
            ...
          }
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "subgraphB": no subgraph can be reached to resolve the implementation type of @interfaceObject type "I".
      `,
    ]);
    */
}

static INTERFACEOBJECT_FAILS_ON_INTERFACEOBJECT_WITH_SOME_UNREACHABLE_IMPLEMENTATION: &str =
    include_str!("./schemas/interfaceObject_fails_on_interfaceObject_with_some_unreachable_implementation.graphql");

#[test]
fn interface_object_fails_on_interface_object_with_some_unreachable_implementation() {
    let _ = run_validation(
        INTERFACEOBJECT_FAILS_ON_INTERFACEOBJECT_WITH_SOME_UNREACHABLE_IMPLEMENTATION,
    );

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      The following supergraph API query:
      {
        iFromB {
          ... on A {
            z
          }
        }
      }
      cannot be satisfied by the subgraphs because:
      - from subgraph "subgraphB":
        - cannot find implementation type "A" (supergraph interface "I" is declared with @interfaceObject in "subgraphB").
        - cannot move to subgraph "subgraphC", which has field "A.z", because interface "I" is not defined in this subgraph (to jump to "subgraphC", it would need to both define interface "I" and have a @key on it).
      - from subgraph "subgraphA":
        - cannot find field "A.z".
        - cannot move to subgraph "subgraphC", which has field "A.z", because type "A" has no @key defined in subgraph "subgraphC".
      `
    ]);
    */
}

static WHEN_SHARED_FIELD_HAS_NON_INTERSECTING_RUNTIME_TYPES_IN_DIFFERENT_SUBGRAPHS_ERRORS_FOR_INTERFACES: &str =
    include_str!("./schemas/when_shared_field_has_non_intersecting_runtime_types_in_different_subgraphs_errors_for_interfaces.graphql");

#[test]
fn when_shared_field_has_non_intersecting_runtime_types_in_different_subgraphs_errors_for_interfaces(
) {
    let _ = run_validation(WHEN_SHARED_FIELD_HAS_NON_INTERSECTING_RUNTIME_TYPES_IN_DIFFERENT_SUBGRAPHS_ERRORS_FOR_INTERFACES);

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      For the following supergraph API query:
      {
        a {
          ...
        }
      }
      Shared field "Query.a" return type "A" has a non-intersecting set of possible runtime types across subgraphs. Runtime types in subgraphs are:
       - in subgraph "A", type "I1";
       - in subgraph "B", type "I2".
      This is not allowed as shared fields must resolve the same way in all subgraphs, and that imply at least some common runtime types between the subgraphs.
      `
    ]);
    */
}

static WHEN_SHARED_FIELD_HAS_NON_INTERSECTING_RUNTIME_TYPES_IN_DIFFERENT_SUBGRAPHS_ERRORS_FOR_UNIONS: &str =
    include_str!("./schemas/when_shared_field_has_non_intersecting_runtime_types_in_different_subgraphs_errors_for_unions.graphql");

#[test]
fn when_shared_field_has_non_intersecting_runtime_types_in_different_subgraphs_errors_for_unions() {
    let _ = run_validation(WHEN_SHARED_FIELD_HAS_NON_INTERSECTING_RUNTIME_TYPES_IN_DIFFERENT_SUBGRAPHS_ERRORS_FOR_UNIONS);

    /*
    expect(errorMessages(result)).toMatchStringArray([
      `
      For the following supergraph API query:
      {
        e {
          s {
            ...
          }
        }
      }
      Shared field "E.s" return type "U!" has a non-intersecting set of possible runtime types across subgraphs. Runtime types in subgraphs are:
       - in subgraph "A", types "A" and "B";
       - in subgraph "B", types "C" and "D".
      This is not allowed as shared fields must resolve the same way in all subgraphs, and that imply at least some common runtime types between the subgraphs.
      `
    ]);
    */
}
