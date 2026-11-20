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

    #[test]
    fn lookup_on_interface_field_is_rejected() {
        // The implementing type is reachable and declares its own lookup, so the directive on the
        // interface field is the only error.
        assert_errors(
            &[(
                "a",
                r#"
                type Query { productLookups: ProductLookups! }
                interface Lookups { productById(id: ID!): Product @lookup }
                type ProductLookups implements Lookups {
                  productById(id: ID!): Product @lookup
                }
                type Product @key(fields: "id") { id: ID! }
                "#,
            )],
            &[(
                "LOOKUP_NOT_REACHABLE",
                "[a] The @lookup field \"Lookups.productById\" is declared by the interface type \
                 \"Lookups\", but @lookup is only valid on fields of object types. An implementing \
                 field is a lookup field only if it is annotated with @lookup itself.",
            )],
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

    #[test]
    fn external_require_collision() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    interface Titled {
                      title(suffix: String @require(field: "subtitle")): String
                      subtitle: String
                    }
                    type Query { bookById(id: ID!): Book @lookup }
                    type Book implements Titled @key(fields: "id") {
                      id: ID!
                      title(suffix: String @require(field: "subtitle")): String @external
                      subtitle: String
                    }
                    "#,
                ),
                (
                    "b",
                    r#"type Book @key(fields: "id") { id: ID! title(suffix: String): String }"#,
                ),
            ],
            &["EXTERNAL_REQUIRE_COLLISION"],
        );
    }

    #[test]
    fn external_require_collision_on_external_type() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    interface Titled {
                      title(suffix: String @require(field: "subtitle")): String
                      subtitle: String
                    }
                    type Query { bookById(id: ID!): Book @lookup }
                    type Book implements Titled @key(fields: "id") @external {
                      id: ID!
                      title(suffix: String @require(field: "subtitle")): String
                      subtitle: String
                    }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Book @key(fields: "id") {
                      id: ID!
                      title(suffix: String): String
                      subtitle: String
                    }
                    "#,
                ),
            ],
            &["EXTERNAL_REQUIRE_COLLISION"],
        );
    }

    #[test]
    fn require_on_non_external_field_is_not_a_collision() {
        compose_sources(&[
            (
                "a",
                r#"
                type Query { bookById(id: ID!): Book @lookup }
                type Book @key(fields: "id") {
                  id: ID!
                  title(suffix: String @require(field: "subtitle")): String
                }
                "#,
            ),
            (
                "b",
                r#"
                type Query { bookByIdInB(id: ID!): Book @lookup }
                type Book @key(fields: "id") { id: ID! subtitle: String }
                "#,
            ),
        ])
        .expect("composes");
    }
}

mod internal_validation {
    use super::*;

