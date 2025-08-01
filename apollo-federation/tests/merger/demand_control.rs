use apollo_federation::composition::expand_subgraphs;
use apollo_federation::composition::merge_subgraphs;
use apollo_federation::composition::upgrade_subgraphs_if_necessary;
use apollo_federation::composition::validate_subgraphs;
use apollo_federation::error::ErrorCode;
use apollo_federation::merger::merger::Merger;
use apollo_federation::subgraph::typestate::Initial;
use apollo_federation::subgraph::typestate::Subgraph;

// Helper function to create subgraphs and process them through the full pipeline
fn merge_subgraphs_with_merger(
    subgraphs: Vec<Subgraph<Initial>>,
) -> Result<apollo_federation::merger::merger::MergeResult, Vec<apollo_federation::error::CompositionError>> {
    let expanded_subgraphs = expand_subgraphs(subgraphs)?;
    let upgraded_subgraphs = upgrade_subgraphs_if_necessary(expanded_subgraphs)?;
    let validated_subgraphs = validate_subgraphs(upgraded_subgraphs)?;

    let merger = Merger::new(validated_subgraphs, Default::default()).map_err(|e| {
        vec![apollo_federation::error::CompositionError::InternalError {
            message: e.to_string(),
        }]
    })?;
    Ok(merger.merge())
}

fn subgraph_with_cost() -> Subgraph<Initial> {
    Subgraph::parse(
        "subgraphWithCost",
        "",
        r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])

    enum AorB @cost(weight: 15) {
      A
      B
    }

    input InputTypeWithCost {
      somethingWithCost: Int @cost(weight: 20)
    }

    scalar ExpensiveInt @cost(weight: 30)

    type ExpensiveObject @cost(weight: 40) {
      id: ID
    }

    type Query {
      fieldWithCost: Int @cost(weight: 5)
      argWithCost(arg: Int @cost(weight: 10)): Int
      enumWithCost: AorB
      inputWithCost(someInput: InputTypeWithCost): Int
      scalarWithCost: ExpensiveInt
      objectWithCost: ExpensiveObject
    }
"#,
    )
    .unwrap()
}

fn subgraph_with_listsize() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithListSize", "", r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

    type HasInts {
      ints: [Int!]
    }

    type Query {
      fieldWithListSize: [String!] @listSize(assumedSize: 2000, requireOneSlicingArgument: false)
      fieldWithDynamicListSize(first: Int!): HasInts @listSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
    }
    "#).unwrap()
}

fn subgraph_with_renamed_cost() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithCost", "", r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/cost/v0.1", import: [{ name: "@cost", as: "@renamedCost" }])

    enum AorB @renamedCost(weight: 15) {
      A
      B
    }

    input InputTypeWithCost {
      somethingWithCost: Int @renamedCost(weight: 20)
    }

    scalar ExpensiveInt @renamedCost(weight: 30)

    type ExpensiveObject @renamedCost(weight: 40) {
      id: ID
    }

    type Query {
      fieldWithCost: Int @renamedCost(weight: 5)
      argWithCost(arg: Int @renamedCost(weight: 10)): Int
      enumWithCost: AorB
      inputWithCost(someInput: InputTypeWithCost): Int
      scalarWithCost: ExpensiveInt
      objectWithCost: ExpensiveObject
    }
    "#).unwrap().into_fed2_subgraph().unwrap()
}

fn subgraph_with_renamed_listsize() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithListSize", "", r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/cost/v0.1", import: [{ name: "@listSize", as: "@renamedListSize" }])

    type HasInts {
      ints: [Int!] @shareable
    }

    type Query {
      fieldWithListSize: [String!] @renamedListSize(assumedSize: 2000, requireOneSlicingArgument: false)
      fieldWithDynamicListSize(first: Int!): HasInts @renamedListSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
    }
    "#).unwrap().into_fed2_subgraph().unwrap()
}

