use std::sync::Arc;

use apollo_compiler::Schema;
use router_bridge::planner::PlanOptions;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use test_log::test;

use super::*;

// Generate the signature and referenced fields using router-bridge to confirm that the expected value we used is correct.
// We can remove this when we no longer use the bridge but should keep the rust implementation verifications.
async fn assert_bridge_results(
    schema_str: &str,
    query_str: &str,
    expected_sig: &str,
    expected_refs: &HashMap<String, ReferencedFieldsForType>,
) {
    let planner =
        Planner::<serde_json::Value>::new(schema_str.to_string(), QueryPlannerConfig::default())
            .await
            .unwrap();
    let plan = planner
        .plan(query_str.to_string(), None, PlanOptions::default())
        .await
        .unwrap();
    let bridge_result = ComparableUsageReporting {
        result: plan.usage_reporting,
    };
    let expected_result = UsageReporting {
        stats_report_key: expected_sig.to_string(),
        referenced_fields_by_type: expected_refs.clone(),
    };
    assert!(matches!(
        bridge_result.compare(&expected_result),
        UsageReportingComparisonResult::Equal
    ));
}

fn assert_expected_results(
    actual: &ComparableUsageReporting,
    expected_sig: &str,
    expected_refs: &HashMap<String, ReferencedFieldsForType>,
) {
    let expected_result = UsageReporting {
        stats_report_key: expected_sig.to_string(),
        referenced_fields_by_type: expected_refs.clone(),
    };
    assert!(matches!(
        actual.compare(&expected_result),
        UsageReportingComparisonResult::Equal
    ));
}

#[test(tokio::test)]
async fn test_complex_query() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query UnusedQuery {
            noInputQuery {
              enumResponse
            }
          }
          
          fragment UnusedFragment on EverythingResponse {
            enumResponse
          }
          
          fragment Fragment2 on EverythingResponse {
            basicTypes {
              nullableFloat
            }
          }
          
          query        TransformedQuery    {
          
          
            scalarInputQuery(idInput: "a1", listInput: [], boolInput: true, intInput: 1, stringInput: "x", floatInput: 1.2)      @skip(if: false)   @include(if: true) {
              ...Fragment2,
          
          
              objectTypeWithInputField(boolInput: true, secondInput: false) {
                stringField
                __typename
                intField
              }
          
              enumResponse
              interfaceResponse {
                sharedField
                ... on InterfaceImplementation2 {
                  implementation2Field
                }
                ... on InterfaceImplementation1 {
                  implementation1Field
                }
              }
              ...Fragment1,
            }
          }
          
          fragment Fragment1 on EverythingResponse {
            basicTypes {
              nonNullFloat
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("TransformedQuery".into()), &schema);

    let expected_sig = "# TransformedQuery\nfragment Fragment1 on EverythingResponse{basicTypes{nonNullFloat}}fragment Fragment2 on EverythingResponse{basicTypes{nullableFloat}}query TransformedQuery{scalarInputQuery(boolInput:true floatInput:0 idInput:\"\"intInput:0 listInput:[]stringInput:\"\")@skip(if:false)@include(if:true){enumResponse interfaceResponse{sharedField...on InterfaceImplementation2{implementation2Field}...on InterfaceImplementation1{implementation1Field}}objectTypeWithInputField(boolInput:true,secondInput:false){__typename intField stringField}...Fragment1...Fragment2}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["scalarInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "BasicTypesResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["nullableFloat".into(), "nonNullFloat".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec![
                    "basicTypes".into(),
                    "objectTypeWithInputField".into(),
                    "enumResponse".into(),
                    "interfaceResponse".into(),
                ],
                is_interface: false,
            },
        ),
        (
            "AnInterface".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into()],
                is_interface: true,
            },
        ),
        (
            "ObjectTypeResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["stringField".into(), "__typename".into(), "intField".into()],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation1".into(),
            ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into()],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation2".into(),
            ReferencedFieldsForType {
                field_names: vec!["implementation2Field".into()],
                is_interface: false,
            },
        ),
    ]);
    assert_expected_results(&generated, expected_sig, &expected_refs);

    // the router-bridge planner will throw errors on unused fragments/queries so we remove them here
    let sanitised_query_str = r#"fragment Fragment2 on EverythingResponse {
            basicTypes {
              nullableFloat
            }
          }
          
          query        TransformedQuery    {
          
          
            scalarInputQuery(idInput: "a1", listInput: [], boolInput: true, intInput: 1, stringInput: "x", floatInput: 1.2)      @skip(if: false)   @include(if: true) {
              ...Fragment2,
          
          
              objectTypeWithInputField(boolInput: true, secondInput: false) {
                stringField
                __typename
                intField
              }
          
              enumResponse
              interfaceResponse {
                sharedField
                ... on InterfaceImplementation2 {
                  implementation2Field
                }
                ... on InterfaceImplementation1 {
                  implementation1Field
                }
              }
              ...Fragment1,
            }
          }
          
          fragment Fragment1 on EverythingResponse {
            basicTypes {
              nonNullFloat
            }
          }"#;

    assert_bridge_results(
        schema_str,
        sanitised_query_str,
        expected_sig,
        &expected_refs,
    )
    .await;
}