    #[test]
    fn internal_override_collision_on_field() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query {
                      productById(id: ID!): Product @lookup @internal @override(from: "b")
                    }
                    type Product @key(fields: "id") { id: ID! }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { productById(id: ID!): Product @lookup }
                    type Product @key(fields: "id") { id: ID! }
                    "#,
                ),
            ],
            &["INTERNAL_OVERRIDE_COLLISION"],
        );
    }

    #[test]
    fn internal_override_collision_on_internal_type() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query @internal {
                      productById(id: ID!): Product @lookup @override(from: "b")
                    }
                    type Product @key(fields: "id") { id: ID! }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { productById(id: ID!): Product @lookup }
                    type Product @key(fields: "id") { id: ID! }
                    "#,
                ),
            ],
            &["INTERNAL_OVERRIDE_COLLISION"],
        );
    }

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
    fn internal_lookup_implementing_public_interface_field() {
        assert_error_codes(
            &[(
                "a",
                r#"
                interface Lookups { productBySku(sku: ID!): Product }
                type Query implements Lookups {
                  productById(id: ID!): Product @lookup
                  productBySku(sku: ID!): Product @lookup @internal
                }
                type Product @key(fields: "id") { id: ID! sku: ID! }
                "#,
            )],
            &["INTERFACE_FIELD_NO_IMPLEM"],
        );
    }

    #[test]
    fn internal_type_by_fields_implementing_public_interface_field() {
        // `Lookups` has only internal fields, so it is not merged and its `implements` clause
        // does not constrain the composite schema.
        compose_sources(&[(
            "a",
            r#"
            interface Finder { productBySku(sku: ID!): Product }
            type Query {
              productById(id: ID!): Product @lookup
              lookups: Lookups! @internal
            }
            type Lookups implements Finder {
              productBySku(sku: ID!): Product @lookup @internal
            }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
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

mod cross_schema_validation {
    use super::*;

    #[test]
    fn is_fields_resolve_against_the_return_type() {
        compose_sources(&[(
            "a",
            r#"
            type Query { personById(productId: ID! @is(field: "id")): Person @lookup }
            type Person @key(fields: "id") { id: ID! name: String }
            "#,
        )])
        .expect("composes");
    }

    #[test]
    fn is_unknown_field_is_caught_as_unmappable_key() {
        // Spec counter-example for `IS_INVALID_FIELDS`: in the lookup's own schema the source-schema
        // rule `LOOKUP_KEY_MISSING_FOR_TYPE` already rejects it.
        assert_errors(
            &[(
                "a",
                r#"
                type Query { personById(id: ID! @is(field: "unknownField")): Person @lookup }
                type Person @key(fields: "id") { id: ID! name: String }
                "#,
            )],
            &[(
                "LOOKUP_KEY_MISSING_FOR_TYPE",
                "[a] The argument \"Query.personById(id:)\" of the @lookup field cannot be mapped for \"Person\": its @is selection map \"unknownField\" selects \"Person.unknownField\", which is not defined.",
            )],
        );
    }

    #[test]
    fn is_invalid_fields_type_mismatch() {
        // Appendix A "Values of Correct Type" counter-example.
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { storeById(id: ID! @is(field: "id")): Store @lookup }
                type Store @key(fields: "id") { id: Int city: String! }
                "#,
            )],
            &["IS_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_selects_fields_of_another_schema() {
        compose_sources(&[
            (
                "a",
                r#"
                type Query { userById(id: ID!): User @lookup }
                type User @key(fields: "id") {
                  id: ID!
                  profile(name: String @require(field: "name")): Profile
                }
                type Profile { id: ID! name: String }
                "#,
            ),
            (
                "b",
                r#"
                type Query { userByIdInB(id: ID!): User @lookup }
                type User @key(fields: "id") { id: ID! name: String }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn require_with_constant_arguments() {
        compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  shippingCost(weight: Float @require(field: "weight(unit: IMPERIAL)")): Int
                }
                "#,
            ),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! weight(unit: WeightUnit!): Float }
                enum WeightUnit { METRIC IMPERIAL }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn require_invalid_fields_unknown_field() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { bookById(id: ID!): Book @lookup }
                type Book @key(fields: "id") {
                  id: ID!
                  pages(pageSize: Int @require(field: "unknownField")): Int
                }
                "#,
            )],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_invalid_fields_own_field_also_declared_elsewhere() {
        // The declaring schema has `size` itself; that another schema also has it doesn't help.
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query { bookById(id: ID!): Book @lookup }
                    type Book @key(fields: "id") {
                      id: ID!
                      size: Int @shareable
                      pages(pageSize: Int @require(field: "size")): Int
                    }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { bookByIdInB(id: ID!): Book @lookup }
                    type Book @key(fields: "id") { id: ID! size: Int @shareable }
                    "#,
                ),
            ],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_invalid_fields_own_field_in_selected_object() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query { bookById(id: ID!): Book @lookup }
                    type Book @key(fields: "id") {
                      id: ID!
                      size: Int @shareable
                      pages(dims: DimsInput @require(field: "{ size, weight }")): Int
                    }
                    input DimsInput { size: Int weight: Int }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { bookByIdInB(id: ID!): Book @lookup }
                    type Book @key(fields: "id") { id: ID! size: Int @shareable weight: Int }
                    "#,
                ),
            ],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_own_field_overridden_by_another_schema() {
        // `b` takes `size` over from `a`, so `a` no longer resolves it and may require it.
        compose_sources(&[
            (
                "a",
                r#"
                type Query { bookById(id: ID!): Book @lookup }
                type Book @key(fields: "id") {
                  id: ID!
                  size: Int
                  pages(pageSize: Int @require(field: "size")): Int
                }
                "#,
            ),
            (
                "b",
                r#"
                type Query { bookByIdInB(id: ID!): Book @lookup }
                type Book @key(fields: "id") { id: ID! size: Int @override(from: "a") }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn require_own_external_field() {
        // An `@external` field is not declared by this schema, so it may be required. (`@require`
        // doesn't count as a use of it, so `@provides` keeps it from being `EXTERNAL_UNUSED`.)
        compose_sources(&[
            (
                "a",
                r#"
                type Query {
                  bookById(id: ID!): Book @lookup
                  featuredBook: Book @provides(fields: "size")
                }
                type Book @key(fields: "id") {
                  id: ID!
                  size: Int @external
                  pages(pageSize: Int @require(field: "size")): Int
                }
                "#,
            ),
            (
                "b",
                r#"
                type Query { bookByIdInB(id: ID!): Book @lookup }
                type Book @key(fields: "id") { id: ID! size: Int @shareable }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn require_invalid_fields_own_field() {
        // Spec counter-example: a requirement cannot be satisfied by the declaring schema.
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { bookById(id: ID!): Book @lookup }
                type Book @key(fields: "id") {
                  id: ID!
                  size: Int
                  pages(pageSize: Int @require(field: "size")): Int
                }
                "#,
            )],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_selects_field_whose_required_argument_is_itself_required() {
        // `Product.price(region:)` carries `@require`, so the executor supplies it and the map
        // does not have to (Appendix A, "Path Field Argument Validity").
        let a = r#"
            type Query { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id") {
              id: ID!
              shipping(price: Int @require(field: "price")): Int
            }
        "#;
        let c = r#"
            type Query { productByIdInC(id: ID!): Product @lookup }
            type Product @key(fields: "id") { id: ID! region: String! }
        "#;
        compose_sources(&[
            ("a", a),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  price(region: String! @require(field: "region")): Int
                }
                "#,
            ),
            ("c", c),
        ])
        .expect("composes");

        // Without `@require`, the required argument must be supplied.
        assert_error_codes(
            &[
                ("a", a),
                (
                    "b",
                    r#"
                    type Query { productByIdInB(id: ID!): Product @lookup }
                    type Product @key(fields: "id") { id: ID! price(region: String!): Int }
                    "#,
                ),
                ("c", c),
            ],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_type_condition_uses_the_declaring_schema_type_relationships() {
        // `User implements Account` is only declared by the declaring schema, which still makes
        // `<User>` a valid type condition on `Account`.
        let a = r#"
            type Query { accountById(id: ID!): Account @lookup userById(id: ID!): User @lookup }
            interface Account @key(fields: "id") {
              id: ID!
              displayName(locale: String @require(field: "<User>.preferredLocale")): String
            }
            type User implements Account @key(fields: "id") {
              id: ID!
              displayName(locale: String @require(field: "preferredLocale")): String
            }
        "#;
        compose_sources(&[
            ("a", a),
            (
                "b",
                r#"
                type Query { userByIdInB(id: ID!): User @lookup }
                type User @key(fields: "id") { id: ID! preferredLocale: String }
                "#,
            ),
        ])
        .expect("composes");

        // The field itself must still come from another schema.
        assert_error_codes(
            &[
                ("a", a),
                (
                    "b",
                    r#"
                    type Query { userByIdInB(id: ID!): User @lookup }
                    type User @key(fields: "id") { id: ID! locale: String }
                    "#,
                ),
            ],
            &["REQUIRE_INVALID_FIELDS", "REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_selects_field_of_an_interface_object_stand_in() {
        // `Book.price` is contributed by the `@interfaceObject` stand-in for `Media` in "b".
        let a = r#"
            type Query { bookById(id: ID!): Book @lookup mediaById(id: ID!): Media @lookup }
            interface Media @key(fields: "id") { id: ID! }
            type Book implements Media @key(fields: "id") {
              id: ID!
              summary(price: Int @require(field: "price")): String
            }
        "#;
        let b = |fields: &str| {
            format!(
                r#"
                extend schema @link(url: "https://specs.apollo.dev/federation/v3.0", import: ["@key", "@interfaceObject"])
                type Query {{ media: Media }}
                type Media @interfaceObject @key(fields: "id") {{ id: ID! {fields} }}
                "#
            )
        };
        compose_sources(&[("a", a), ("b", &b("price: Int"))]).expect("composes");
        assert_error_codes(
            &[("a", a), ("b", &b("cost: Int"))],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }
}

/// The supergraph SDL without the `join__Graph` enum, whose `internalDefinitions` record the
/// `@internal` elements that the merged types leave out.
fn merged_types_sdl(supergraph: &Supergraph<Satisfiable>) -> String {
    print_sdl(supergraph.schema().schema())
        .split("\n\n")
        .filter(|block| !block.starts_with("enum join__Graph"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

mod internal_merging {
    use super::*;

    #[test]
    fn internal_elements_are_not_merged() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query {
              top: Product
              productBySku(sku: ID!): Product @lookup @internal
              productByKey(key: ProductKey! @is(field: "{ id }")): Product @lookup @internal
              lookups: InternalLookups @internal
            }
            type InternalLookups @internal { productById(id: ID!): Product @lookup }
            input ProductKey @internal { id: ID! }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
        let sdl = merged_types_sdl(&supergraph);
        for internal in [
            "productBySku",
            "productByKey",
            "lookups",
            "InternalLookups",
            "ProductKey",
        ] {
            assert!(
                !sdl.contains(internal),
                "{internal} is in the supergraph:\n{sdl}"
            );
        }
        assert!(sdl.contains("top: Product"), "{sdl}");
    }

    #[test]
    fn internal_lookup_does_not_conflict_with_public_lookup() {
        // Spec `INVALID_FIELD_SHARING` excludes `@internal` declarations.
        let supergraph = compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! name: String }
                "#,
            ),
            (
                "b",
                r#"
                type Query { productById(id: ID!): Product @lookup @internal }
                type Product @key(fields: "id") { id: ID! price: Int }
                "#,
            ),
        ])
        .expect("composes");
        let sdl = merged_types_sdl(&supergraph);
        assert!(
            sdl.contains(
                "productById(id: ID!): Product @join__field(graph: A, lookup: true, lookupArguments: [{name: \"id\", type: \"ID!\"}])\n"
            ),
            "{sdl}"
        );
    }

    #[test]
    fn require_arguments_and_their_internal_input_types_are_not_merged() {
        // `@require` arguments are supplied by the executor and do not participate in merging,
        // and neither do internal input types.
        let supergraph = compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  shipping(zip: String, dimension: Dimension! @require(field: "{ width }")): Int
                }
                input Dimension @internal { width: Int }
                "#,
            ),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! width: Int }
                "#,
            ),
        ])
        .expect("composes");
        let sdl = merged_types_sdl(&supergraph);
        assert!(!sdl.contains("input Dimension"), "{sdl}");
        assert!(
            sdl.contains(
                "shipping(zip: String): Int @join__field(graph: A, requireArguments: [{name: \"dimension\", type: \"Dimension!\", selection: \"{ width }\"}])"
            ),
            "{sdl}"
        );
    }

    #[test]
    fn internal_input_types_do_not_conflict_across_schemas() {
        // Spec "Merge Input Types": internal input types neither contribute fields to nor
        // constrain an input type of the same name in another source schema.
        let supergraph = compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  shipping(dimension: Dimension @require(field: "{ width }")): Int
                }
                input Dimension @internal { width: Int }
                "#,
            ),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  tax(dimension: Dimension @require(field: "{ length: height }")): Int
                }
                input Dimension @internal { length: Int! }
                "#,
            ),
            (
                "c",
                r#"
                type Query {
                  productByIdInC(id: ID!): Product @lookup
                  boxes(dimension: Dimension): [Int]
                }
                type Product @key(fields: "id") { id: ID! width: Int height: Int! }
                input Dimension { depth: Int! }
                "#,
            ),
        ])
        .expect("composes");
        let sdl = merged_types_sdl(&supergraph);
        assert!(
            sdl.contains("input Dimension @join__type(graph: C) {\n  depth: Int!\n}"),
            "{sdl}"
        );
    }

    #[test]
    fn recursive_internal_input_types_are_not_merged() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query {
              productById(id: ID!): Product @lookup
              productByKey(key: ProductKey! @is(field: "{ sku }")): Product @lookup @internal
            }
            input ProductKey @internal { sku: ID! fallback: ProductKey other: OtherKey }
            input OtherKey @internal { key: ProductKey }
            type Product @key(fields: "id") { id: ID! sku: ID! }
            "#,
        )])
        .expect("composes");
        let sdl = merged_types_sdl(&supergraph);
        assert!(!sdl.contains("ProductKey"), "{sdl}");
        assert!(!sdl.contains("OtherKey"), "{sdl}");
    }
}

