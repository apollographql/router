use apollo_compiler::coord;
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

    let [upgraded]: [Subgraph<_>; 1] = upgrade_subgraphs_if_necessary(vec![subgraph])
        .expect("upgrades schema")
        .try_into()
        .expect("Expected 1 element");

    let custom_directive = coord!(@customDirective)
        .lookup(upgraded.validated_schema().schema())
        .expect("directive definition");
    assert_snapshot!(custom_directive, @r#"
        " This is a description of the custom directive "
        directive @customDirective(value: String!) on FIELD_DEFINITION | OBJECT
    "#);
}

/// Fed1 schema with federation directive description - description should now be preserved after upgrade.
#[test]
fn fed1_preserves_federation_directive_descriptions() {
    let subgraph = Subgraph::parse(
        "subgraph",
        "",
        r#"
            """ This is a custom description of the key directive """
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

    let key_directive = coord!(@key)
        .lookup(upgraded.validated_schema().schema())
        .expect("directive definition");
    assert_snapshot!(key_directive, @r#"
        " This is a custom description of the key directive "
        directive @key(fields: federation__FieldSet!) on OBJECT
    "#);
}

// =============================================================================
// Fed2 Schema Passthrough Tests
// =============================================================================

/// Fed2 schema with custom directive - upgrade is a no-op, so description should be unchanged.
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

    let validate_directive = coord!(@validate)
        .lookup(validated.validated_schema().schema())
        .expect("directive definition");
    assert_snapshot!(validate_directive, @r#"
        " A custom validation directive for Fed2 "
        directive @validate(pattern: String!) on FIELD_DEFINITION | ARGUMENT_DEFINITION
    "#);
}

/// Fed2 schema with custom description on federation directive - upgrade is a no-op so
/// full definition and description should be unchanged
#[test]
fn fed2_uses_standard_federation_directive_definitions() {
    let subgraph = Subgraph::parse(
        "subgraph",
        "",
        r#"
            extend schema
                @link(url: "https://specs.apollo.dev/federation/v2.3", import: ["@key", "@shareable"])

            """ A custom description for the key directive in Fed2 """
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

    let key_directive = coord!(@key)
        .lookup(validated.validated_schema().schema())
        .expect("directive definition");
    assert_snapshot!(key_directive, @r#"
        " A custom description for the key directive in Fed2 "
        directive @key(fields: federation__FieldSet!) on OBJECT
    "#);
}
