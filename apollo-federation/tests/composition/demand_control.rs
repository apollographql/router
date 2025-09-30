use apollo_federation::composition::compose;
use apollo_federation::error::ErrorCode;
use apollo_federation::subgraph::typestate::Initial;
use apollo_federation::subgraph::typestate::Subgraph;

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
    "#).unwrap().into_fed2_test_subgraph(false, false).unwrap()
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
    "#).unwrap().into_fed2_test_subgraph(false, false).unwrap()
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
    .into_fed2_test_subgraph(true, false)
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
    "#).unwrap().into_fed2_test_subgraph(true, false).unwrap()
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
fn composes_directives_imported_from_cost_spec() {
    let result = compose(vec![subgraph_with_cost(), subgraph_with_listsize()]).unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn composes_directives_imported_from_federation_spec() {
    let result = compose(vec![
        subgraph_with_cost_from_federation_spec(),
        subgraph_with_listsize_from_federation_spec(),
    ])
    .unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn composes_renamed_directives_imported_from_cost_spec() {
    let result = compose(vec![
        subgraph_with_renamed_cost(),
        subgraph_with_renamed_listsize(),
    ])
    .unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn composes_renamed_directives_imported_from_federation_spec() {
    let result = compose(vec![
        subgraph_with_renamed_cost_from_federation_spec(),
        subgraph_with_renamed_listsize_from_federation_spec(),
    ])
    .unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
}

#[ignore = "until merge implementation completed"]
#[test]
fn composes_fully_qualified_directive_names() {
    let result = compose(vec![
        subgraph_with_unimported_cost(),
        subgraph_with_unimported_listsize(),
    ])
    .unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
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
    let errors = compose(vec![
        subgraph_with_default_name,
        subgraph_with_different_name,
    ])
    .unwrap_err();

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
    let result = compose(vec![subgraph_a, subgraph_b]).unwrap();

    assert_eq!(result.hints().len(), 1);
    let hint = result.hints().first().unwrap();
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
    let result = compose(vec![subgraph_a, subgraph_b]).unwrap();

    assert_eq!(result.hints().len(), 1);
    let hint = result.hints().first().unwrap();
    assert_eq!(hint.code(), "MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS");
    assert_eq!(
        hint.message(),
        r#"Directive @listSize is applied to "Query.sharedWithListSize" in multiple subgraphs with different arguments. Merging strategies used by arguments: { "assumedSize": NULLABLE_MAX, "slicingArguments": NULLABLE_UNION, "sizedFields": NULLABLE_UNION, "requireOneSlicingArgument": NULLABLE_AND }"#
    )
}
