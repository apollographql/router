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

/// Asserts the `(code, message)` pairs of a failed composition.
#[track_caller]
fn assert_errors(sources: &[(&str, &str)], expected: &[(&str, &str)]) {
    let errors = errors(sources);
    let errors: Vec<(&str, &str)> = errors
        .iter()
        .map(|(code, message)| (code.as_str(), message.as_str()))
        .collect();
    assert_eq!(errors, expected);
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
    fn user_copy_of_apollo_key_definition_still_rejects_resolvable() {
        let products = format!(
            "{APOLLO_KEY_DEFINITION}{}",
            r#"
            type Query { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id", resolvable: false) { id: ID! }
            "#
        );
        assert_error_codes(&[("a", &products)], &["KEY_RESOLVABLE_IN_SOURCE_SCHEMA"]);
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
    fn user_lookup_directive_with_non_spec_shape_is_not_detected() {
        // Fed 1 schemas with their own argument-less `@lookup` that differs from the spec only
        // in repeatability or locations are not source schemas.
        for sdl in [
            r#"
            directive @lookup on FIELD_DEFINITION | OBJECT
            type Query { t: T @lookup }
            type T @key(fields: "id") @lookup { id: ID! }
            "#,
            r#"
            directive @lookup repeatable on FIELD_DEFINITION
            type Query { t: T @lookup @lookup }
            type T @key(fields: "id") { id: ID! }
            "#,
        ] {
            compose_sources(&[("a", sdl)]).expect("composes");
        }
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

mod lookup_validation {
    use super::*;

    #[test]
    fn lookup_must_have_arguments() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { product: Product @lookup }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["LOOKUP_MUST_HAVE_ARGUMENTS"],
        );
    }

    #[test]
    fn lookup_returns_list() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { products(id: ID!): [Product] @lookup }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["LOOKUP_RETURNS_LIST"],
        );
    }

    #[test]
    fn non_nullable_lookup_is_a_warning() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query { productById(id: ID!): Product! @lookup }
            type Product @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
        assert!(
            supergraph
                .hints()
                .iter()
                .any(|h| h.code() == "LOOKUP_RETURNS_NON_NULLABLE_TYPE"),
            "{:#?}",
            supergraph.hints()
        );
    }

    #[test]
    fn lookup_key_missing_for_type() {
        // Spec counter-example: `Clothing` has no `categoryId`.
        assert_errors(
            &[(
                "a",
                r#"
                type Query { product(id: ID!, categoryId: Int): Product @lookup }
                union Product = Electronics | Clothing
                type Electronics { id: ID! categoryId: Int name: String }
                type Clothing { id: ID! name: String }
                "#,
            )],
            &[(
                "LOOKUP_KEY_MISSING_FOR_TYPE",
                "[a] The argument \"Query.product(categoryId:)\" of the @lookup field cannot be mapped for the possible type \"Clothing\" of \"Product\": \"Clothing\" has no field named \"categoryId\".",
            )],
        );
    }

    #[test]
    fn lookup_key_mapped_for_every_type_by_is() {
        compose_sources(&[(
            "a",
            r#"
            type Query {
              mediaByKey(
                key: MediaKeyInput!
                  @is(field: "{ isbn: <Book>.isbn } | { upc: <Movie>.upc } | { feedUrl: <Podcast>.feedUrl }")
              ): Media @lookup
            }
            input MediaKeyInput @oneOf { isbn: String upc: String feedUrl: String }
            interface Media { id: ID! }
            type Book implements Media { id: ID! isbn: String! }
            type Movie implements Media { id: ID! upc: String! }
            type Podcast implements Media { id: ID! feedUrl: String! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn lookup_is_map_missing_a_type() {
        assert_errors(
            &[(
                "a",
                r#"
                type Query {
                  mediaByKey(
                    key: MediaKeyInput! @is(field: "{ isbn: <Book>.isbn } | { upc: <Movie>.upc }")
                  ): Media @lookup
                }
                input MediaKeyInput @oneOf { isbn: String upc: String }
                interface Media { id: ID! }
                type Book implements Media { id: ID! isbn: String! }
                type Movie implements Media { id: ID! upc: String! }
                type Podcast implements Media { id: ID! feedUrl: String! }
                "#,
            )],
            &[(
                "LOOKUP_KEY_MISSING_FOR_TYPE",
                "[a] The argument \"Query.mediaByKey(key:)\" of the @lookup field cannot be mapped for the possible type \"Podcast\" of \"Media\": no alternative of its @is selection map \"{ isbn: <Book>.isbn } | { upc: <Movie>.upc }\" applies to \"Podcast\".",
            )],
        );
    }

    #[test]
    fn lookup_is_map_nested_field_defined_only_in_another_schema() {
        // `IS_INVALID_FIELDS` resolves `info.code` against the other schemas, but the lookup's
        // own schema must define every field of its key.
        assert_errors(
            &[
                (
                    "a",
                    r#"
                    type Query { productByCode(code: ID! @is(field: "info.code")): Product @lookup }
                    type Product @key(fields: "id") { id: ID! info: Info @shareable }
                    type Info @shareable { name: String }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { productById(id: ID!): Product @lookup }
                    type Product @key(fields: "id") { id: ID! info: Info @shareable }
                    type Info @shareable { name: String code: ID }
                    "#,
                ),
            ],
            &[(
                "LOOKUP_KEY_MISSING_FOR_TYPE",
                "[a] The argument \"Query.productByCode(code:)\" of the @lookup field cannot be mapped for \"Product\": its @is selection map \"info.code\" selects \"Info.code\", which is not defined.",
            )],
        );
    }

    #[test]
    fn nested_lookup_is_reachable() {
        compose_sources(&[(
            "a",
            r#"
            type Query { lookups: Lookups! }
            type Lookups { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn lookup_behind_arguments_is_not_reachable() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { lookups(tenant: String): Lookups! }
                type Lookups { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["LOOKUP_NOT_REACHABLE"],
        );
    }

    #[test]
    fn lookup_behind_interface_is_not_reachable() {
        // The executor needs a concrete object type to call a nested lookup on.
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: Lookups!
                }
                interface Lookups { productBySku(sku: ID!): Product @lookup }
                type ProductLookups implements Lookups { productBySku(sku: ID!): Product }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &["LOOKUP_NOT_REACHABLE"],
        );
    }
}

mod is_validation {
    use super::*;

    #[test]
    fn is_invalid_syntax() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { personById(id: ID! @is(field: "{ id")): Person @lookup }
                type Person @key(fields: "id") { id: ID! }
                "#,
            )],
            &["IS_INVALID_SYNTAX"],
        );
    }

    #[test]
    fn is_invalid_field_type() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { personById(id: ID! @is(field: 123)): Person @lookup }
                type Person @key(fields: "id") { id: ID! }
                "#,
            )],
            &["IS_INVALID_FIELD_TYPE"],
        );
    }

    #[test]
    fn is_invalid_usage() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  personById(id: ID!): Person @lookup
                  personByName(name: String! @is(field: "name")): Person
                }
                type Person @key(fields: "id") { id: ID! name: String! }
                "#,
            )],
            &["IS_INVALID_USAGE"],
        );
    }

    #[test]
    fn is_fields_has_arguments() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { personById(id: ID! @is(field: "code(kind: X)")): Person @lookup }
                type Person @key(fields: "id") { id: ID! code(kind: String): ID! }
                "#,
            )],
            // The argumented field is also not mappable as a plain key field.
            &["IS_FIELDS_HAS_ARGUMENTS"],
        );
    }
}

