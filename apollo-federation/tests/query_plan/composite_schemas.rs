//! Query planning over GraphQL Federation source schemas (entity fetches through `@lookup`).

use apollo_federation::composition::CompositionOptions;
use apollo_federation::query_plan::QueryPlan;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::subgraph::typestate::Subgraph;

/// Compose source schemas with the Rust composition pipeline and build an incremental planner.
#[track_caller]
fn planner(sources: &[(&str, &str)]) -> QueryPlanner {
    planner_with(sources, true)
}

#[track_caller]
fn planner_with(sources: &[(&str, &str)], incremental: bool) -> QueryPlanner {
    let supergraph = supergraph_sdl(sources);
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&supergraph)
        .expect("valid supergraph");
    let mut config = QueryPlannerConfig::default();
    config.incremental_planner.enabled = incremental;
    config.incremental_delivery.enable_defer = true;
    QueryPlanner::new(&supergraph, config).expect("query planner")
}

#[track_caller]
fn supergraph_sdl(sources: &[(&str, &str)]) -> String {
    let subgraphs = sources
        .iter()
        .map(|(name, sdl)| Subgraph::parse(name, &format!("http://{name}"), sdl).expect("parses"))
        .collect();
    let supergraph =
        apollo_federation::composition::compose(subgraphs, CompositionOptions::default())
            .unwrap_or_else(|e| panic!("composition failed: {e:#?}"));
    supergraph.schema().schema().to_string()
}

/// Plan without the correctness check, for plans the checker cannot interpret yet (e.g. input
/// `KeyRenamer` rewrites of aliased requirements, which it rejects for `_entities` fetches too).
#[track_caller]
fn plan_unchecked(planner: &QueryPlanner, operation: &str) -> QueryPlan {
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        operation,
        "operation.graphql",
    )
    .expect("valid operation");
    planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan")
}

#[track_caller]
fn plan(planner: &QueryPlanner, operation: &str) -> QueryPlan {
    let document = apollo_compiler::ExecutableDocument::parse_and_validate(
        planner.api_schema().schema(),
        operation,
        "operation.graphql",
    )
    .expect("valid operation");
    let plan = planner
        .build_query_plan(&document, None, Default::default())
        .expect("query plan");
    if let Err(error) = apollo_federation::correctness::check_plan(
        planner.api_schema(),
        planner.supergraph_schema(),
        planner.subgraph_schemas(),
        &document,
        &plan,
    ) {
        panic!("plan failed the correctness check: {error}\n{plan}");
    }
    plan
}

const PRODUCTS: &str = r#"
    type Query {
      topProducts: [Product!]!
      productById(id: ID!): Product @lookup
    }
    type Product @key(fields: "id") {
      id: ID!
      name: String!
      dimension: Dimension!
    }
    type Dimension { size: Int! weight: Int! }
"#;

const REVIEWS: &str = r#"
    type Query {
      productById(id: ID!): Product @lookup @internal
    }
    type Product @key(fields: "id") {
      id: ID!
      reviewCount: Int!
    }
"#;