mod internal_root_type {
    use super::*;

    #[test]
    fn internal_query_root_type_holds_only_internal_lookups() {
        // `@internal` on a type is shorthand for `@internal` on each of its fields, so the
        // lookups of "b" stay out of the supergraph. The `_service`/`_entities` fields that
        // federation adds are not subject to `INTERNAL_INVALID_USAGE`.
        let supergraph = compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! name: String sku: ID! @shareable }
                "#,
            ),
            (
                "b",
                r#"
                type Query @internal { productBySku(sku: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! sku: ID! @shareable price: Int }
                "#,
            ),
        ])
        .expect("composes");
        let sdl = merged_types_sdl(&supergraph);
        assert!(!sdl.contains("productBySku"), "{sdl}");
        assert!(sdl.contains("price: Int @join__field(graph: B)"), "{sdl}");
    }

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

mod cross_schema_order {
    use super::*;

    const A: &str = r#"
        type Query { productById(id: ID!): Product @lookup }
        type Product @key(fields: "id") {
          id: ID!
          shippingCost(weight: Float @require(field: "weight")): Float
        }
    "#;
    const REQUIRED: &str = r#"
        enum WeightUnit { KG LB }
        type Query { productByIdB(id: ID!): Product @lookup }
        type Product @key(fields: "id") { id: ID! weight(unit: WeightUnit!): Float @shareable }
    "#;
    const OPTIONAL: &str = r#"
        enum WeightUnit { KG LB }
        type Query { productByIdC(id: ID!): Product @lookup }
        type Product @key(fields: "id") { id: ID! weight(unit: WeightUnit): Float @shareable }
    "#;