fn subgraph_with_cost_from_federation_spec() -> Subgraph<Initial> {
    Subgraph::parse(
        "subgraphWithCost",
        "",
        r#"
    enum AorB @cost(weight: 15) {
      A
      B
    }
    
    input InputTypeWithCost {
      somethingWithCost: Int @cost(weight: 20)
    }

    scalar ExpensiveInt @cost(weight: 30)

    type ExpensiveObject @cost(weight: 40) {
      id: ID
    }

    type Query {
      fieldWithCost: Int @cost(weight: 5)
      argWithCost(arg: Int @cost(weight: 10)): Int
      enumWithCost: AorB
      inputWithCost(someInput: InputTypeWithCost): Int
      scalarWithCost: ExpensiveInt
      objectWithCost: ExpensiveObject
    }
    "#,
    )
    .unwrap()
    .into_fed2_test_subgraph(true)
    .unwrap()
}

fn subgraph_with_listsize_from_federation_spec() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithListSize", "", r#"
    type HasInts {
      ints: [Int!]
    }

    type Query {
      fieldWithListSize: [String!] @listSize(assumedSize: 2000, requireOneSlicingArgument: false)
      fieldWithDynamicListSize(first: Int!): HasInts @listSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
    }
    "#).unwrap().into_fed2_test_subgraph(true).unwrap()
}

fn subgraph_with_renamed_cost_from_federation_spec() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithCost", "", r#"
      extend schema @link(url: "https://specs.apollo.dev/federation/v2.9", import: [{ name: "@cost", as: "@renamedCost" }])

      enum AorB @renamedCost(weight: 15) {
        A
        B
      }

      input InputTypeWithCost {
        somethingWithCost: Int @renamedCost(weight: 20)
      }

      scalar ExpensiveInt @renamedCost(weight: 30)

      type ExpensiveObject @renamedCost(weight: 40) {
        id: ID
      }

      type Query {
        fieldWithCost: Int @renamedCost(weight: 5)
        argWithCost(arg: Int @renamedCost(weight: 10)): Int
        enumWithCost: AorB
        inputWithCost(someInput: InputTypeWithCost): Int
        scalarWithCost: ExpensiveInt
        objectWithCost: ExpensiveObject
      }
    "#).unwrap()
}

fn subgraph_with_renamed_listsize_from_federation_spec() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithListSize", "", r#"
      extend schema @link(url: "https://specs.apollo.dev/federation/v2.9", import: [{ name: "@listSize", as: "@renamedListSize" }])

      type HasInts {
        ints: [Int!]
      }

      type Query {
        fieldWithListSize: [String!] @renamedListSize(assumedSize: 2000, requireOneSlicingArgument: false)
        fieldWithDynamicListSize(first: Int!): HasInts @renamedListSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
      }
    "#).unwrap()
}

fn subgraph_with_unimported_cost() -> Subgraph<Initial> {
    Subgraph::parse(
        "subgraphWithCost",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/federation/v2.9")

        enum AorB @federation__cost(weight: 15) {
            A
            B
        }

        input InputTypeWithCost {
            somethingWithCost: Int @federation__cost(weight: 20)
        }

        scalar ExpensiveInt @federation__cost(weight: 30)

        type ExpensiveObject @federation__cost(weight: 40) {
            id: ID
        }

        type Query {
            fieldWithCost: Int @federation__cost(weight: 5)
            argWithCost(arg: Int @federation__cost(weight: 10)): Int
            enumWithCost: AorB
            inputWithCost(someInput: InputTypeWithCost): Int
            scalarWithCost: ExpensiveInt
            objectWithCost: ExpensiveObject
        }
    "#,
    )
    .unwrap()
}

