// Ported from federation/composition-js/src/__tests__/compose.test.ts
// Original describe block: 'Union types'

use super::ServiceDefinition;
use super::assert_api_schema_snapshot;
use super::assert_composition_success;
use super::compose_as_fed2_subgraphs;

#[ignore = "until merge implementation completed"]
#[test]
fn merges_inconsistent_unions() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
            u: U!
        }

        union U = A | B

        type A {
            a: Int
        }

        type B {
            b: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        union U = C

        type C {
            b: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = assert_composition_success(&result);

    assert_api_schema_snapshot(supergraph);
}