    #[test]
    fn require_argument_check_does_not_depend_on_subgraph_order() {
        assert_error_codes(
            &[("a", A), ("b", REQUIRED), ("c", OPTIONAL)],
            &["REQUIRE_INVALID_FIELDS"],
        );
        assert_error_codes(
            &[("a", A), ("b", OPTIONAL), ("c", REQUIRED)],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }

    #[test]
    fn require_own_field_with_type_condition_is_reported() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query { bookById(id: ID!): Book @lookup }
                    type Book @key(fields: "id") {
                      id: ID!
                      size: Int @shareable
                      pages(pageSize: Int @require(field: "<Book>.size")): Int
                    }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { bookByIdInB(id: ID!): Book @lookup }
                    type Book @key(fields: "id") { id: ID! size: Int @shareable }
                    "#,
                ),
            ],
            &["REQUIRE_INVALID_FIELDS"],
        );
    }
}

/// The printed supergraph, API schema and extracted subgraph schemas, with the boilerplate
/// definitions (link, join and federation directive definitions and scalars) left out so the
/// snapshots show what the test is about.
fn composition_summary(supergraph: &Supergraph<Satisfiable>) -> String {
    fn strip(sdl: &str) -> String {
        let mut out = Vec::new();
        for block in sdl.split("\n\n") {
            let first = block.trim_start();
            let boilerplate = first.starts_with("directive @")
                || first.starts_with("scalar ")
                || first.starts_with("enum link__Purpose")
                || first.starts_with("input join__ContextArgument")
                || first.starts_with("input join__LookupArgument")
                || first.starts_with("input join__RequireArgument")
                || first.starts_with("type _Service");
            if !boilerplate {
                out.push(block.trim_end().to_string());
            }
        }
        out.join("\n\n")
    }
    let mut summary = String::new();
    summary.push_str("# Supergraph\n\n");
    summary.push_str(&strip(&print_sdl(supergraph.schema().schema())));
    let api = supergraph
        .to_api_schema(Default::default())
        .expect("api schema");
    summary.push_str("\n\n# API schema\n\n");
    summary.push_str(&strip(&print_sdl(api.schema())));
    let extracted = super::test_helpers::extract_subgraphs_from_supergraph_result(supergraph)
        .expect("extracts");
    for (name, subgraph) in extracted {
        summary.push_str(&format!("\n\n# Extracted subgraph \"{name}\"\n\n"));
        summary.push_str(&strip(&print_sdl(subgraph.schema.schema())));
    }
    summary
}