mod require_validation {
    use super::*;

    #[test]
    fn require_invalid_syntax() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  delivery(size: Int! @require(field: "dimension.")): String
                }
                "#,
            )],
            &["REQUIRE_INVALID_SYNTAX"],
        );
    }

    #[test]
    fn require_invalid_usage_on_lookup() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!, region: String @require(field: "region")): Product @lookup
                }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["REQUIRE_INVALID_USAGE"],
        );
    }

    #[test]
    fn require_inconsistent_on_implementation() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { accountById(id: ID!): Account @lookup }
                interface Account @key(fields: "id") {
                  id: ID!
                  displayName(locale: String @require(field: "preferredLocale")): String
                }
                type User implements Account @key(fields: "id") {
                  id: ID!
                  displayName(locale: String): String
                }
                "#,
            )],
            &["REQUIRE_INCONSISTENT_ON_IMPLEMENTATION"],
        );
    }

    #[test]
    fn require_inconsistent_on_interface_implementing_interface() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { accountById(id: ID!): Account @lookup }
                interface Node {
                  id: ID!
                  displayName(locale: String @require(field: "preferredLocale")): String
                }
                interface Account implements Node @key(fields: "id") {
                  id: ID!
                  displayName(locale: String): String
                }
                "#,
            )],
            &["REQUIRE_INCONSISTENT_ON_IMPLEMENTATION"],
        );
    }
}