#[test]
fn entity_fetch_calls_the_lookup() {
    let planner = planner(&[("products", PRODUCTS), ("reviews", REVIEWS)]);
    let plan = plan(&planner, "{ topProducts { name reviewCount } }");
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "products") {
          {
            topProducts {
              __typename
              name
              id
            }
          }
        },
        Flatten(path: "topProducts.@") {
          Fetch(service: "reviews", lookup: "productById") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            $lookupArgument_0 = id
            {
              productById(id: $lookupArgument_0) {
                reviewCount
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn legacy_planner_is_refused() {
    let supergraph = supergraph_sdl(&[("products", PRODUCTS), ("reviews", REVIEWS)]);
    let supergraph = apollo_federation::Supergraph::new_with_router_specs(&supergraph).unwrap();
    let error = QueryPlanner::new(&supergraph, QueryPlannerConfig::default())
        .err()
        .expect("the legacy planner cannot plan lookups");
    assert!(
        error.to_string().contains("incremental query planner"),
        "{error}"
    );
}

#[test]
fn nested_and_renamed_lookup_arguments() {
    let planner = planner(&[
        ("products", PRODUCTS),
        (
            "reviews",
            r#"
            type Query { lookups: InternalLookups! @internal }
            type InternalLookups @internal {
              product(productId: ID! @is(field: "id")): Product @lookup
            }
            type Product @key(fields: "id") { id: ID! reviewCount: Int! }
            "#,
        ),
    ]);
    let plan = plan(&planner, "{ topProducts { name reviewCount } }");
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "products") {
          {
            topProducts {
              __typename
              name
              id
            }
          }
        },
        Flatten(path: "topProducts.@") {
          Fetch(service: "reviews", lookup: "lookups.product") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            $lookupArgument_0 = id
            {
              lookups {
                product(productId: $lookupArgument_0) {
                  reviewCount
                }
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn composite_key_and_object_argument() {
    let planner = planner(&[
        (
            "products",
            r#"
            type Query { topProducts: [Product!]! product(id: ID!, region: String!): Product @lookup }
            type Product @key(fields: "id region") { id: ID! region: String! name: String! }
            "#,
        ),
        (
            "prices",
            r#"
            type Query {
              price(key: PriceKey! @is(field: "{ productId: id, region }")): Product @lookup @internal
            }
            input PriceKey { productId: ID! region: String! }
            type Product @key(fields: "id region") { id: ID! region: String! price: Int! }
            "#,
        ),
    ]);
    let plan = plan(&planner, "{ topProducts { name price } }");
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "products") {
          {
            topProducts {
              __typename
              name
              id
              region
            }
          }
        },
        Flatten(path: "topProducts.@") {
          Fetch(service: "prices", lookup: "price") {
            {
              ... on Product {
                __typename
                id
                region
              }
            } =>
            $lookupArgument_0 = { productId: id, region: region }
            {
              price(key: $lookupArgument_0) {
                price
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn requirement_is_passed_as_a_variable() {
    let planner = planner(&[
        ("products", PRODUCTS),
        (
            "shipping",
            r#"
            type Query { productById(id: ID!): Product @lookup @internal }
            type Product @key(fields: "id") {
              id: ID!
              estimate(zip: String!, weight: Int! @require(field: "dimension.weight")): Int
            }
            "#,
        ),
    ]);
    let plan = plan(
        &planner,
        r#"{ topProducts { name estimate(zip: "94110") } }"#,
    );
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "products") {
          {
            topProducts {
              __typename
              name
              id
              dimension {
                weight
              }
            }
          }
        },
        Flatten(path: "topProducts.@") {
          Fetch(service: "shipping", lookup: "productById") {
            {
              ... on Product {
                __typename
                id
                dimension {
                  weight
                }
              }
            } =>
            $lookupArgument_0 = id
            $requireArgument_0 = dimension.weight
            {
              productById(id: $lookupArgument_0) {
                estimate(weight: $requireArgument_0, zip: "94110")
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn abstract_return_and_one_of_lookup() {
    let planner = planner(&[
        (
            "catalog",
            r#"
            type Query { featured: [Media!]! }
            union Media = Book | Movie
            type Book @key(fields: "isbn") { isbn: String! title: String! }
            type Movie @key(fields: "upc") { upc: String! title: String! }
            "#,
        ),
        (
            "ratings",
            r#"
            type Query {
              mediaByKey(
                key: MediaKeyInput! @is(field: "{ isbn: <Book>.isbn } | { upc: <Movie>.upc }")
              ): Media @lookup @internal
            }
            input MediaKeyInput @oneOf { isbn: String upc: String }
            union Media = Book | Movie
            type Book @key(fields: "isbn") { isbn: String! rating: Int! }
            type Movie @key(fields: "upc") { upc: String! rating: Int! }
            "#,
        ),
    ]);
    let plan = plan(
        &planner,
        "{ featured { ... on Book { title rating } ... on Movie { title rating } } }",
    );
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "catalog") {
          {
            featured {
              __typename
              ... on Book {
                __typename
                title
                isbn
              }
              ... on Movie {
                __typename
                title
                upc
              }
            }
          }
        },
        Flatten(path: "featured.@") {
          Fetch(service: "ratings", lookup: "mediaByKey") {
            {
              ... on Book {
                __typename
                isbn
              }
              ... on Movie {
                __typename
                upc
              }
            } =>
            $lookupArgument_0 = <Book> { isbn: <Book> isbn } | <Movie> { upc: <Movie> upc }
            {
              mediaByKey(key: $lookupArgument_0) {
                ... on Book {
                  rating
                }
                ... on Movie {
                  rating
                }
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn different_lookups_at_the_same_path_run_in_parallel() {
    let planner = planner(&[
        (
            "catalog",
            r#"
            type Query { featured: [Media!]! }
            union Media = Book | Movie
            type Book @key(fields: "isbn") { isbn: String! title: String! }
            type Movie @key(fields: "upc") { upc: String! title: String! }
            "#,
        ),
        (
            "ratings",
            r#"
            type Query {
              bookByIsbn(isbn: String!): Book @lookup @internal
              movieByUpc(upc: String!): Movie @lookup @internal
            }
            type Book @key(fields: "isbn") { isbn: String! rating: Int! }
            type Movie @key(fields: "upc") { upc: String! rating: Int! }
            "#,
        ),
    ]);
    let plan = plan(
        &planner,
        "{ featured { ... on Book { rating } ... on Movie { rating } } }",
    );
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "catalog") {
          {
            featured {
              __typename
              ... on Book {
                __typename
                isbn
              }
              ... on Movie {
                __typename
                upc
              }
            }
          }
        },
        Parallel {
          Flatten(path: "featured.@") {
            Fetch(service: "ratings", lookup: "bookByIsbn") {
              {
                ... on Book {
                  __typename
                  isbn
                }
              } =>
              $lookupArgument_0 = isbn
              {
                bookByIsbn(isbn: $lookupArgument_0) {
                  rating
                }
              }
            },
          },
          Flatten(path: "featured.@") {
            Fetch(service: "ratings", lookup: "movieByUpc") {
              {
                ... on Movie {
                  __typename
                  upc
                }
              } =>
              $lookupArgument_0 = upc
              {
                movieByUpc(upc: $lookupArgument_0) {
                  rating
                }
              }
            },
          },
        },
      },
    }
    "#);
}

#[test]
fn chained_hops_through_source_schemas() {
    // `inventory` recalls products by `sku` only; `products` provides the `sku` from `id`.
    let planner = planner(&[
        (
            "catalog",
            r#"
            type Query { featured: [Product!]! }
            type Product @key(fields: "id") { id: ID! }
            "#,
        ),
        (
            "products",
            r#"
            type Query { productById(id: ID!): Product @lookup @internal }
            type Product @key(fields: "id") @key(fields: "sku") { id: ID! sku: String! }
            "#,
        ),
        (
            "inventory",
            r#"
            type Query { productBySku(sku: String!): Product @lookup @internal }
            type Product @key(fields: "sku") { sku: String! stock: Int! }
            "#,
        ),
    ]);
    let plan = plan(&planner, "{ featured { stock } }");
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "catalog") {
          {
            featured {
              __typename
              id
            }
          }
        },
        Flatten(path: "featured.@") {
          Fetch(service: "products", lookup: "productById") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            $lookupArgument_0 = id
            {
              productById(id: $lookupArgument_0) {
                sku
              }
            }
          },
        },
        Flatten(path: "featured.@") {
          Fetch(service: "inventory", lookup: "productBySku") {
            {
              ... on Product {
                __typename
                sku
              }
            } =>
            $lookupArgument_0 = sku
            {
              productBySku(sku: $lookupArgument_0) {
                stock
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn requirement_reenters_the_same_source_schema() {
    // `shipping` owns the root field and the estimate, but the estimate requires the weight,
    // which only `products` has: fetch it, then re-enter `shipping` through its lookup.
    let planner = planner(&[
        (
            "shipping",
            r#"
            type Query {
              deliveries: [Product!]!
              productById(id: ID!): Product @lookup @internal
            }
            type Product @key(fields: "id") {
              id: ID!
              estimate(weight: Int! @require(field: "weight")): Int
            }
            "#,
        ),
        (
            "products",
            r#"
            type Query { productById(id: ID!): Product @lookup @internal }
            type Product @key(fields: "id") { id: ID! weight: Int! }
            "#,
        ),
    ]);
    // The requirement is aliased to avoid a response-path collision and renamed back by an input
    // rewrite, which the correctness checker does not support (see `plan_unchecked`).
    let plan = plan_unchecked(&planner, "{ deliveries { estimate } }");
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "shipping") {
          {
            deliveries {
              __typename
              id
            }
          }
        },
        Flatten(path: "deliveries.@") {
          Fetch(service: "products", lookup: "productById") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            $lookupArgument_0 = id
            {
              productById(id: $lookupArgument_0) {
                __require_0_weight: weight
              }
            }
          },
        },
        Flatten(path: "deliveries.@") {
          Fetch(service: "shipping", lookup: "productById") {
            {
              ... on Product {
                __typename
                id
                __require_0_weight: weight
              }
            } =>
            $lookupArgument_0 = id
            $requireArgument_0 = weight
            {
              productById(id: $lookupArgument_0) {
                estimate(weight: $requireArgument_0)
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn mutation_result_entities_are_looked_up() {
    let planner = planner(&[
        (
            "products",
            r#"
            type Query { productById(id: ID!): Product @lookup }
            type Mutation { createProduct(name: String!): Product! }
            type Product @key(fields: "id") { id: ID! name: String! }
            "#,
        ),
        ("reviews", REVIEWS),
    ]);
    let plan = plan(
        &planner,
        r#"mutation { createProduct(name: "x") { name reviewCount } }"#,
    );
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Sequence {
        Fetch(service: "products") {
          {
            createProduct(name: "x") {
              __typename
              name
              id
            }
          }
        },
        Flatten(path: "createProduct") {
          Fetch(service: "reviews", lookup: "productById") {
            {
              ... on Product {
                __typename
                id
              }
            } =>
            $lookupArgument_0 = id
            {
              productById(id: $lookupArgument_0) {
                reviewCount
              }
            }
          },
        },
      },
    }
    "#);
}

#[test]
fn deferred_lookup_fetch() {
    let planner = planner(&[("products", PRODUCTS), ("reviews", REVIEWS)]);
    let plan = plan(
        &planner,
        "{ topProducts { name ... @defer { reviewCount } } }",
    );
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Defer {
        Primary {
          { topProducts { name } }:
          Fetch(service: "products", id: 0) {
            {
              topProducts {
                __typename
                name
                id
              }
            }
          },
        }, [
          Deferred(depends: [0], path: "topProducts") {
            { reviewCount }:
            Flatten(path: "topProducts.@") {
              Fetch(service: "reviews", lookup: "productById") {
                {
                  ... on Product {
                    __typename
                    id
                  }
                } =>
                $lookupArgument_0 = id
                {
                  productById(id: $lookupArgument_0) {
                    reviewCount
                  }
                }
              },
            },
          },
        ]
      },
    }
    "#);
}

#[test]
fn deferred_block_depends_on_every_fetch_of_a_split_lookup_group() {
    let planner = planner(&[
        ("catalog", MEDIA_CATALOG),
        ("ratings", MEDIA_RATINGS),
        ("reviews", RATING_REVIEWS),
    ]);
    let plan = plan(
        &planner,
        "{ featured { rating { stars ... @defer { text } } } }",
    );
    insta::assert_snapshot!(plan, @r#"
    QueryPlan {
      Defer {
        Primary {
          { featured { rating { stars } } }:
          Sequence {
            Fetch(service: "catalog") {
              {
                featured {
                  __typename
                  ... on Book {
                    __typename
                    isbn
                  }
                  ... on Movie {
                    __typename
                    upc
                  }
                }
              }
            },
            Parallel {
              Flatten(path: "featured.@") {
                Fetch(service: "ratings", id: 0, lookup: "bookByIsbn") {
                  {
                    ... on Book {
                      __typename
                      isbn
                    }
                  } =>
                  $lookupArgument_0 = isbn
                  {
                    bookByIsbn(isbn: $lookupArgument_0) {
                      rating {
                        __typename
                        stars
                        id
                      }
                    }
                  }
                },
              },
              Flatten(path: "featured.@") {
                Fetch(service: "ratings", id: 1, lookup: "movieByUpc") {
                  {
                    ... on Movie {
                      __typename
                      upc
                    }
                  } =>
                  $lookupArgument_0 = upc
                  {
                    movieByUpc(upc: $lookupArgument_0) {
                      rating {
                        __typename
                        stars
                        id
                      }
                    }
                  }
                },
              },
            },
          },
        }, [
          Deferred(depends: [0, 1], path: "featured/rating") {
            { text }:
            Flatten(path: "featured.@.rating") {
              Fetch(service: "reviews", lookup: "ratingById") {
                {
                  ... on Rating {
                    __typename
                    id
                  }
                } =>
                $lookupArgument_0 = id
                {
                  ratingById(id: $lookupArgument_0) {
                    text
                  }
                }
              },
            },
          },
        ]
      },
    }
    "#);
}

const MEDIA_CATALOG: &str = r#"
    type Query { featured: [Media!]! }
    interface Media { id: ID! }
    type Book implements Media @key(fields: "isbn") { id: ID! @shareable isbn: String! }
    type Movie implements Media @key(fields: "upc") { id: ID! @shareable upc: String! }
"#;

const MEDIA_RATINGS: &str = r#"
    type Query {
      bookByIsbn(isbn: String!): Book @lookup @internal
      movieByUpc(upc: String!): Movie @lookup @internal
    }
    interface Media { rating: Rating! }
    type Book implements Media @key(fields: "isbn") { isbn: String! rating: Rating! }
    type Movie implements Media @key(fields: "upc") { upc: String! rating: Rating! }
    type Rating @key(fields: "id") { id: ID! stars: Int! }
"#;

const RATING_REVIEWS: &str = r#"
    type Query { ratingById(id: ID!): Rating @lookup @internal }
    type Rating @key(fields: "id") { id: ID! text: String! }
"#;