mod composition_output {
    use super::*;

    #[test]
    fn lookups_internal_elements_and_requirements() {
        let supergraph = compose_sources(&[
            (
                "products",
                r#"
                type Query {
                  productById(id: ID!): Product @lookup
                  topProducts: [Product!]!
                }
                type Product @key(fields: "id") @key(fields: "sku") {
                  id: ID!
                  sku: String!
                  name: String!
                  dimension: Dimension!
                }
                type Dimension { size: Int! weight: Int! }
                "#,
            ),
            (
                "reviews",
                r#"
                type Query {
                  lookups: InternalLookups! @internal
                }
                type InternalLookups @internal {
                  productBySku(key: String! @is(field: "sku")): Product @lookup
                }
                type Product @key(fields: "sku") {
                  sku: String! @external
                  reviewCount: Int!
                  shippingEstimate(
                    zip: String!
                    dimension: DimensionInput! @require(field: "dimension.{ size, weight }")
                  ): Int
                }
                input DimensionInput @internal { size: Int! weight: Int! }
                "#,
            ),
        ])
        .expect("composes");
        insta::assert_snapshot!(composition_summary(&supergraph));
    }

    #[test]
    fn keys_are_inferred_from_lookups() {
        // No `@key` at all: the lookups imply `id` (via `@is`) and `upc`.
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query {
              productById(productId: ID! @is(field: "id")): Product @lookup
              productByUpc(upc: String!): Product @lookup
            }
            type Product { id: ID! upc: String! name: String }
            "#,
        )])
        .expect("composes");
        insta::assert_snapshot!(composition_summary(&supergraph));
    }

    #[test]
    fn abstract_and_one_of_lookups() {
        let supergraph = compose_sources(&[(
            "media",
            r#"
            type Query {
              mediaByKey(
                key: MediaKeyInput!
                  @is(field: "{ isbn: <Book>.isbn } | { upc: <Movie>.upc }")
              ): Media @lookup
              person(by: PersonByInput! @is(field: "{ id } | { email }")): Person @lookup
            }
            input MediaKeyInput @oneOf { isbn: String upc: String }
            input PersonByInput @oneOf { id: ID email: String }
            union Media = Book | Movie
            type Book { id: ID! isbn: String! }
            type Movie { id: ID! upc: String! }
            type Person { id: ID! email: String! }
            "#,
        )])
        .expect("composes");
        insta::assert_snapshot!(composition_summary(&supergraph));
    }

    #[test]
    fn nested_public_lookup() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query { lookups: Lookups! }
            type Lookups { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
        insta::assert_snapshot!(composition_summary(&supergraph));
    }

    #[test]
    fn source_schema_selects_join_0_6() {
        let supergraph = compose_sources(&[("products", PRODUCTS)]).expect("composes");
        let sdl = print_sdl(supergraph.schema().schema());
        assert!(sdl.contains("https://specs.apollo.dev/join/v0.6"), "{sdl}");
    }

    #[test]
    fn requirement_conflicting_with_required_client_argument() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query { productById(id: ID!): Product @lookup }
                    type Product @key(fields: "id") {
                      id: ID!
                      price(weight: Int! @require(field: "weight")): Int @shareable
                    }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { productByIdInB(id: ID!): Product @lookup }
                    type Product @key(fields: "id") {
                      id: ID!
                      weight: Int
                      price(weight: Int!): Int @shareable
                    }
                    "#,
                ),
            ],
            &["REQUIRED_ARGUMENT_MISSING_IN_SOME_SUBGRAPH"],
        );
    }
}

