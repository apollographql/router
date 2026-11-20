//! GraphQL Federation (composite schemas) source schemas.

use apollo_federation::composition::CompositionFailure;
use apollo_federation::subgraph::typestate::Initial;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::supergraph::Satisfiable;
use apollo_federation::supergraph::Supergraph;

use super::test_helpers::compose;
use super::test_helpers::print_sdl;

fn source(name: &str, sdl: &str) -> Subgraph<Initial> {
    Subgraph::parse(name, &format!("http://{name}"), sdl).expect("parses")
}

/// Composes the sources. The crate's tests build with the `graphql_federation` feature, so
/// source schemas are allowed to merge.
fn compose_sources(
    sources: &[(&str, &str)],
) -> Result<Supergraph<Satisfiable>, CompositionFailure> {
    compose(
        sources
            .iter()
            .map(|(name, sdl)| source(name, sdl))
            .collect(),
    )
}

/// The `(code, message)` pairs of a failed composition.
#[track_caller]
fn errors(sources: &[(&str, &str)]) -> Vec<(String, String)> {
    match compose_sources(sources) {
        Ok(supergraph) => panic!(
            "expected composition to fail, got:\n{}",
            print_sdl(supergraph.schema().schema())
        ),
        Err(failure) => failure
            .errors
            .iter()
            .map(|e| (e.code().definition().code().to_string(), e.to_string()))
            .collect(),
    }
}

#[track_caller]
fn assert_error_codes(sources: &[(&str, &str)], expected: &[&str]) {
    let errors = errors(sources);
    let codes: Vec<&str> = errors.iter().map(|(code, _)| code.as_str()).collect();
    assert_eq!(codes, expected, "errors: {errors:#?}");
}

const PRODUCTS: &str = r#"
    type Query {
      productById(id: ID!): Product @lookup
      topProducts: [Product!]!
    }
    type Product @key(fields: "id") {
      id: ID!
      name: String!
    }
"#;

mod detection {
    use super::*;

    #[test]
    fn source_schema_without_link_composes_as_federation_2() {
        let supergraph = compose_sources(&[("products", PRODUCTS)]).expect("composes");
        let sdl = print_sdl(supergraph.schema().schema());
        assert!(sdl.contains("productById(id: ID!): Product"), "{sdl}");
    }

    #[test]
    fn source_schema_gets_no_entities_field() {
        // A source schema resolves entities through its lookups, not `_entities`.
        let validated = |sdl: &str| {
            source("a", sdl)
                .expand_links()
                .expect("expands")
                .assume_upgraded()
                .validate()
                .expect("validates")
                .schema_string()
        };
        let sdl = validated(PRODUCTS);
        for federation_only in ["_entities", "_Entity", "_Any"] {
            assert!(!sdl.contains(federation_only), "{sdl}");
        }
        assert!(sdl.contains("_service: _Service!"), "{sdl}");

        // A federation subgraph still gets them.
        let sdl = validated(
            r#"
            extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key"])
            type Query { topProducts: [Product!]! }
            type Product @key(fields: "id") { id: ID! }
            "#,
        );
        assert!(
            sdl.contains("_entities(representations: [_Any!]!): [_Entity]!"),
            "{sdl}"
        );
    }

    #[test]
    fn schema_with_key_but_no_composite_directive_stays_federation_1() {
        // Both schemas share the value type `Money` without `@shareable`. Read as Fed 1, the
        // upgrade makes its fields shareable and composition succeeds; read as source schemas
        // (with a `@lookup`), the same fields are not shareable.
        let schema = |lookup: &str| {
            format!(
                r#"
                type Query {{ t: T {lookup} }}
                type T @key(fields: "id") {{ id: ID! price: Money }}
                type Money {{ amount: Int }}
                "#
            )
        };
        let a = schema("");
        let b = schema("").replace("t: T", "u: T").replace("price", "cost");
        compose_sources(&[("a", &a), ("b", &b)]).expect("composes as Fed 1");

        let a = schema("tById(id: ID!): T @lookup");
        let b = schema("uById(id: ID!): T @lookup")
            .replace("t: T", "u: T")
            .replace("price", "cost");
        let codes: Vec<String> = errors(&[("a", &a), ("b", &b)])
            .into_iter()
            .map(|(code, _)| code)
            .collect();
        assert!(
            codes.iter().any(|code| code == "INVALID_FIELD_SHARING"),
            "{codes:?}"
        );
    }

