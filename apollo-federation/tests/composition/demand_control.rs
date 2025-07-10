use apollo_federation::composition::compose;
use apollo_federation::subgraph::typestate::{Initial, Subgraph};

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
    .into_fed2_subgraph()
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
    "#).unwrap().into_fed2_subgraph().unwrap()
}

fn subgraph_with_renamed_cost() -> Subgraph<Initial> {
    Subgraph::parse("subgraphWithCost", "", r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v0.1")
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

#[test]
fn composes_directives_imported_from_cost_spec() {
    let result = compose(vec![subgraph_with_cost(), subgraph_with_listsize()]).unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
}

#[test]
fn composes_directives_imported_from_cost_spec_renamed() {
    let result = compose(vec![
        subgraph_with_renamed_cost(),
        subgraph_with_renamed_listsize(),
    ])
    .unwrap();

    assert!(result.hints().is_empty());
    insta::assert_snapshot!(result.schema().schema());
}