/// One field of a subgraph extracted from the supergraph, as printed SDL.
fn extracted_field(
    supergraph: &Supergraph<Satisfiable>,
    subgraph: &str,
    type_name: &str,
    field: &str,
) -> String {
    let subgraphs = super::test_helpers::extract_subgraphs_from_supergraph_result(supergraph)
        .expect("extracts");
    let (_, subgraph) = subgraphs
        .into_iter()
        .find(|(name, _)| &**name == subgraph)
        .expect("subgraph is extracted");
    subgraph
        .schema
        .schema()
        .type_field(type_name, field)
        .expect("field is extracted")
        .to_string()
}

fn extracted_type(supergraph: &Supergraph<Satisfiable>, subgraph: &str, type_name: &str) -> String {
    let subgraphs = super::test_helpers::extract_subgraphs_from_supergraph_result(supergraph)
        .expect("extracts");
    let (_, subgraph) = subgraphs
        .into_iter()
        .find(|(name, _)| &**name == subgraph)
        .expect("subgraph is extracted");
    subgraph
        .schema
        .schema()
        .types
        .get(type_name)
        .expect("type is extracted")
        .to_string()
}

/// The `@join__type` applications on `type_name` in the supergraph.
fn join_types(supergraph: &Supergraph<Satisfiable>, type_name: &str) -> Vec<String> {
    supergraph
        .schema()
        .schema()
        .types
        .get(type_name)
        .expect("type is in the supergraph")
        .directives()
        .get_all("join__type")
        .map(|directive| directive.to_string())
        .collect()
}

