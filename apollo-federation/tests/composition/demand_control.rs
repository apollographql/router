use apollo_compiler::coord;
use apollo_compiler::Name;
use apollo_compiler::schema::ExtendedType;
use apollo_federation::composition::compose;
use apollo_federation::composition::Supergraph;
use apollo_federation::error::ErrorCode;
use apollo_federation::subgraph::typestate::Initial;
use apollo_federation::subgraph::typestate::Subgraph;
use apollo_federation::supergraph::Satisfiable;
use test_log::test;

// Helper function to check directive applications on various schema elements
fn check_cost_and_listsize_directives(result: &Supergraph<Satisfiable>, cost_name: &str, listsize_name: &str) {
    let schema = result.schema().schema();
    
    // Check directive definitions exist
    assert!(
        schema.directive_definitions.contains_key(&Name::new_unchecked(cost_name)),
        "Expected @{} directive definition in supergraph", cost_name
    );
    assert!(
        schema.directive_definitions.contains_key(&Name::new_unchecked(listsize_name)),
        "Expected @{} directive definition in supergraph", listsize_name
    );
    
    // Check @cost on FIELD_DEFINITION
    let field = coord!(Query.fieldWithCost).lookup_field(schema).unwrap();
    let cost_directive = field.directives.iter()
        .find(|d| d.name == cost_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on Query.fieldWithCost", cost_name));
    assert_eq!(cost_directive.to_string(), format!("@{}(weight: 5)", cost_name));
    
    // Check @cost on ARGUMENT_DEFINITION
    let field = coord!(Query.argWithCost).lookup_field(schema).unwrap();
    let arg = field.arguments.iter()
        .find(|a| a.name == "arg")
        .expect("Expected arg argument on Query.argWithCost");
    let cost_directive = arg.directives.iter()
        .find(|d| d.name == cost_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on Query.argWithCost.arg", cost_name));
    assert_eq!(cost_directive.to_string(), format!("@{}(weight: 10)", cost_name));
    
    // Check @cost on ENUM
    let enum_type = match coord!(AorB).lookup(schema).unwrap() {
        ExtendedType::Enum(e) => e,
        _ => panic!("Expected AorB to be an enum"),
    };
    let cost_directive = enum_type.directives.iter()
        .find(|d| d.name == cost_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on AorB enum", cost_name));
    assert_eq!(cost_directive.to_string(), format!("@{}(weight: 15)", cost_name));
    
    // Check @cost on INPUT_FIELD_DEFINITION
    let input_field = coord!(InputTypeWithCost.somethingWithCost).lookup_input_field(schema).unwrap();
    let cost_directive = input_field.directives.iter()
        .find(|d| d.name == cost_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on InputTypeWithCost.somethingWithCost", cost_name));
    assert_eq!(cost_directive.to_string(), format!("@{}(weight: 20)", cost_name));
    
    // Check @cost on SCALAR
    let scalar = match coord!(ExpensiveInt).lookup(schema).unwrap() {
        ExtendedType::Scalar(s) => s,
        _ => panic!("Expected ExpensiveInt to be a scalar"),
    };
    let cost_directive = scalar.directives.iter()
        .find(|d| d.name == cost_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on ExpensiveInt scalar", cost_name));
    assert_eq!(cost_directive.to_string(), format!("@{}(weight: 30)", cost_name));
    
    // Check @cost on OBJECT
    let object = match coord!(ExpensiveObject).lookup(schema).unwrap() {
        ExtendedType::Object(o) => o,
        _ => panic!("Expected ExpensiveObject to be an object"),
    };
    let cost_directive = object.directives.iter()
        .find(|d| d.name == cost_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on ExpensiveObject type", cost_name));
    assert_eq!(cost_directive.to_string(), format!("@{}(weight: 40)", cost_name));
    
    // Check @listSize with assumedSize
    let field = coord!(Query.fieldWithListSize).lookup_field(schema).unwrap();
    let listsize_directive = field.directives.iter()
        .find(|d| d.name == listsize_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on Query.fieldWithListSize", listsize_name));
    assert_eq!(listsize_directive.to_string(), format!("@{}(assumedSize: 2000, requireOneSlicingArgument: false)", listsize_name));
    
    // Check @listSize with slicingArguments and sizedFields
    let field = coord!(Query.fieldWithDynamicListSize).lookup_field(schema).unwrap();
    let listsize_directive = field.directives.iter()
        .find(|d| d.name == listsize_name)
        .unwrap_or_else(|| panic!("Expected @{} directive on Query.fieldWithDynamicListSize", listsize_name));
    assert_eq!(listsize_directive.to_string(), format!("@{}(slicingArguments: [\"first\"], sizedFields: [\"ints\"], requireOneSlicingArgument: true)", listsize_name));
}

fn subgraph_with_cost() -> Subgraph<Initial> {
    Subgraph::parse(
        "subgraphWithCost",
        "",
        r#"
    extend schema
        @link(url: "https://specs.apollo.dev/link/v1.0")
        @link(url: "https://specs.apollo.dev/federation/v2.9")
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
        @link(url: "https://specs.apollo.dev/federation/v2.9")
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
            @link(url: "https://specs.apollo.dev/cost/v0.1")

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
            @link(url: "https://specs.apollo.dev/cost/v0.1")

        type HasInts {
            ints: [Int!]
        }

        type Query {
            fieldWithListSize: [String!] @federation__listSize(assumedSize: 2000, requireOneSlicingArgument: false)
            fieldWithDynamicListSize(first: Int!): HasInts @federation__listSize(slicingArguments: ["first"], sizedFields: ["ints"], requireOneSlicingArgument: true)
        }
    "#).unwrap()
}

#[test]
fn composes_directives_imported_from_cost_spec() {
    let result = compose(vec![subgraph_with_cost(), subgraph_with_listsize()]).unwrap();
    assert!(result.hints().is_empty());
    check_cost_and_listsize_directives(&result, "cost", "listSize");
}

#[test]
fn composes_directives_imported_from_federation_spec() {
    let result = compose(vec![
        subgraph_with_cost_from_federation_spec(),
        subgraph_with_listsize_from_federation_spec(),
    ])
    .unwrap();
    assert!(result.hints().is_empty());
    check_cost_and_listsize_directives(&result, "cost", "listSize");
}

#[test]
fn composes_renamed_directives_imported_from_cost_spec() {
    let result = compose(vec![
        subgraph_with_renamed_cost(),
        subgraph_with_renamed_listsize(),
    ])
    .unwrap();
    
    // Allow hints about implicit federation version upgrades
    for hint in result.hints() {
        assert_eq!(hint.code(), "IMPLICITLY_UPGRADED_FEDERATION_VERSION");
    }
    
    check_cost_and_listsize_directives(&result, "renamedCost", "renamedListSize");
}

#[test]
fn composes_renamed_directives_imported_from_federation_spec() {
    let result = compose(vec![
        subgraph_with_renamed_cost_from_federation_spec(),
        subgraph_with_renamed_listsize_from_federation_spec(),
    ])
    .unwrap();
    assert!(result.hints().is_empty());
    check_cost_and_listsize_directives(&result, "renamedCost", "renamedListSize");
}

#[test]
fn composes_fully_qualified_directive_names() {
    let result = compose(vec![
        subgraph_with_unimported_cost(),
        subgraph_with_unimported_listsize(),
    ])
    .unwrap();
    assert!(result.hints().is_empty());
    check_cost_and_listsize_directives(&result, "federation__cost", "federation__listSize");
}

#[test]
fn errors_when_subgraphs_use_different_names() {
    let subgraph_with_default_name = Subgraph::parse(
        "subgraphWithDefaultName",
        "",
        r#"
        extend schema 
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/federation/v2.9")
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
            @link(url: "https://specs.apollo.dev/federation/v2.9")
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

    assert_eq!(errors.len(), 1, "Expected 1 error but got {}", errors.len());
    let error = errors.first().unwrap();
    assert_eq!(error.code(), ErrorCode::LinkImportNameMismatch);
    
    // The error message can reference either "@cost" or "@renamedCost" depending on which is encountered first
    let error_msg = error.to_string();
    assert!(
        error_msg.contains("directive (from https://specs.apollo.dev/cost/v0.1) is imported with mismatched name between subgraphs"),
        "Error message should mention cost spec and mismatched names"
    );
    assert!(
        error_msg.contains("\"@renamedCost\" in subgraph \"subgraphWithDifferentName\""),
        "Error message should mention @renamedCost in subgraphWithDifferentName"
    );
    assert!(
        error_msg.contains("\"@cost\" in subgraph \"subgraphWithDefaultName\""),
        "Error message should mention @cost in subgraphWithDefaultName"
    );
}

#[test]
fn hints_when_merging_cost_arguments() {
    let subgraph_a = Subgraph::parse(
        "subgraph-a",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@cost", "@shareable"])

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
            @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@cost", "@shareable"])

        type Query {
            sharedWithCost: Int @shareable @cost(weight: 10)
        }
    "#,
    )
    .unwrap();
    let result = compose(vec![subgraph_a, subgraph_b]).unwrap();

    /* TODO: Re-enable once FED-693 is merged
    assert_eq!(result.hints().len(), 1);
    let hint = result.hints().first().unwrap();
    assert_eq!(hint.code(), "MERGED_NON_REPEATABLE_DIRECTIVE_ARGUMENTS");
    assert_eq!(
        hint.message(),
        r#"Directive @cost is applied to "Query.sharedWithCost" in multiple subgraphs with different arguments. Merging strategies used by arguments: { "weight": MAX }""#
    );
    */

    let shared_with_cost = coord!(Query.sharedWithCost)
        .lookup_field(result.schema().schema())
        .unwrap();
    let cost_directive = shared_with_cost
        .directives
        .iter()
        .find(|d| d.name == "cost")
        .expect("Expected @cost directive to be present on Query.sharedWithCost");
    assert_eq!(cost_directive.to_string(), r#"@cost(weight: 10)"#);
}

#[test]
fn hints_when_merging_listsize_arguments() {
    let subgraph_a = Subgraph::parse(
        "subgraph-a",
        "",
        r#"
        extend schema
            @link(url: "https://specs.apollo.dev/link/v1.0")
            @link(url: "https://specs.apollo.dev/cost/v0.1", import: ["@listSize"])
            @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@shareable"])
    
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
            @link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@shareable"])

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
    // Note: Rust implementation doesn't quote keys in the merge strategies map
    assert_eq!(
        hint.message(),
        r#"Directive @listSize is applied to "Query.sharedWithListSize" in multiple subgraphs with different arguments. Merging strategies used by arguments: { assumedSize: NULLABLE_MAX, slicingArguments: NULLABLE_UNION, sizedFields: NULLABLE_UNION, requireOneSlicingArgument: NULLABLE_AND }"#
    )
}
