use std::sync::Arc;

use apollo_compiler::Schema;
use router_bridge::planner::PlanOptions;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;
use test_log::test;

use super::*;

macro_rules! assert_generated_report {
    ($actual:expr) => {
        // Field names need sorting
        let mut result = $actual.result;
        for ty in result.referenced_fields_by_type.values_mut() {
            ty.field_names.sort();
        }

        insta::with_settings!({sort_maps => true, snapshot_suffix => "report"}, {
            insta::assert_yaml_snapshot!(result);
        });
    };
}

// Generate the signature and referenced fields using router-bridge to confirm that the expected value we used is correct.
// We can remove this when we no longer use the bridge but should keep the rust implementation verifications.
macro_rules! assert_bridge_results {
    ($schema_str:expr, $query_str:expr) => {
        let planner = Planner::<serde_json::Value>::new(
            $schema_str.to_string(),
            QueryPlannerConfig::default(),
        )
        .await
        .unwrap();
        let mut plan = planner
            .plan($query_str.to_string(), None, PlanOptions::default())
            .await
            .unwrap();

         // Field names need sorting
        for ty in plan.usage_reporting.referenced_fields_by_type.values_mut() {
            ty.field_names.sort();
        }

        insta::with_settings!({sort_maps => true, snapshot_suffix => "bridge"}, {
            insta::assert_yaml_snapshot!(plan.usage_reporting);
        });
    };
}

fn assert_expected_signature(actual: &ComparableUsageReporting, expected_sig: &str) {
    assert_eq!(actual.result.stats_report_key, expected_sig);
}

// Generate usage reporting with the same signature and refs doc, and with legacy normalization algorithm
fn generate_legacy(
    doc: &ExecutableDocument,
    operation_name: &Option<String>,
    schema: &Valid<Schema>,
) -> ComparableUsageReporting {
    generate_usage_reporting(
        doc,
        doc,
        operation_name,
        schema,
        &ApolloSignatureNormalizationAlgorithm::Legacy,
    )
}

// Generate usage reporting with the same signature and refs doc, and with enhanced normalization algorithm
fn generate_enhanced(
    doc: &ExecutableDocument,
    operation_name: &Option<String>,
    schema: &Valid<Schema>,
) -> ComparableUsageReporting {
    generate_usage_reporting(
        doc,
        doc,
        operation_name,
        schema,
        &ApolloSignatureNormalizationAlgorithm::Enhanced,
    )
}

#[test(tokio::test)]
async fn test_complex_query() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/complex_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("TransformedQuery".into()), &schema);

    assert_generated_report!(generated);

    // the router-bridge planner will throw errors on unused fragments/queries so we remove them here
    let sanitised_query_str = include_str!("testdata/complex_query_sanitized.graphql");

    assert_bridge_results!(schema_str, sanitised_query_str);
}