#[test(tokio::test)]
async fn test_complex_references() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query Query($secondInput: Boolean!) {
            scalarResponseQuery
            noInputQuery {
              basicTypes {
                nonNullId
                nonNullInt
              }
              enumResponse
              interfaceImplementationResponse {
                sharedField
                implementation2Field
              }
              interfaceResponse {
                ... on InterfaceImplementation1 {
                  implementation1Field
                  sharedField
                }
                ... on InterfaceImplementation2 {
                  implementation2Field
                  sharedField
                }
              }
              listOfUnions {
                ... on UnionType1 {
                  nullableString
                }
              }
              objectTypeWithInputField(secondInput: $secondInput) {
                intField
              }
            }
            basicInputTypeQuery(input: { someFloat: 1 }) {
              unionResponse {
                ... on UnionType1 {
                  nullableString
                }
              }
              unionType2Response {
                unionType2Field
              }
              listOfObjects {
                stringField
              }
            }
          }"#;

    let schema: Valid<Schema> = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("Query".into()), &schema);

    let expected_sig = "# Query\nquery Query($secondInput:Boolean!){basicInputTypeQuery(input:{}){listOfObjects{stringField}unionResponse{...on UnionType1{nullableString}}unionType2Response{unionType2Field}}noInputQuery{basicTypes{nonNullId nonNullInt}enumResponse interfaceImplementationResponse{implementation2Field sharedField}interfaceResponse{...on InterfaceImplementation1{implementation1Field sharedField}...on InterfaceImplementation2{implementation2Field sharedField}}listOfUnions{...on UnionType1{nullableString}}objectTypeWithInputField(secondInput:$secondInput){intField}}scalarResponseQuery}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec![
                    "scalarResponseQuery".into(),
                    "noInputQuery".into(),
                    "basicInputTypeQuery".into(),
                ],
                is_interface: false,
            },
        ),
        (
            "BasicTypesResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["nonNullId".into(), "nonNullInt".into()],
                is_interface: false,
            },
        ),
        (
            "ObjectTypeResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["intField".into(), "stringField".into()],
                is_interface: false,
            },
        ),
        (
            "UnionType2".into(),
            ReferencedFieldsForType {
                field_names: vec!["unionType2Field".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec![
                    "basicTypes".into(),
                    "enumResponse".into(),
                    "interfaceImplementationResponse".into(),
                    "interfaceResponse".into(),
                    "listOfUnions".into(),
                    "objectTypeWithInputField".into(),
                    "unionResponse".into(),
                    "unionType2Response".into(),
                    "listOfObjects".into(),
                ],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation1".into(),
            ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            },
        ),
        (
            "UnionType1".into(),
            ReferencedFieldsForType {
                field_names: vec!["nullableString".into()],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation2".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            },
        ),
    ]);
    assert_expected_results(&generated, expected_sig, &expected_refs);

    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_basic_whitespace() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query MyQuery {
            noInputQuery {
              id
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("MyQuery".into()), &schema);

    let expected_sig = "# MyQuery\nquery MyQuery{noInputQuery{id}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_anonymous_query() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query {
            noInputQuery {
              id
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    let expected_sig = "# -\n{noInputQuery{id}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_anonymous_mutation() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"mutation {
            noInputMutation {
              id
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    let expected_sig = "# -\nmutation{noInputMutation{id}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Mutation".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputMutation".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_anonymous_subscription() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str: &str = r#"subscription {
            noInputSubscription {
              id
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    let expected_sig = "# -\nsubscription{noInputSubscription{id}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Subscription".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputSubscription".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_ordered_fields_and_variables() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query VariableScalarInputQuery($idInput: ID!, $boolInput: Boolean!, $floatInput: Float!, $intInput: Int!, $listInput: [String!]!, $stringInput: String!, $nullableStringInput: String) {
            sortQuery(
              idInput: $idInput
              boolInput: $boolInput
              floatInput: $floatInput
              INTInput: $intInput
              listInput: $listInput
              stringInput: $stringInput
              nullableStringInput: $nullableStringInput
            ) {
              zzz
              CCC
              nullableId
              aaa
              id
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(
        &doc,
        &doc,
        &Some("VariableScalarInputQuery".into()),
        &schema,
    );

    let expected_sig = "# VariableScalarInputQuery\nquery VariableScalarInputQuery($boolInput:Boolean!,$floatInput:Float!,$idInput:ID!,$intInput:Int!,$listInput:[String!]!,$nullableStringInput:String,$stringInput:String!){sortQuery(INTInput:$intInput boolInput:$boolInput floatInput:$floatInput idInput:$idInput listInput:$listInput nullableStringInput:$nullableStringInput stringInput:$stringInput){CCC aaa id nullableId zzz}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["sortQuery".into()],
                is_interface: false,
            },
        ),
        (
            "SortResponse".into(),
            ReferencedFieldsForType {
                field_names: vec![
                    "aaa".into(),
                    "CCC".into(),
                    "id".into(),
                    "nullableId".into(),
                    "zzz".into(),
                ],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_fragments() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query FragmentQuery {
            noInputQuery {
              listOfBools
              interfaceResponse {
                sharedField
                ... on InterfaceImplementation2 {
                  implementation2Field
                }
                ...bbbInterfaceFragment
                ...aaaInterfaceFragment
                ... {
                  ... on InterfaceImplementation1 {
                    implementation1Field
                  }
                }
                ... on InterfaceImplementation1 {
                  implementation1Field
                }
              }
              unionResponse {
                ... on UnionType2 {
                  unionType2Field
                }
                ... on UnionType1 {
                  unionType1Field
                }
              }
              ...zzzFragment
              ...aaaFragment
              ...ZZZFragment
            }
          }
          
          fragment zzzFragment on EverythingResponse {
            listOfInterfaces {
              sharedField
            }
          }
          
          fragment ZZZFragment on EverythingResponse {
            listOfInterfaces {
              sharedField
            }
          }
          
          fragment aaaFragment on EverythingResponse {
            listOfInterfaces {
              sharedField
            }
          }

          fragment UnusedFragment on InterfaceImplementation2 {
            sharedField
            implementation2Field
          }
          
          fragment bbbInterfaceFragment on InterfaceImplementation2 {
            sharedField
            implementation2Field
          }
          
          fragment aaaInterfaceFragment on InterfaceImplementation1 {
            sharedField
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("FragmentQuery".into()), &schema);

    let expected_sig = "# FragmentQuery\nfragment ZZZFragment on EverythingResponse{listOfInterfaces{sharedField}}fragment aaaFragment on EverythingResponse{listOfInterfaces{sharedField}}fragment aaaInterfaceFragment on InterfaceImplementation1{sharedField}fragment bbbInterfaceFragment on InterfaceImplementation2{implementation2Field sharedField}fragment zzzFragment on EverythingResponse{listOfInterfaces{sharedField}}query FragmentQuery{noInputQuery{interfaceResponse{sharedField...aaaInterfaceFragment...bbbInterfaceFragment...on InterfaceImplementation2{implementation2Field}...{...on InterfaceImplementation1{implementation1Field}}...on InterfaceImplementation1{implementation1Field}}listOfBools unionResponse{...on UnionType2{unionType2Field}...on UnionType1{unionType1Field}}...ZZZFragment...aaaFragment...zzzFragment}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "UnionType1".into(),
            ReferencedFieldsForType {
                field_names: vec!["unionType1Field".into()],
                is_interface: false,
            },
        ),
        (
            "UnionType2".into(),
            ReferencedFieldsForType {
                field_names: vec!["unionType2Field".into()],
                is_interface: false,
            },
        ),
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec![
                    "listOfInterfaces".into(),
                    "listOfBools".into(),
                    "interfaceResponse".into(),
                    "unionResponse".into(),
                ],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation1".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation1Field".into()],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation1".into(),
            ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            },
        ),
        (
            "AnInterface".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into()],
                is_interface: true,
            },
        ),
        (
            "InterfaceImplementation2".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);

    // the router-bridge planner will throw errors on unused fragments/queries so we remove them here
    let sanitised_query_str = r#"query FragmentQuery {
            noInputQuery {
              listOfBools
              interfaceResponse {
                sharedField
                ... on InterfaceImplementation2 {
                  implementation2Field
                }
                ...bbbInterfaceFragment
                ...aaaInterfaceFragment
                ... {
                  ... on InterfaceImplementation1 {
                    implementation1Field
                  }
                }
                ... on InterfaceImplementation1 {
                  implementation1Field
                }
              }
              unionResponse {
                ... on UnionType2 {
                  unionType2Field
                }
                ... on UnionType1 {
                  unionType1Field
                }
              }
              ...zzzFragment
              ...aaaFragment
              ...ZZZFragment
            }
          }
          
          fragment zzzFragment on EverythingResponse {
            listOfInterfaces {
              sharedField
            }
          }
          
          fragment ZZZFragment on EverythingResponse {
            listOfInterfaces {
              sharedField
            }
          }
          
          fragment aaaFragment on EverythingResponse {
            listOfInterfaces {
              sharedField
            }
          }
          
          fragment bbbInterfaceFragment on InterfaceImplementation2 {
            sharedField
            implementation2Field
          }
          
          fragment aaaInterfaceFragment on InterfaceImplementation1 {
            sharedField
          }"#;
    assert_bridge_results(
        schema_str,
        sanitised_query_str,
        expected_sig,
        &expected_refs,
    )
    .await;
}