    #[test]
    fn schema_without_lookup_stays_federation_1() {
        // `@require` alone does not make a source schema: read as Fed 1, it is an unknown
        // directive (a source schema would accept it).
        let errors = errors(&[(
            "a",
            r#"
            type Query { t: T }
            type T @key(fields: "id") {
              id: ID!
              f(x: Int @require(field: "x")): Int
            }
            "#,
        )]);
        assert!(
            errors
                .iter()
                .any(|(code, message)| code == "INVALID_GRAPHQL"
                    && message.contains("cannot find directive `@require`")),
            "{errors:#?}"
        );
    }

    #[test]
    fn user_copies_of_spec_definitions_are_replaced() {
        compose_sources(&[(
            "products",
            r#"
            directive @lookup on FIELD_DEFINITION
            directive @key(fields: FieldSelectionSet!) repeatable on OBJECT | INTERFACE
            directive @is(field: FieldSelectionMap!) on ARGUMENT_DEFINITION
            scalar FieldSelectionSet
            scalar FieldSelectionMap
            type Query {
              productById(productId: ID! @is(field: "id")): Product @lookup
            }
            type Product @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
    }

    const APOLLO_KEY_DEFINITION: &str = r#"
        directive @key(fields: FieldSet!, resolvable: Boolean = true) repeatable on OBJECT | INTERFACE
        scalar FieldSet
    "#;

    #[test]
    fn user_copy_of_apollo_key_definition_keeps_the_key() {
        // A copied Apollo `@key` definition stays the federation `@key`: `Product` is an entity
        // in both schemas, so its key field can be shared.
        let products = format!(
            "{APOLLO_KEY_DEFINITION}{}",
            r#"
            type Query { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id") { id: ID! name: String }
            "#
        );
        let supergraph = compose_sources(&[
            ("a", &products),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! price: Int }
                "#,
            ),
        ])
        .expect("composes");
        let sdl = print_sdl(supergraph.schema().schema());
        assert!(sdl.contains(r#"@join__type(graph: A, key: "id")"#), "{sdl}");
        assert!(!sdl.contains("scalar FieldSet"), "{sdl}");
    }

    #[test]
    fn user_copy_of_tag_definition_keeps_the_tag() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            directive @tag(name: String!) repeatable on FIELD_DEFINITION | OBJECT | INTERFACE | UNION | ARGUMENT_DEFINITION | SCALAR | ENUM | ENUM_VALUE | INPUT_OBJECT | INPUT_FIELD_DEFINITION
            type Query { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id") { id: ID! name: String @tag(name: "public") }
            "#,
        )])
        .expect("composes");
        let sdl = print_sdl(supergraph.schema().schema());
        assert!(
            sdl.contains(r#"name: String @tag(name: "public")"#),
            "{sdl}"
        );
    }

    #[test]
    fn unrelated_key_definition_is_reported() {
        // An applied `@key` is the federation `@key`; a definition that does not match it is an
        // error rather than a custom directive.
        assert_error_codes(
            &[(
                "a",
                r#"
                directive @key(table: String!) on OBJECT
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(table: "products") { id: ID! }
                "#,
            )],
            &[
                "DIRECTIVE_DEFINITION_INVALID",
                "DIRECTIVE_DEFINITION_INVALID",
            ],
        );
    }

    #[test]
    fn spec_scalar_still_referenced_is_kept() {
        compose_sources(&[(
            "a",
            r#"
            directive @key(fields: FieldSelectionSet!) repeatable on OBJECT | INTERFACE
            directive @myIndex(fields: FieldSelectionSet!) on OBJECT
            scalar FieldSelectionSet
            type Query { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id") @myIndex(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn unrelated_user_lookup_directive_is_not_detected() {
        // A Fed 1 schema with its own `@lookup(table:)` directive is not a source schema.
        compose_sources(&[(
            "a",
            r#"
            directive @lookup(table: String!) on FIELD_DEFINITION
            type Query { t: T @lookup(table: "t") }
            type T @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
    }
}