mod internal_validation {
    use super::*;

    #[test]
    fn reference_to_internal_type() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: InternalLookups!
                }
                type InternalLookups @internal { productBySku(sku: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &["REFERENCE_TO_INTERNAL_TYPE"],
        );
    }

    #[test]
    fn reference_to_type_with_only_internal_fields() {
        // `Lookups` is internal (spec `IsInternalType`) and is not merged, so a public field
        // cannot return it.
        assert_errors(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup lookups: Lookups }
                type Lookups { productBySku(sku: ID!): Product @lookup @internal }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &[(
                "REFERENCE_TO_INTERNAL_TYPE",
                "[a] The field \"Query.lookups\" references the type \"Lookups\", whose fields are \
                 all @internal; internal types are local to their source schema, so only internal \
                 fields may reference them.",
            )],
        );
    }

    #[test]
    fn internal_root_lookup() {
        compose_sources(&[(
            "a",
            r#"
            type Query {
              productById(id: ID!): Product @lookup
              productBySku(sku: ID!): Product @lookup @internal
            }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn internal_root_field_and_nested_lookup_types() {
        compose_sources(&[(
            "a",
            r#"
            type Query {
              productById(id: ID!): Product @lookup
              lookups: InternalLookups! @internal
            }
            type InternalLookups @internal {
              productBySku(sku: ID!): Product @lookup
              products: ProductLookups!
            }
            type ProductLookups @internal { productByName(name: String!): Product @lookup }
            type Product @key(fields: "id") { id: ID! sku: ID! name: String! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn internal_entity_field() {
        // With its only field internal, `Product` is an internal type, so the public lookup
        // returning it is reported too.
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! @internal }
                "#,
            )],
            &["REFERENCE_TO_INTERNAL_TYPE", "INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_nested_lookup_field() {
        compose_sources(&[(
            "a",
            r#"
            type Query { lookups: Lookups! }
            type Lookups {
              productById(id: ID!): Product @lookup
              productBySku(sku: ID!): Product @lookup @internal
            }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn internal_lookup_path_field_below_the_root() {
        compose_sources(&[(
            "a",
            r#"
            type Query { lookups: Lookups! }
            type Lookups {
              productById(id: ID!): Product @lookup
              internal: InternalLookups! @internal
            }
            type InternalLookups @internal { bySku: SkuLookups! }
            type SkuLookups @internal { productBySku(sku: ID!): Product @lookup }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn internal_lookup_path_field_returning_type_with_all_fields_internal() {
        // `@internal` on a type is shorthand for `@internal` on all of its fields.
        compose_sources(&[(
            "a",
            r#"
            type Query {
              productById(id: ID!): Product @lookup
              lookups: Lookups! @internal
            }
            type Lookups { productBySku(sku: ID!): Product @lookup @internal }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn internal_type_with_non_lookup_field() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: InternalLookups! @internal
                }
                type InternalLookups @internal {
                  productBySku(sku: ID!): Product @lookup
                  version: Int
                }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_lookup_path_field_returning_interface() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: Lookups! @internal
                }
                interface Lookups { version: Int }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_root_field_without_internal_lookup_type() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: InternalLookups! @internal
                }
                type InternalLookups { productBySku(sku: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_lookup_type_leading_to_public_lookup_type() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: InternalLookups! @internal
                }
                type InternalLookups @internal { products: ProductLookups! }
                type ProductLookups { productBySku(sku: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_root_field_that_is_not_a_lookup_path() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  version: String @internal
                  allProducts: [Product!]! @internal
                }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE", "INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_root_field_leading_to_no_lookups() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  meta: Meta! @internal
                }
                type Meta @internal { version: String }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_mutation_field() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Mutation { resetProduct(id: ID!): Product @internal }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_type_that_is_not_a_lookup_type() {
        // Spec §4 "Merge Object Types" example: an internal entity type.
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") @internal { id: ID! price: Int }
                "#,
            )],
            &["REFERENCE_TO_INTERNAL_TYPE", "INTERNAL_INVALID_USAGE"],
        );
    }

    #[test]
    fn internal_type_in_union() {
        assert_errors(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  lookups: InternalLookups! @internal
                  things: [Thing]
                }
                union Thing = Product | InternalLookups
                type InternalLookups @internal { productBySku(sku: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &[(
                "REFERENCE_TO_INTERNAL_TYPE",
                "[a] The union \"Thing\" includes the @internal type \"InternalLookups\"; internal \
                 types are local to their source schema and cannot be union members.",
            )],
        );
    }

    #[test]
    fn internal_input_type_used_by_require_argument() {
        compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  delivery(
                    zip: String!
                    dimension: Dimension! @require(field: "{ height: height, size: { width: size.width } }")
                  ): Int
                }
                input Dimension @internal { height: Int! size: Size! }
                input Size @internal { width: Int! }
                "#,
            ),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! height: Int! size: ProductSize! }
                type ProductSize { width: Int! }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn internal_input_type_used_by_internal_lookup() {
        compose_sources(&[(
            "a",
            r#"
            type Query {
              productById(id: ID!): Product @lookup
              productByKey(key: ProductKey! @is(field: "{ sku: sku }")): Product @lookup @internal
            }
            input ProductKey @internal { sku: ID! }
            type Product @key(fields: "id") @key(fields: "sku") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn internal_input_type_used_by_public_argument() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  boxes(dimension: Dimension): [Product!]!
                }
                input Dimension @internal { height: Int! }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["INTERNAL_INPUT_TYPE_REFERENCED_PUBLICLY"],
        );
    }

    #[test]
    fn internal_input_type_used_by_public_input_field() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  boxes(filter: BoxFilter): [Product!]!
                }
                input BoxFilter { dimension: Dimension }
                input Dimension @internal { height: Int! }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &["INTERNAL_INPUT_TYPE_REFERENCED_PUBLICLY"],
        );
    }
}

mod carve_outs {
    use super::*;

    #[test]
    fn requires_is_rejected() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  weight: Int @external
                  shipping: Int @requires(fields: "weight")
                }
                "#,
            )],
            &["REQUIRES_IN_SOURCE_SCHEMA"],
        );
    }

    #[test]
    fn key_resolvable_is_rejected() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id", resolvable: false) { id: ID! }
                "#,
            )],
            &["KEY_RESOLVABLE_IN_SOURCE_SCHEMA"],
        );
    }

    #[test]
    fn linked_subgraph_with_lookup_cannot_use_requires_or_resolvable() {
        assert_error_codes(
            &[(
                "a",
                r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key", "@lookup", "@requires", "@external"])
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  weight: Int @external
                  shipping: Int @requires(fields: "weight")
                }
                type Other @key(fields: "id", resolvable: false) { id: ID! }
                "#,
            )],
            &[
                "REQUIRES_IN_SOURCE_SCHEMA",
                "KEY_RESOLVABLE_IN_SOURCE_SCHEMA",
            ],
        );
    }

    #[test]
    fn linked_subgraph_without_lookup_can_use_requires_and_resolvable() {
        // `@require` alone does not make a source schema, so `@requires` and
        // `@key(resolvable:)` stay available.
        compose_sources(&[
            (
                "a",
                r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key", "@require", "@requires", "@external"])
                type Query { t: T }
                type T @key(fields: "id") {
                  id: ID!
                  x: Int @external
                  f(x: Int @require(field: "x")): Int
                  g: Int @requires(fields: "x")
                }
                type Other @key(fields: "id", resolvable: false) { id: ID! }
                "#,
            ),
            (
                "b",
                r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key"])
                type Query { u: T other: Other }
                type T @key(fields: "id") { id: ID! x: Int }
                type Other @key(fields: "id") { id: ID! }
                "#,
            ),
        ])
        .expect("composes");
    }
}

mod internal_root_type {
    use super::*;

    #[test]
    fn internal_query_root_type_with_non_lookup_field() {
        assert_errors(
            &[(
                "a",
                r#"
                type Query @internal {
                  productBySku(sku: ID!): Product @lookup
                  version: String
                }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &[(
                "INTERNAL_INVALID_USAGE",
                "[a] The field \"Query.version\" of the @internal type \"Query\" must be a @lookup \
                 field, or a lookup path field without arguments that returns a single object type.",
            )],
        );
    }
}
