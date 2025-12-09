use apollo_federation::composition::upgrade_subgraphs_if_necessary;
use apollo_federation::subgraph::typestate::Subgraph;
use insta::assert_snapshot;
use test_log::test;

// =============================================================================
// Fed1 Schema Upgrade Tests
// =============================================================================

/// Fed1 schema with custom directive - description should be preserved after upgrade.
#[test]
fn fed1_preserves_custom_directive_descriptions() {
    let subgraph = Subgraph::parse(
        "subgraph",
        "",
        r#"
            """ This is a description of the custom directive """
            directive @customDirective(value: String!) on FIELD_DEFINITION | OBJECT

            """ This is a description of the custom directive 2 """
            directive @key(fields: _FieldSet!) on OBJECT

            scalar _FieldSet

            type Query {
                hello: String
            }

            type A @key(fields: "id") {
                id: ID!
                name: String @customDirective(value: "test")
            }
        "#,
    )
    .expect("parses schema")
    .expand_links()
    .expect("expands schema");

    assert_snapshot!(subgraph.schema_string());

    let [upgraded]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph])
        .expect("upgrades schema")
        .try_into()
        .expect("Expected 1 element");

    assert_snapshot!(upgraded.schema_string());
}

/// Fed1 schema with federation directive - description should be replaced with standard Fed2 definition.
#[test]
fn fed1_replaces_federation_directive_descriptions() {
    let subgraph = Subgraph::parse(
        "subgraph",
        "",
        r#"
            """ This is my custom description for the key directive """
            directive @key(fields: _FieldSet!) on OBJECT

            scalar _FieldSet

            type Query {
                hello: String
            }

            type A @key(fields: "id") {
                id: ID!
            }
        "#,
    )
    .expect("parses schema")
    .expand_links()
    .expect("expands schema");

    let [upgraded]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph])
        .expect("upgrades schema")
        .try_into()
        .expect("Expected 1 element");

    assert_snapshot!(upgraded.schema_string());
}

// =============================================================================
// Fed2 Schema Passthrough Tests
// =============================================================================

/// Fed2 schema with custom directive - description should be unchanged.
#[test]
fn fed2_preserves_custom_directive_descriptions() {
    let subgraph = Subgraph::parse(
        "subgraph",
        "",
        r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key"])

            """ A custom validation directive for Fed2 """
            directive @validate(pattern: String!) on FIELD_DEFINITION | ARGUMENT_DEFINITION

            type Query {
                search(term: String! @validate(pattern: "^[a-zA-Z]+$")): [Result]
            }

            type Result @key(fields: "id") {
                id: ID!
            }
        "#,
    )
    .expect("parses schema")
    .expand_links()
    .expect("expands schema");

    let [validated]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph])
        .expect("validates schema")
        .try_into()
        .expect("Expected 1 element");

    assert_snapshot!(validated.schema_string());
}

/// Fed2 schema with federation directive - already uses standard definitions.
#[test]
fn fed2_uses_standard_federation_directive_definitions() {
    let subgraph = Subgraph::parse(
        "subgraph",
        "",
        r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@shareable"])

            """ This is my custom description for the key directive """
            directive @key(fields: federation__FieldSet!) on OBJECT

            type Query {
                hello: String
            }

            type A @key(fields: "id") {
                id: ID!
                name: String @shareable
            }
        "#,
    )
    .expect("parses schema")
    .expand_links()
    .expect("expands schema");

    let [validated]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph])
        .expect("validates schema")
        .try_into()
        .expect("Expected 1 element");

    assert_snapshot!(validated.schema_string());
}