mod key_resolvability {
    use super::*;

    #[test]
    fn declared_key_written_differently_is_recalled() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query { product(id: ID!, sku: String!): Product @lookup }
            type Product @key(fields: "sku id") { id: ID! sku: String! }
            "#,
        )])
        .expect("composes");
        assert_eq!(
            join_types(&supergraph, "Product"),
            [r#"@join__type(graph: A, key: "sku id")"#]
        );
    }

    #[test]
    fn declared_key_larger_than_the_lookup_key_is_not_recalled() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query { productById(id: ID!): Product @lookup }
            type Product @key(fields: "id sku") { id: ID! sku: String! }
            "#,
        )])
        .expect("composes");
        assert_eq!(
            join_types(&supergraph, "Product"),
            [
                r#"@join__type(graph: A, key: "id sku", resolvable: false)"#,
                r#"@join__type(graph: A, key: "id")"#,
            ]
        );
    }

    #[test]
    fn interface_key_is_recalled_when_every_implementation_is() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query { node(id: ID!): Node @lookup }
            interface Node @key(fields: "id") { id: ID! }
            type Product implements Node @key(fields: "id") { id: ID! }
            type Review implements Node @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
        assert_eq!(
            join_types(&supergraph, "Node"),
            [r#"@join__type(graph: A, key: "id")"#]
        );
    }

    #[test]
    fn interface_key_is_not_recalled_when_an_implementation_is_not() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query { productById(id: ID!): Product @lookup nodes: [Node] }
            interface Node @key(fields: "id") { id: ID! }
            type Product implements Node @key(fields: "id") { id: ID! }
            type Review implements Node @key(fields: "id") { id: ID! }
            "#,
        )])
        .expect("composes");
        assert_eq!(
            join_types(&supergraph, "Node"),
            [r#"@join__type(graph: A, key: "id", resolvable: false)"#]
        );
        assert_eq!(
            join_types(&supergraph, "Review"),
            [r#"@join__type(graph: A, key: "id", resolvable: false)"#]
        );
    }

    #[test]
    fn nested_key_is_inferred_from_a_lookup() {
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query {
              product(key: ProductKey! @is(field: "{ id, w: dimension.width, h: dimension.height }")): Product @lookup
            }
            input ProductKey { id: ID! w: Int! h: Int! }
            type Product { id: ID! dimension: Dimension! }
            type Dimension { width: Int! height: Int! }
            "#,
        )])
        .expect("composes");
        assert_eq!(
            join_types(&supergraph, "Product"),
            [r#"@join__type(graph: A, key: "id dimension { width height }")"#]
        );
    }
}

mod extraction {
    use super::*;