#[test(tokio::test)]
async fn test_complex_references() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/complex_references_query.graphql");

    let schema: Valid<Schema> = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("Query".into()), &schema);

    assert_generated_report!(generated);

    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_basic_whitespace() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/named_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("MyQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_anonymous_query() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/anonymous_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &None, &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_anonymous_mutation() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/anonymous_mutation.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &None, &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_anonymous_subscription() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str: &str = include_str!("testdata/subscription_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &None, &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_ordered_fields_and_variables() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/ordered_fields_and_variables_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("VariableScalarInputQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_fragments() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/fragments_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("FragmentQuery".into()), &schema);

    assert_generated_report!(generated);

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
    assert_bridge_results!(schema_str, sanitised_query_str);
}

#[test(tokio::test)]
async fn test_directives() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/directives_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("DirectiveQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_aliases() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/aliases_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("AliasQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_inline_values() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/inline_values_query.graphql");
    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("InlineInputTypeQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_root_type_fragment() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/root_type_fragment_query.graphql");
    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &None, &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_directive_arg_spacing() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/directive_arg_spacing_query.graphql");
    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &None, &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_operation_with_single_variable() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/operation_with_single_variable_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("QueryWithVar".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_operation_with_multiple_variables() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/operation_with_multiple_variables_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("QueryWithVars".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_field_arg_comma_or_space() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/field_arg_comma_or_space_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("QueryArgLength".into()), &schema);

    // enumInputQuery has a variable line length of 81, so it should be separated by spaces (which are converted from newlines
    // in the original implementation).
    // enumInputQuery has a variable line length of 80, so it should be separated by commas.
    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_operation_arg_always_commas() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/operation_arg_always_commas_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("QueryArgLength".into()), &schema);

    // operation variables shouldn't ever be converted to spaces, since the line length check is only on field variables
    // in the original implementation
    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_comma_separator_always() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/comma_separator_always_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("QueryCommaEdgeCase".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_nested_fragments() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/nested_fragments_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("NestedFragmentQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_mutation_space() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/mutation_space_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("Test_Mutation_Space".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_mutation_comma() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/mutation_comma_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("Test_Mutation_Comma".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_comma_lower_bound() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/comma_lower_bound_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("TestCommaLowerBound".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_comma_upper_bound() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/comma_upper_bound_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("TestCommaUpperBound".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_underscore() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/underscore_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_legacy(&doc, &Some("UnderscoreQuery".into()), &schema);

    assert_generated_report!(generated);
    assert_bridge_results!(schema_str, query_str);
}

#[test(tokio::test)]
async fn test_enhanced_uses_comma_always() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/enhanced_uses_comma_always_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_enhanced(&doc, &Some("TestCommaEnhanced".into()), &schema);
    let expected_sig = "# TestCommaEnhanced\nquery TestCommaEnhanced($arg1:String,$arg2:String,$veryMuchUsuallyTooLongName1234567890:String){manyArgsQuery(arg1:$arg1,arg2:$arg2,arg3:\"\",arg4:$veryMuchUsuallyTooLongName1234567890){basicTypes{nullableId}enumResponse id}}";
    assert_expected_signature(&generated, expected_sig);
}

#[test(tokio::test)]
async fn test_enhanced_sorts_fragments() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/enhanced_sorts_fragments_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_enhanced(&doc, &Some("EnhancedFragmentQuery".into()), &schema);
    let expected_sig = "# EnhancedFragmentQuery\nfragment ZZZFragment on EverythingResponse{listOfInterfaces{sharedField}}fragment aaaFragment on EverythingResponse{listOfInterfaces{sharedField}}fragment aaaInterfaceFragment on InterfaceImplementation1{sharedField}fragment bbbInterfaceFragment on InterfaceImplementation2{implementation2Field sharedField}fragment zzzFragment on EverythingResponse{listOfInterfaces{sharedField}}query EnhancedFragmentQuery{noInputQuery{interfaceResponse{...aaaInterfaceFragment...bbbInterfaceFragment...{...on InterfaceImplementation1{implementation1Field}}...{...on InterfaceImplementation2{sharedField}}...on InterfaceImplementation1{implementation1Field}...on InterfaceImplementation2{implementation2Field}}listOfBools unionResponse{...on UnionType1{unionType1Field}...on UnionType2{unionType2Field}}...ZZZFragment...aaaFragment...zzzFragment}}";
    assert_expected_signature(&generated, expected_sig);
}

#[test(tokio::test)]
async fn test_enhanced_sorts_directives() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/enhanced_sorts_directives_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_enhanced(&doc, &Some("DirectiveQuery".into()), &schema);
    let expected_sig = "# DirectiveQuery\nfragment Fragment1 on InterfaceImplementation1{implementation1Field sharedField}fragment Fragment2 on InterfaceImplementation2@noArgs@withArgs(arg1:\"\",arg2:\"\",arg3:true,arg4:0,arg5:[]){implementation2Field sharedField}query DirectiveQuery@noArgs@withArgs(arg1:\"\",arg2:\"\"){noInputQuery{enumResponse@noArgs@withArgs(arg3:false,arg4:0,arg5:[])interfaceResponse{...Fragment1@noArgs@withArgs(arg1:\"\")...Fragment2}unionResponse{...on UnionType1@noArgs@withArgs(arg1:\"\",arg2:\"\"){unionType1Field}}}}";
    assert_expected_signature(&generated, expected_sig);
}

#[test(tokio::test)]
async fn test_enhanced_inline_input_object() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str: &str = include_str!("testdata/enhanced_inline_input_object_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_enhanced(&doc, &Some("InputObjectTypeQuery".into()), &schema);
    let expected_sig = "# InputObjectTypeQuery\nquery InputObjectTypeQuery{inputTypeQuery(input:{inputString:\"\",inputInt:0,inputBoolean:null,nestedType:{someFloat:0},enumInput:SOME_VALUE_1,nestedTypeList:[],listInput:[]}){enumResponse}}";
    assert_expected_signature(&generated, expected_sig);
}

#[test(tokio::test)]
async fn test_enhanced_alias_preservation() {
    let schema_str = include_str!("testdata/schema_interop.graphql");

    let query_str = include_str!("testdata/enhanced_alias_preservation_query.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_enhanced(&doc, &Some("AliasQuery".into()), &schema);
    let expected_sig = "# AliasQuery\nquery AliasQuery{enumInputQuery(enumInput:SOME_VALUE_1){enumResponse nullableId aliasedId:id}ZZAlias:enumInputQuery(enumInput:SOME_VALUE_3){enumResponse}aaAlias:enumInputQuery(enumInput:SOME_VALUE_2){aliasedAgain:enumResponse}xxAlias:enumInputQuery(enumInput:SOME_VALUE_1){aliased:enumResponse}}";
    assert_expected_signature(&generated, expected_sig);
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
