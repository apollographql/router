// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'satisfiablility validation'

use super::ServiceDefinition;
use super::assert_composition_success;
use super::assert_error_contains;
use super::compose_as_fed2_subgraphs;

#[ignore = "until merge implementation completed"]
#[test]
fn uses_the_proper_error_code() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            a: A
        }

        type A @shareable {
            x: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A @shareable {
            x: Int
            y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert!(result.is_err());

    assert_error_contains(
        &result,
        r#"
    The following supergraph API query:
    {
      a {
        y
      }
    }
    cannot be satisfied by the subgraphs because:
    - from subgraph "subgraphA":
      - cannot find field "A.y".
      - cannot move to subgraph "subgraphB", which has field "A.y", because type "A" has no @key defined in subgraph "subgraphB".
    "#,
    )
}

#[ignore = "until merge implementation completed"]
#[test]
fn handles_indirectly_reachable_keys() {
    // This tests ensure that a regression introduced by https://github.com/apollographql/federation/pull/1653
    // is properly fixed. All we want to check is that validation succeed on this example, which it should.

    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            t: T
        }

        type T @key(fields: "k1") {
            k1: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        # Note: the ordering of the key happens to matter for this to be a proper reproduction of the
        # issue #1653 created.
        type T @key(fields: "k2") @key(fields: "k1") {
            k1: Int
            k2: Int
        }
        "#,
    };

    let subgraph_c = ServiceDefinition {
        name: "subgraphC",
        type_defs: r#"
        type T @key(fields: "k2") {
            k2: Int
            v: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b, subgraph_c]);
    let _ = assert_composition_success(&result);
}