#[test(tokio::test)]
async fn test_directives() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"fragment Fragment1 on InterfaceImplementation1 {
            sharedField
            implementation1Field
          }
          
          fragment Fragment2 on InterfaceImplementation2 @withArgs(arg2: "" arg1: "test" arg3: true arg5: [1,2] arg4: 2) @noArgs {
            sharedField
            implementation2Field
          }
          
          query DirectiveQuery @withArgs(arg2: "" arg1: "test") @noArgs {
            noInputQuery {
              enumResponse @withArgs(arg3: false arg5: [1,2] arg4: 2) @noArgs
              unionResponse {
                ... on UnionType1 @withArgs(arg2: "" arg1: "test") @noArgs {
                  unionType1Field
                }
              }
              interfaceResponse {
                ... Fragment1 @withArgs(arg1: "test") @noArgs
                ... Fragment2
              }
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("DirectiveQuery".into()), &schema);

    let expected_sig = "# DirectiveQuery\nfragment Fragment1 on InterfaceImplementation1{implementation1Field sharedField}fragment Fragment2 on InterfaceImplementation2@noArgs@withArgs(arg1:\"\",arg2:\"\",arg3:true,arg4:0,arg5:[]){implementation2Field sharedField}query DirectiveQuery@withArgs(arg1:\"\",arg2:\"\")@noArgs{noInputQuery{enumResponse@withArgs(arg3:false,arg4:0,arg5:[])@noArgs interfaceResponse{...Fragment1@noArgs@withArgs(arg1:\"\")...Fragment2}unionResponse{...on UnionType1@noArgs@withArgs(arg1:\"\",arg2:\"\"){unionType1Field}}}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "UnionType1".into(),
            ReferencedFieldsForType {
                field_names: vec!["unionType1Field".into()],
                is_interface: false,
            },
        ),
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec![
                    "enumResponse".into(),
                    "interfaceResponse".into(),
                    "unionResponse".into(),
                ],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation1".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation1Field".into()],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation1".into(),
            ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            },
        ),
        (
            "InterfaceImplementation2".into(),
            ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_aliases() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query AliasQuery {
            xxAlias: enumInputQuery(enumInput: SOME_VALUE_1) {
              aliased: enumResponse
            }
            aaAlias: enumInputQuery(enumInput: SOME_VALUE_2) {
              aliasedAgain: enumResponse
            }
            ZZAlias: enumInputQuery(enumInput: SOME_VALUE_3) {
              enumResponse
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("AliasQuery".into()), &schema);

    let expected_sig = "# AliasQuery\nquery AliasQuery{enumInputQuery(enumInput:SOME_VALUE_1){enumResponse}enumInputQuery(enumInput:SOME_VALUE_2){enumResponse}enumInputQuery(enumInput:SOME_VALUE_3){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumInputQuery".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_inline_values() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query InlineInputTypeQuery {
            inputTypeQuery(input: { 
                inputString: "foo", 
                inputInt: 42, 
                inputBoolean: null, 
                nestedType: { someFloat: 4.2 }, 
                enumInput: SOME_VALUE_1, 
                nestedTypeList: [ { someFloat: 4.2, someNullableFloat: null } ], 
                listInput: [1, 2, 3] 
            }) {
              enumResponse
            }
          }"#;
    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("InlineInputTypeQuery".into()), &schema);

    let expected_sig = "# InlineInputTypeQuery\nquery InlineInputTypeQuery{inputTypeQuery(input:{}){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["inputTypeQuery".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_root_type_fragment() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query SomeQuery {
            ... on Query {
              ... {
                basicResponseQuery {
                  id
                }
              }
            }
            noInputQuery {
              enumResponse
            }
          }"#;
    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    let expected_sig = "# SomeQuery\nquery SomeQuery{noInputQuery{enumResponse}...on Query{...{basicResponseQuery{id}}}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "BasicResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["basicResponseQuery".into(), "noInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_directive_arg_spacing() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query {
            basicResponseQuery {
              id @withArgs(arg1: "")
              id
            }
          }"#;
    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &None, &schema);

    let expected_sig = "# -\n{basicResponseQuery{id@withArgs(arg1:\"\")id}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "BasicResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["basicResponseQuery".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_operation_with_single_variable() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query QueryWithVar($input_enum: SomeEnum) {
            enumInputQuery(enumInput: $input_enum) {
              listOfBools
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("QueryWithVar".into()), &schema);

    let expected_sig = "# QueryWithVar\nquery QueryWithVar($input_enum:SomeEnum){enumInputQuery(enumInput:$input_enum){listOfBools}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["listOfBools".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_operation_with_multiple_variables() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query QueryWithVars($stringInput: String!, $floatInput: Float!, $boolInput: Boolean!) {
            scalarInputQuery(listInput: ["x"], stringInput: $stringInput, intInput: 6, floatInput: $floatInput, boolInput: $boolInput, idInput: "y") {
              enumResponse
            }
            inputTypeQuery(input: { inputInt: 2, inputString: "z", listInput: [], nestedType: { someFloat: 5 }}) {
              enumResponse
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("QueryWithVars".into()), &schema);

    let expected_sig = "# QueryWithVars\nquery QueryWithVars($boolInput:Boolean!,$floatInput:Float!,$stringInput:String!){inputTypeQuery(input:{}){enumResponse}scalarInputQuery(boolInput:$boolInput floatInput:$floatInput idInput:\"\"intInput:0 listInput:[]stringInput:$stringInput){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["scalarInputQuery".into(), "inputTypeQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_field_arg_comma_or_space() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query QueryArgLength($StringInputWithAVeryyyLongNameSoLineLengthIs80: String!, $inputType: AnotherInputType, $enumInputWithAVryLongNameSoLineLengthIsOver80: SomeEnum, $enumInputType: EnumInputType) {
            enumInputQuery (enumInput:$enumInputWithAVryLongNameSoLineLengthIsOver80,inputType:$enumInputType) {
              enumResponse
            }
            defaultArgQuery(stringInput:$StringInputWithAVeryyyLongNameSoLineLengthIs80,inputType:$inputType) {
              id
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("QueryArgLength".into()), &schema);

    // enumInputQuery has a variable line length of 81, so it should be separated by spaces (which are converted from newlines
    // in the original implementation).
    // enumInputQuery has a variable line length of 80, so it should be separated by commas.
    let expected_sig = "# QueryArgLength\nquery QueryArgLength($StringInputWithAVeryyyLongNameSoLineLengthIs80:String!,$enumInputType:EnumInputType,$enumInputWithAVryLongNameSoLineLengthIsOver80:SomeEnum,$inputType:AnotherInputType){defaultArgQuery(inputType:$inputType stringInput:$StringInputWithAVeryyyLongNameSoLineLengthIs80){id}enumInputQuery(enumInput:$enumInputWithAVryLongNameSoLineLengthIsOver80 inputType:$enumInputType){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumInputQuery".into(), "defaultArgQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
        (
            "BasicResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["id".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_operation_arg_always_commas() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query QueryArgLength($enumInputWithAVerrrrrrrrrrrryLongNameSoLineLengthIsOver80: SomeEnum, $enumInputType: EnumInputType) {
            enumInputQuery (enumInput:$enumInputWithAVerrrrrrrrrrrryLongNameSoLineLengthIsOver80,inputType:$enumInputType) {
              enumResponse
            }
          }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("QueryArgLength".into()), &schema);

    // operation variables shouldn't ever be converted to spaces, since the line length check is only on field variables
    // in the original implementation
    let expected_sig = "# QueryArgLength\nquery QueryArgLength($enumInputType:EnumInputType,$enumInputWithAVerrrrrrrrrrrryLongNameSoLineLengthIsOver80:SomeEnum){enumInputQuery(enumInput:$enumInputWithAVerrrrrrrrrrrryLongNameSoLineLengthIsOver80 inputType:$enumInputType){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_comma_separator_always() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"query QueryCommaEdgeCase {
        enumInputQuery (anotherStr:"",enumInput:SOME_VALUE_1,stringInput:"") {
          enumResponse
        }
      }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("QueryCommaEdgeCase".into()), &schema);

    let expected_sig = "# QueryCommaEdgeCase\nquery QueryCommaEdgeCase{enumInputQuery(anotherStr:\"\",enumInput:SOME_VALUE_1,stringInput:\"\"){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_nested_fragments() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = "
      fragment UnionType1Fragment on UnionType1 {
        unionType1Field
      }

      fragment ObjectResponseFragment on ObjectTypeResponse {
        intField
      }

      fragment EverythingResponseFragment on EverythingResponse {
        listOfObjects {
          ...ObjectResponseFragment
          ... on ObjectTypeResponse {
            stringField
          }
        }
      }

      query NestedFragmentQuery {
        noInputQuery {
          ...EverythingResponseFragment
          ... on EverythingResponse {
            listOfUnions {
              ...UnionType1Fragment
              ... on UnionType2 {
                unionType2Field
              }
            }
          }
        }
      }";

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("NestedFragmentQuery".into()), &schema);

    let expected_sig = "# NestedFragmentQuery\nfragment EverythingResponseFragment on EverythingResponse{listOfObjects{...ObjectResponseFragment...on ObjectTypeResponse{stringField}}}fragment ObjectResponseFragment on ObjectTypeResponse{intField}fragment UnionType1Fragment on UnionType1{unionType1Field}query NestedFragmentQuery{noInputQuery{...EverythingResponseFragment...on EverythingResponse{listOfUnions{...UnionType1Fragment...on UnionType2{unionType2Field}}}}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["listOfObjects".into(), "listOfUnions".into()],
                is_interface: false,
            },
        ),
        (
            "ObjectTypeResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["intField".into(), "stringField".into()],
                is_interface: false,
            },
        ),
        (
            "UnionType1".into(),
            ReferencedFieldsForType {
                field_names: vec!["unionType1Field".into()],
                is_interface: false,
            },
        ),
        (
            "UnionType2".into(),
            ReferencedFieldsForType {
                field_names: vec!["unionType2Field".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_mutation_space() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = "
      mutation Test_Mutation_Space($arg1withalongnamegoeshere0123456789: Boolean) {
        mutation2(id: \"x\") {
          updateCheckConfiguration(arg1: $arg1withalongnamegoeshere0123456789, arg2: false)
        }
      }";

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("Test_Mutation_Space".into()), &schema);

    let expected_sig = "# Test_Mutation_Space\nmutation Test_Mutation_Space($arg1withalongnamegoeshere0123456789:Boolean){mutation2(id:\"\"){updateCheckConfiguration(arg1:$arg1withalongnamegoeshere0123456789 arg2:false)}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Mutation".into(),
            ReferencedFieldsForType {
                field_names: vec!["mutation2".into()],
                is_interface: false,
            },
        ),
        (
            "TestGraphResponse2".into(),
            ReferencedFieldsForType {
                field_names: vec!["updateCheckConfiguration".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_mutation_comma() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = "
      mutation Test_Mutation_Comma($arg1withalongnamegoeshere012345678: Boolean) {
        mutation2(id: \"x\") {
          updateCheckConfiguration(arg1: $arg1withalongnamegoeshere012345678, arg2: false)
        }
      }";

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("Test_Mutation_Comma".into()), &schema);

    let expected_sig = "# Test_Mutation_Comma\nmutation Test_Mutation_Comma($arg1withalongnamegoeshere012345678:Boolean){mutation2(id:\"\"){updateCheckConfiguration(arg1:$arg1withalongnamegoeshere012345678,arg2:false)}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Mutation".into(),
            ReferencedFieldsForType {
                field_names: vec!["mutation2".into()],
                is_interface: false,
            },
        ),
        (
            "TestGraphResponse2".into(),
            ReferencedFieldsForType {
                field_names: vec!["updateCheckConfiguration".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_comma_lower_bound() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = "
      query TestCommaLowerBound($arg: String, $slightlyTooLongName1234: String) {
        manyArgsQuery(arg1: $arg, arg2: $arg, arg3: $arg, arg4: $slightlyTooLongName1234) {
          enumResponse
        }
      }";

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("TestCommaLowerBound".into()), &schema);

    let expected_sig = "# TestCommaLowerBound\nquery TestCommaLowerBound($arg:String,$slightlyTooLongName1234:String){manyArgsQuery(arg1:$arg arg2:$arg arg3:$arg arg4:$slightlyTooLongName1234){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["manyArgsQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_comma_upper_bound() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = "
      query TestCommaUpperBound($arg: String, $slightlyTooLongName12345: String) {
        manyArgsQuery(arg1: $arg, arg2: $arg, arg3: $arg, arg4: $slightlyTooLongName12345) {
          enumResponse
        }
      }";

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_usage_reporting(&doc, &doc, &Some("TestCommaUpperBound".into()), &schema);

    let expected_sig = "# TestCommaUpperBound\nquery TestCommaUpperBound($arg:String,$slightlyTooLongName12345:String){manyArgsQuery(arg1:$arg arg2:$arg arg3:$arg arg4:$slightlyTooLongName12345){enumResponse}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["manyArgsQuery".into()],
                is_interface: false,
            },
        ),
        (
            "EverythingResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_underscore() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = "
      query UnderscoreQuery($arg2_: String, $_arg3_: String) {
        underscoreQuery(arg_: \"x\", _arg2: $arg2_, _arg3_: $_arg3_) {
          _
          _name
          _name_
          name_
        }
      }";

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_usage_reporting(&doc, &doc, &Some("UnderscoreQuery".into()), &schema);

    let expected_sig = "# UnderscoreQuery\nquery UnderscoreQuery($_arg3_:String,$arg2_:String){underscoreQuery(_arg2:$arg2_,_arg3_:$_arg3_,arg_:\"\"){_ _name _name_ name_}}";
    let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
        (
            "Query".into(),
            ReferencedFieldsForType {
                field_names: vec!["underscoreQuery".into()],
                is_interface: false,
            },
        ),
        (
            "UnderscoreResponse".into(),
            ReferencedFieldsForType {
                field_names: vec!["_".into(), "_name".into(), "_name_".into(), "name_".into()],
                is_interface: false,
            },
        ),
    ]);

    assert_expected_results(&generated, expected_sig, &expected_refs);
    assert_bridge_results(schema_str, query_str, expected_sig, &expected_refs).await;
}