fn subgraph_with_unimported_listsize() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithListSize", "", r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/federation/v2.9")

        type HasInts {
            ints: [Int!]
        }

        type Query {
            fieldWithListSize: [String!] @federation__listSize(assumedSize: 2000, requireOneSlicingArgument: false)
            fieldWithDynamicListSize(first: Int!): HasInts @federation__listSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
        }
    "#).unwrap()
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_directives_imported_from_cost_spec() {
    let result = merge_subgraphs_with_merger(vec![subgraph_with_cost(), subgraph_with_listsize()]).unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_directives_imported_from_federation_spec() {
    let result = merge_subgraphs_with_merger(vec![
        subgraph_with_cost_from_federation_spec(),
        subgraph_with_listsize_from_federation_spec(),
    ])
    .unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_renamed_directives_imported_from_cost_spec() {
    let result = merge_subgraphs_with_merger(vec![
        subgraph_with_renamed_cost(),
        subgraph_with_renamed_listsize(),
    ])
    .unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_renamed_directives_imported_from_federation_spec() {
    let result = merge_subgraphs_with_merger(vec![
        subgraph_with_renamed_cost_from_federation_spec(),
        subgraph_with_renamed_listsize_from_federation_spec(),
    ])
    .unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_fully_qualified_directive_names() {
    let result = merge_subgraphs_with_merger(vec![
        subgraph_with_unimported_cost(),
        subgraph_with_unimported_listsize(),
    ])
    .unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn errors_when_subgraphs_use_different_names() {
    let subgraph_with_default_name = Subgraph::parse(
        "subgraphWithDefaultName",
        "",
        r#"
        extend schema 
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])
    
        type Query {
            field1: Int @cost(weight: 5)
        }
    "#,
    )
    .unwrap();
    let subgraph_with_different_name = Subgraph::parse("subgraphWithDifferentName", "", r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: [{ name: "@cost", as: "@renamedCost" }])
    
        type Query {
            field2: Int @renamedCost(weight: 10)
        }
    "#).unwrap();
    let result = merge_subgraphs_with_merger(vec![
        subgraph_with_default_name,
        subgraph_with_different_name,
    ]);

    assert!(result.is_err());
    let errors = result.unwrap_err();
    assert_eq!(errors.len(), 1);
    let error = errors.first().unwrap();
    assert_eq!(error.code(), ErrorCode::LinkImportNameMismatch);
    assert_eq!(
        error.to_string(),
        r#"The "@cost" directive (from https://specs.apollo.dev/cost/v0.1) is imported with mismatched name between subgraphs: it is imported as "@renamedCost" in subgraph "subgraphWithDifferentName" but "@cost" in subgraph "subgraphWithDefaultName""#
    )
}

#[ignore = "until merge implementation completed"]
#[test]
fn hints_when_merging_cost_arguments() {
    let subgraph_a = Subgraph::parse(
        "subgraph-a",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])

        type Query {
            sharedWithCost: Int @shareable @cost(weight: 5)
        }
    "#,
    )
    .unwrap();
    let subgraph_b = Subgraph::parse(
        "subgraph-b",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])

        type Query {
            sharedWithCost: Int @shareable @cost(weight: 10)
        }
    "#,
    )
    .unwrap();
    let result = merge_subgraphs_with_merger(vec![subgraph_a, subgraph_b]).unwrap();

    assert_eq!(result.hints.len(), 1);
    let hint = result.hints.first().unwrap();
    assert_eq!(hint.code(), "MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS");
    assert_eq!(
        hint.message(),
        r#"Directive @cost is applied to "Query.sharedWithCost" in multiple subgraphs with different arguments. Merging strategies used by arguments: { "weight": MAX }""#
    );
}

#[ignore = "until merge implementation completed"]
#[test]
fn hints_when_merging_listsize_arguments() {
    let subgraph_a = Subgraph::parse(
        "subgraph-a",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])
    
        type Query {
            sharedWithListSize: [Int] @shareable @listSize(assumedSize: 10)
        }
    "#,
    )
    .unwrap();
    let subgraph_b = Subgraph::parse(
        "subgraph-b",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

        type Query {
            sharedWithListSize: [Int] @shareable @listSize(assumedSize: 20)
        }
    "#,
    )
    .unwrap();
    let result = merge_subgraphs_with_merger(vec![subgraph_a, subgraph_b]).unwrap();

    assert_eq!(result.hints.len(), 1);
    let hint = result.hints.first().unwrap();
    assert_eq!(hint.code(), "MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS");
    assert_eq!(
        hint.message(),
        r#"Directive @listSize is applied to "Query.sharedWithListSize" in multiple subgraphs with different arguments. Merging strategies used by arguments: { "assumedSize": NULLABLE_MAX, "slicingArguments": NULLABLE_UNION, "sizedFields": NULLABLE_UNION, "requireOneSlicingArgument": NULLABLE_AND }"#
    )
}

// Additional tests for comprehensive directive location coverage

