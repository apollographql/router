use apollo_compiler::coord;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::Subgraph;
use test_log::test;

use super::ServiceDefinition;
use super::assert_composition_errors;
use super::compose_as_fed2_subgraphs;

// =============================================================================
// @inaccessible DIRECTIVE PROPAGATION - Tests for @inaccessible propagation
// =============================================================================

#[test]
fn inaccessible_propagates_to_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          me: User @inaccessible
          users: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: String!
          age: Int! @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");

    assert!(
        coord!(Query.me)
            .lookup_field(supergraph.schema().schema())
            .expect("Query.me should exist")
            .directives
            .iter()
            .any(|d| d.name == "inaccessible"),
        "Expected @inaccessible directive on Query.me field"
    );
    assert!(
        coord!(User.age)
            .lookup_field(supergraph.schema().schema())
            .expect("User.age should exist")
            .directives
            .iter()
            .any(|d| d.name == "inaccessible"),
        "Expected @inaccessible directive on User.age field"
    );
}

#[test]
fn inaccessible_merges_on_same_element() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String @shareable @inaccessible
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          name: String @shareable @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let supergraph = result.expect("Expected composition to succeed");

    assert!(
        coord!(User.name)
            .lookup_field(supergraph.schema().schema())
            .expect("User.name should exist")
            .directives
            .iter()
            .any(|d| d.name == "inaccessible"),
        "Expected @inaccessible directive on User.name field"
    );
}

#[test]
fn inaccessible_rejects_inaccessible_and_external_together() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          user: [User]
        }

        type User @key(fields: "id") {
          id: ID!
          name: String!
          birthdate: Int! @external @inaccessible
          age: Int! @requires(fields: "birthdate")
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type User @key(fields: "id") {
          id: ID!
          birthdate: Int!
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "MERGED_DIRECTIVE_APPLICATION_ON_EXTERNAL",
            r#"[subgraphA] Cannot apply merged directive @inaccessible to external field "User.birthdate""#,
        )],
    );
}

#[test]
fn inaccessible_errors_if_imported_under_mismatched_names() {
    let subgraph_a = Subgraph::parse("subgraphA", "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

        type Query {
          q: Int
          q1: Int @private
        }
        "#,
    ).unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraphB",
        "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: ["@inaccessible"])

        type Query {
          q2: Int @inaccessible
        }
        "#,
    )
    .unwrap();

    let result = compose(vec![subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "LINK_IMPORT_NAME_MISMATCH",
            r#"The "@inaccessible" directive (from https://specs.apollo.dev/federation/v2.0) is imported with mismatched name between subgraphs: it is imported as "@inaccessible" in subgraph "subgraphB" but "@private" in subgraph "subgraphA""#,
        )],
    );
}

#[test]
fn inaccessible_succeeds_if_imported_under_same_non_default_name() {
    let subgraph_a = Subgraph::parse("subgraphA", "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

        type Query {
          q: Int
          q1: Int @private
        }
        "#,
    ).unwrap();

    let subgraph_b = Subgraph::parse("subgraphB", "",
        r#"
        extend schema
          @link(url: "https://specs.apollo.dev/federation/v2.0", import: [{name: "@inaccessible", as: "@private"}])

        type Query {
          q2: Int @private
        }
        "#,
    ).unwrap();

    let result = compose(vec![subgraph_a, subgraph_b]);
    let supergraph =
        result.expect("Expected composition to succeed with consistent @inaccessible import names");

    let schema = supergraph.schema().schema();
    if let Some(query_type_name) = &schema.schema_definition.query
        && let Some(ExtendedType::Object(query_obj)) = schema.types.get(query_type_name.as_str())
    {
        let q1 = query_obj
            .fields
            .get("q1")
            .expect("Query.q1 should exist in supergraph");
        assert!(q1.directives.iter().any(|d| d.name == "private"));

        let q2 = query_obj
            .fields
            .get("q2")
            .expect("Query.q2 should exist in supergraph");
        assert!(q2.directives.iter().any(|d| d.name == "private"));
    }
}

#[test]
fn inaccessible_ignores_inaccessible_element_when_validating_composition() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          origin: Point
        }

        type Point @shareable {
          x: Int
          y: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type Point @shareable {
          x: Int
          y: Int
          z: Int @inaccessible
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    let _supergraph = result.expect(
        "Expected composition to succeed - @inaccessible fields should be ignored during validation",
    );
}

#[test]
fn inaccessible_errors_if_subgraph_misuses_inaccessible() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
        type Query {
          q1: Int
          q2: A
        }

        type A @shareable {
          x: Int
          y: Int
        }
        "#,
    };

    let subgraph_b = ServiceDefinition {
        name: "subgraphB",
        type_defs: r#"
        type A @shareable @inaccessible {
          x: Int
          y: Int
        }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a, subgraph_b]);
    assert_composition_errors(
        &result,
        &[(
            "REFERENCED_INACCESSIBLE",
            r#"Type `A` is @inaccessible but is referenced by `Query.q2`, which is in the API schema."#,
        )],
    );
}

#[test]
fn inaccessible_uses_security_core_purpose_in_supergraph() {
    let subgraph_a = ServiceDefinition {
        name: "subgraphA",
        type_defs: r#"
          type Query {
            someField: String!
            privateField: String! @inaccessible
          }
        "#,
    };

    let result = compose_as_fed2_subgraphs(&[subgraph_a]);
    let supergraph = result.expect("Expected composition to succeed");
    // Assert there is a @link to @inaccessible with `for: SECURITY` on the supergraph schema
    let schema = supergraph.schema().schema();
    let inaccessible_link = schema
        .schema_definition
        .directives
        .iter()
        .find(|d| {
            d.name == "link"
                && d.specified_argument_by_name("url").is_some_and(|url| {
                    url.to_string()
                        .contains("https://specs.apollo.dev/inaccessible")
                })
        })
        .expect("Link to inaccessible spec should be present in supergraph schema");
    let inaccessible_purpose = inaccessible_link
        .specified_argument_by_name("for")
        .expect("Link to inaccessible spec should have a \"for\" argument indicating purpose");

    assert!(
        matches!(inaccessible_purpose.as_ref(), apollo_compiler::ast::Value::Enum(enum_name) if enum_name == "SECURITY"),
        "Expected a @link with for: SECURITY in the supergraph schema",
    );
}