#[test(tokio::test)]
async fn test_compare() {
    let source = ComparableUsageReporting {
        result: UsageReporting {
            stats_report_key: "# -\n{basicResponseQuery{field1 field2}}".into(),
            referenced_fields_by_type: HashMap::from([
                (
                    "Query".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["basicResponseQuery".into()],
                        is_interface: false,
                    },
                ),
                (
                    "SomeResponse".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["field1".into(), "field2".into()],
                        is_interface: false,
                    },
                ),
            ]),
        },
    };

    // Same signature and ref fields should match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: source.result.referenced_fields_by_type.clone(),
        }),
        UsageReportingComparisonResult::Equal
    ));

    // Reordered signature should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: "# -\n{basicResponseQuery{field2 field1}}".into(),
            referenced_fields_by_type: source.result.referenced_fields_by_type.clone(),
        }),
        UsageReportingComparisonResult::StatsReportKeyNotEqual
    ));

    // Different signature should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: "# NamedQuery\nquery NamedQuery {basicResponseQuery{field1 field2}}"
                .into(),
            referenced_fields_by_type: source.result.referenced_fields_by_type.clone(),
        }),
        UsageReportingComparisonResult::StatsReportKeyNotEqual
    ));

    // Reordered parent type should match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: HashMap::from([
                (
                    "Query".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["basicResponseQuery".into()],
                        is_interface: false,
                    },
                ),
                (
                    "SomeResponse".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["field1".into(), "field2".into()],
                        is_interface: false,
                    },
                ),
            ])
        }),
        UsageReportingComparisonResult::Equal
    ));

    // Reordered fields should match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: HashMap::from([
                (
                    "Query".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["basicResponseQuery".into()],
                        is_interface: false,
                    },
                ),
                (
                    "SomeResponse".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["field2".into(), "field1".into()],
                        is_interface: false,
                    },
                ),
            ])
        }),
        UsageReportingComparisonResult::Equal
    ));

    // Added parent type should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: HashMap::from([
                (
                    "Query".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["basicResponseQuery".into()],
                        is_interface: false,
                    },
                ),
                (
                    "SomeResponse".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["field1".into(), "field2".into()],
                        is_interface: false,
                    },
                ),
                (
                    "OtherType".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["otherField".into()],
                        is_interface: false,
                    },
                ),
            ])
        }),
        UsageReportingComparisonResult::ReferencedFieldsNotEqual
    ));

    // Added field should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: HashMap::from([
                (
                    "Query".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["basicResponseQuery".into()],
                        is_interface: false,
                    },
                ),
                (
                    "SomeResponse".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["field1".into(), "field2".into(), "field3".into()],
                        is_interface: false,
                    },
                ),
            ])
        }),
        UsageReportingComparisonResult::ReferencedFieldsNotEqual
    ));

    // Missing parent type should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: HashMap::from([(
                "Query".into(),
                ReferencedFieldsForType {
                    field_names: vec!["basicResponseQuery".into()],
                    is_interface: false,
                },
            ),])
        }),
        UsageReportingComparisonResult::ReferencedFieldsNotEqual
    ));

    // Missing field should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: source.result.stats_report_key.clone(),
            referenced_fields_by_type: HashMap::from([
                (
                    "Query".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["basicResponseQuery".into()],
                        is_interface: false,
                    },
                ),
                (
                    "SomeResponse".into(),
                    ReferencedFieldsForType {
                        field_names: vec!["field1".into()],
                        is_interface: false,
                    },
                ),
            ])
        }),
        UsageReportingComparisonResult::ReferencedFieldsNotEqual
    ));

    // Both different should not match
    assert!(matches!(
        source.compare(&UsageReporting {
            stats_report_key: "# -\n{basicResponseQuery{field2 field1}}".into(),
            referenced_fields_by_type: HashMap::from([(
                "Query".into(),
                ReferencedFieldsForType {
                    field_names: vec!["basicResponseQuery".into()],
                    is_interface: false,
                },
            ),])
        }),
        UsageReportingComparisonResult::BothNotEqual
    ));
}

#[test(tokio::test)]
async fn test_generate_extended_references_inline_enums() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"
      fragment EnumFragment on Query {
        query2: enumInputQuery(enumInput: SOME_VALUE_4) {
          listOfBools
        }
      }
      
      query InlineEnumQuery {
        query1: enumInputQuery(enumInput: SOME_VALUE_1, inputType: { enumInput: SOME_VALUE_2, enumListInput: [SOME_VALUE_3] }) {
          basicTypes {
            nonNullId
          }
        }
        ...EnumFragment
        ... {
          query3: enumInputQuery(enumInput: SOME_VALUE_5) {
            listOfBools
          }
        }
      }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_extended_references(Arc::new(doc), Some("InlineEnumQuery".into()), &schema);

    println!("generated: {:?}", generated);
}

#[test(tokio::test)]
async fn test_generate_extended_references_variable_enums() {
    // todo
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = r#"
      query AliasQuery2($var1: SomeEnum) {
        enumInputQuery(enumInput: $var1) {
          basicTypes {
            nonNullId
          }
        }
      }"#;

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_extended_references(Arc::new(doc), Some("AliasQuery2".into()), &schema);

    println!("generated: {:?}", generated);
}