    #[test]
    fn lookup_arguments_dropped_by_merging_are_restored() {
        // `sku` is only in "a", so merging drops it from the supergraph field. The lookup in "a"
        // still takes it.
        let supergraph = compose_sources(&[
            (
                "a",
                r#"
                type Query {
                  product(id: ID @is(field: "id"), sku: String, upc: String @is(field: "upc")): Product
                    @lookup @shareable
                }
                type Product @key(fields: "id") { id: ID! sku: String! upc: String! }
                "#,
            ),
            (
                "b",
                r#"
                type Query { product(id: ID @is(field: "id")): Product @lookup @shareable }
                type Product @key(fields: "id") { id: ID! name: String }
                "#,
            ),
        ])
        .expect("composes");
        assert_eq!(
            extracted_field(&supergraph, "a", "Query", "product"),
            r#"product(
  id: ID @federation__is(field: "id"),
  sku: String,
  upc: String @federation__is(field: "upc"),
): Product @shareable @federation__lookup"#
        );
        assert_eq!(
            extracted_field(&supergraph, "b", "Query", "product"),
            r#"product(
  id: ID @federation__is(field: "id"),
): Product @shareable @federation__lookup"#
        );
    }

    #[test]
    fn require_argument_default_value_is_restored() {
        let supergraph = compose_sources(&[
            (
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") {
                  id: ID!
                  price(weight: Int = 3 @require(field: "weight")): Int
                }
                "#,
            ),
            (
                "b",
                r#"
                type Query { productByIdInB(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! weight: Int }
                "#,
            ),
        ])
        .expect("composes");
        assert_eq!(
            extracted_field(&supergraph, "a", "Product", "price"),
            r#"price(
  weight: Int = 3 @federation__require(field: "weight"),
): Int"#
        );
    }

    #[test]
    fn internal_one_of_input_keeps_one_of() {
        // The lookup's `@is` alternatives are only valid against a `@oneOf` input.
        let supergraph = compose_sources(&[(
            "a",
            r#"
            type Query {
              products: [Product]
              product(by: ProductBy! @is(field: "{ id } | { sku }")): Product @lookup @internal
            }
            input ProductBy @oneOf @internal { id: ID sku: String }
            type Product @key(fields: "id") @key(fields: "sku") { id: ID! sku: String! }
            "#,
        )])
        .expect("composes");
        assert_eq!(
            extracted_type(&supergraph, "a", "ProductBy"),
            "input ProductBy @oneOf {\n  id: ID\n  sku: String\n}\n"
        );
    }
}

mod unsupported {
    use apollo_federation::query_plan::query_planner::QueryPlanner;
    use apollo_federation::query_plan::query_planner::QueryPlannerConfig;

    use super::*;

    const PRODUCTS: &str = r#"
        type Query { productById(id: ID!): Product @lookup topProducts: [Product!]! }
        type Product @key(fields: "id") { id: ID! sku: String! name: String! }
    "#;
    const REVIEWS: &str = r#"
        type Query { lookups: InternalLookups! @internal }
        type InternalLookups @internal {
          productBySku(key: String! @is(field: "sku")): Product @lookup
        }
        type Product @key(fields: "sku") { sku: String! @external reviewCount: Int! }
    "#;

    #[test]
    fn entity_fetch_into_a_source_schema_is_unsupported() {
        let supergraph =
            compose_sources(&[("products", PRODUCTS), ("reviews", REVIEWS)]).expect("composes");
        let sdl = print_sdl(supergraph.schema().schema());
        let supergraph = apollo_federation::Supergraph::new(&sdl).expect("supergraph");
        for incremental in [false, true] {
            let mut config = QueryPlannerConfig::default();
            config.incremental_planner.enabled = incremental;
            let planner = QueryPlanner::new(&supergraph, config).expect("planner");
            let document = apollo_compiler::ExecutableDocument::parse_and_validate(
                planner.api_schema().schema(),
                "{ topProducts { reviewCount } }",
                "op.graphql",
            )
            .expect("valid operation");
            let error = planner
                .build_query_plan(&document, None, Default::default())
                .expect_err("lookups cannot be planned yet");
            assert_eq!(
                error.to_string(),
                "Subgraph `reviews` is a GraphQL Federation source schema: query planning does \
                 not support fetching entities through @lookup fields yet",
                "incremental: {incremental}"
            );
        }
    }

    #[test]
    fn connectors_cannot_be_combined_with_source_schemas() {
        // Satisfiability checks the supergraph with connectors expanded, and expansion re-merges
        // the subgraphs with the legacy merger, which would lose lookups and internal elements.
        assert_errors(
            &[
                ("products", PRODUCTS),
                (
                    "c",
                    r#"
                    extend schema
                      @link(url: "https://specs.apollo.dev/federation/v2.11", import: ["@key"])
                      @link(url: "https://specs.apollo.dev/connect/v0.1", import: ["@connect", "@source"])
                      @source(name: "v1", http: { baseURL: "http://v1" })
                    type Query {
                      resources: [Resource!]!
                        @connect(source: "v1", http: { GET: "/resources" }, selection: "id name")
                    }
                    type Resource { id: ID! name: String! }
                    "#,
                ),
            ],
            &[(
                "UNSUPPORTED_FEATURE",
                "Subgraph `products` is a GraphQL Federation source schema: connectors cannot be \
                 expanded in a supergraph with source schemas yet",
            )],
        );
    }
}