#[ignore = "until merge implementation completed"]
#[test]
fn merges_cost_directive_on_all_locations() {
    let subgraph = Subgraph::parse(
        "subgraphWithCostOnAllLocations",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])

        enum Status @cost(weight: 5) {
            ACTIVE
            INACTIVE
        }

        input UserInput {
            name: String @cost(weight: 10)
        }

        scalar UserId @cost(weight: 15)

        type User @cost(weight: 20) {
            id: UserId
            name: String @cost(weight: 25)
            status(filter: Status @cost(weight: 30)): Status
        }

        type Query {
            user(input: UserInput): User
        }
    "#,
    )
    .unwrap();

    let result = merge_subgraphs_with_merger(vec![subgraph]).unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_listsize_directive_with_various_configurations() {
    let subgraph = Subgraph::parse(
        "subgraphWithListSizeVariations",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])

        type User {
            posts: [Post!] @listSize(assumedSize: 100)
        }

        type Post {
            comments: [Comment!] @listSize(slicingArguments: ["first", "last"], requireOneSlicingArgument: true)
            tags: [String!] @listSize(sizedFields: ["tagCount"])
            tagCount: Int
        }

        type Comment {
            id: ID
        }

        type Query {
            users(first: Int, last: Int): [User!] @listSize(slicingArguments: ["first", "last"], assumedSize: 50, requireOneSlicingArgument: false)
        }
    "#,
    )
    .unwrap();

    let result = merge_subgraphs_with_merger(vec![subgraph]).unwrap();

    assert!(result.hints.is_empty());
    assert!(result.supergraph.is_some());
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn handles_custom_directives_with_same_names() {
    let subgraph_with_custom_cost = Subgraph::parse(
        "subgraphWithCustomCost",
        "",
        r#"
        # Custom directive with same name but different definition
        directive @cost(multiplier: Float!) on FIELD_DEFINITION

        type Query {
            expensiveField: String @cost(multiplier: 2.5)
        }
    "#,
    )
    .unwrap();

    let subgraph_with_federation_cost = Subgraph::parse(
        "subgraphWithFederationCost",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost"])

        type Query {
            cheapField: String @cost(weight: 1)
        }
    "#,
    )
    .unwrap();

    let result = merge_subgraphs_with_merger(vec![
        subgraph_with_custom_cost,
        subgraph_with_federation_cost,
    ]);

    // This should either succeed with proper directive handling or fail with clear error
    // The exact behavior depends on the merger implementation
    match result {
        Ok(merge_result) => {
            assert!(merge_result.supergraph.is_some());
            let schema = merge_result.supergraph.unwrap();
            insta::assert_snapshot!(schema.schema());
        }
        Err(errors) => {
            // Verify we get appropriate error messages for directive conflicts
            assert!(!errors.is_empty());
            insta::assert_debug_snapshot!(errors);
        }
    }
}

#[ignore = "until merge implementation completed"]
#[test]
fn merges_demand_control_directives_on_shareable_fields() {
    let subgraph_a = Subgraph::parse(
        "subgraph-a",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost", "@listSize"])

        type Product {
            id: ID! @shareable
            name: String @shareable @cost(weight: 5)
            reviews: [Review!] @shareable @listSize(assumedSize: 10)
        }

        type Review {
            id: ID!
            rating: Int
        }

        type Query {
            product(id: ID!): Product
        }
    "#,
    )
    .unwrap();

    let subgraph_b = Subgraph::parse(
        "subgraph-b",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@cost", "@listSize"])

        type Product {
            id: ID! @shareable
            name: String @shareable @cost(weight: 8)
            reviews: [Review!] @shareable @listSize(assumedSize: 15)
            price: Float @cost(weight: 3)
        }

        type Review {
            id: ID!
            content: String
        }

        type Query {
            products: [Product!] @listSize(assumedSize: 100)
        }
    "#,
    )
    .unwrap();

    let result = merge_subgraphs_with_merger(vec![subgraph_a, subgraph_b]).unwrap();

    // Should have hints about merged directive arguments
    assert!(!result.hints.is_empty());
    assert!(result.supergraph.is_some());
    
    // Verify hints mention the merging strategies
    let cost_hint = result.hints.iter().find(|h| h.message().contains("@cost"));
    let listsize_hint = result.hints.iter().find(|h| h.message().contains("@listSize"));
    
    assert!(cost_hint.is_some());
    assert!(listsize_hint.is_some());
    
    let schema = result.supergraph.unwrap();
    insta::assert_snapshot!(schema.schema());
}