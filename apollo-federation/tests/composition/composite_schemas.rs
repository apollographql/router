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
    fn schema_with_key_but_no_composite_directive_stays_federation_1() {
        // `@shareable` is not a Fed 1 directive, so a Fed 1 reading rejects it; a composite reading
        // would accept it. No `@lookup`/`@internal`/`@is`/`@require` means Fed 1.
        let result = compose_sources(&[(
            "a",
            r#"
            type Query { t: T }
            type T @key(fields: "id") { id: ID! }
            "#,
        )]);
        assert!(result.is_ok());
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
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { product(id: ID!, categoryId: Int): Product @lookup }
                union Product = Electronics | Clothing
                type Electronics { id: ID! categoryId: Int name: String }
                type Clothing { id: ID! name: String }
                "#,
            )],
            &["LOOKUP_KEY_MISSING_FOR_TYPE"],
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
        assert_error_codes(
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
            &["LOOKUP_KEY_MISSING_FOR_TYPE"],
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
    fn internal_field_cannot_be_a_key_field() {
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { productById(id: ID!): Product @lookup }
                type Product @key(fields: "id") { id: ID! @internal }
                "#,
            )],
            &["KEY_INVALID_FIELDS"],
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
        assert_error_codes(
            &[(
                "a",
                r#"
                type Query { personById(id: ID! @is(field: "unknownField")): Person @lookup }
                type Person @key(fields: "id") { id: ID! name: String }
                "#,
            )],
            &["LOOKUP_KEY_MISSING_FOR_TYPE"],
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
    fn require_cannot_select_internal_fields() {
        assert_error_codes(
            &[
                (
                    "a",
                    r#"
                    type Query { bookById(id: ID!): Book @lookup }
                    type Book @key(fields: "id") {
                      id: ID!
                      pages(pageSize: Int @require(field: "size")): Int
                    }
                    "#,
                ),
                (
                    "b",
                    r#"
                    type Query { bookByIdInB(id: ID!): Book @lookup }
                    type Book @key(fields: "id") { id: ID! size: Int @internal }
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
                || first.starts_with("input join__IsArgument")
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
                  productBySku(sku: String!): Product @lookup @internal
                  topProducts: [Product!]!
                }
                type Product @key(fields: "id") @key(fields: "sku") @key(fields: "upc") {
                  id: ID!
                  sku: String!
                  upc: String!
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
                  sku: String!
                  reviewCount: Int!
                  shippingEstimate(
                    zip: String!
                    dimension: DimensionInput! @require(field: "dimension.{ size, weight }")
                  ): Int
                }
                input DimensionInput { size: Int! weight: Int! }
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
    fn federation_2_16_without_source_schemas_keeps_join_0_5() {
        let supergraph = super::super::test_helpers::compose_as_fed2_subgraphs(&[
            super::super::test_helpers::ServiceDefinition {
                name: "a",
                type_defs: r#"
                type Query { t: T }
                type T @key(fields: "id") { id: ID! }
                "#,
            },
        ])
        .expect("composes");
        let sdl = print_sdl(supergraph.schema().schema());
        assert!(sdl.contains("https://specs.apollo.dev/join/v0.5"), "{sdl}");
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

mod satisfiability {
    use super::*;

    #[test]
    fn entity_without_lookup_is_not_reachable() {
        // `reviews` declares `@key(fields: "id")` but no lookup recalls `Product` there, so the key is
        // identity only and `reviewCount` cannot be reached.
        let errors = errors(&[
            ("products", PRODUCTS),
            (
                "reviews",
                r#"
                type Query { review(id: ID!): Review @lookup }
                type Review @key(fields: "id") { id: ID! }
                type Product @key(fields: "id") { id: ID! reviewCount: Int! }
                "#,
            ),
        ]);
        let codes: Vec<&str> = errors.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, ["SATISFIABILITY_ERROR"], "{errors:#?}");
        assert!(errors[0].1.contains("reviewCount"), "{errors:#?}");
        assert!(
            errors[0]
                .1
                .contains("no @lookup field in subgraph \"reviews\" resolves type \"Product\""),
            "{errors:#?}"
        );
    }

    #[test]
    fn entity_with_lookup_is_reachable() {
        compose_sources(&[
            ("products", PRODUCTS),
            (
                "reviews",
                r#"
                type Query { productById(id: ID!): Product @lookup @internal }
                type Product @key(fields: "id") { id: ID! reviewCount: Int! }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn unsatisfiable_requirement() {
        // `weight` lives in `inventory`, which no lookup can enter.
        let errors = errors(&[
            ("products", PRODUCTS),
            (
                "shipping",
                r#"
                type Query { productById(id: ID!): Product @lookup @internal }
                type Product @key(fields: "id") {
                  id: ID!
                  cost(weight: Int @require(field: "weight")): Int
                }
                "#,
            ),
            (
                "inventory",
                r#"
                type Query { stock(id: ID!): Stock @lookup }
                type Stock @key(fields: "id") { id: ID! }
                type Product @key(fields: "id") { id: ID! weight: Int }
                "#,
            ),
        ]);
        let codes: Vec<&str> = errors.iter().map(|(c, _)| c.as_str()).collect();
        assert!(
            codes.iter().all(|c| *c == "SATISFIABILITY_ERROR"),
            "{errors:#?}"
        );
        assert!(
            errors
                .iter()
                .any(|(_, m)| m
                    .contains("cannot satisfy @require conditions on field \"Product.cost\"")),
            "{errors:#?}"
        );
    }

    #[test]
    fn satisfiable_requirement() {
        compose_sources(&[
            ("products", PRODUCTS),
            (
                "shipping",
                r#"
                type Query { productById(id: ID!): Product @lookup @internal }
                type Product @key(fields: "id") {
                  id: ID!
                  cost(weight: Int @require(field: "weight")): Int
                }
                "#,
            ),
            (
                "inventory",
                r#"
                type Query { productById(id: ID!): Product @lookup @internal }
                type Product @key(fields: "id") { id: ID! weight: Int }
                "#,
            ),
        ])
        .expect("composes");
    }

    #[test]
    fn provides_is_not_a_requirement_for_source_schemas() {
        // `Review.author.name` is only reachable through the `@provides`: `users` has no lookup.
        let errors = errors(&[
            (
                "reviews",
                r#"
                type Query { reviews: [Review!]! reviewById(id: ID!): Review @lookup }
                type Review @key(fields: "id") { id: ID! author: User! @provides(fields: "name") }
                type User @key(fields: "id") { id: ID! name: String! @external }
                "#,
            ),
            (
                "users",
                r#"
                type Query { thing(id: ID!): Thing @lookup }
                type Thing @key(fields: "id") { id: ID! }
                type User @key(fields: "id") { id: ID! name: String! @shareable }
                "#,
            ),
        ]);
        let codes: Vec<&str> = errors.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, ["SATISFIABILITY_ERROR"], "{errors:#?}");
        assert!(errors[0].1.contains("name"), "{errors:#?}");

        // Control: once `users` can recall `User`, the same graph composes.
        compose_sources(&[
            (
                "reviews",
                r#"
                type Query { reviews: [Review!]! reviewById(id: ID!): Review @lookup }
                type Review @key(fields: "id") { id: ID! author: User! @provides(fields: "name") }
                type User @key(fields: "id") { id: ID! name: String! @external }
                "#,
            ),
            (
                "users",
                r#"
                type Query { userById(id: ID!): User @lookup }
                type User @key(fields: "id") { id: ID! name: String! @shareable }
                "#,
            ),
        ])
        .expect("composes");
    }
}

mod mixed_dialect_detection {
    use super::*;

    #[test]
    fn unlinked_subgraph_without_lookups_is_a_source_schema_when_another_is() {
        // `reviews` has no GraphQL Federation directive. Read as Fed 1 its key would be resolvable
        // through `_entities`; as a source schema it has no lookup, so `reviewCount` is unreachable.
        let errors = errors(&[
            ("products", PRODUCTS),
            (
                "reviews",
                r#"
                type Query { reviewsVersion: Int }
                type Product @key(fields: "id") { id: ID! reviewCount: Int! }
                "#,
            ),
        ]);
        let codes: Vec<&str> = errors.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(codes, ["SATISFIABILITY_ERROR"], "{errors:#?}");
        assert!(errors[0].1.contains("reviewCount"), "{errors:#?}");
    }
}
