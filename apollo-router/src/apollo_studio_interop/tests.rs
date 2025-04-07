use apollo_compiler::Schema;
use test_log::test;

use super::*;
use crate::Configuration;

fn assert_expected_signature(actual: &UsageReporting, expected_sig: &str) {
    assert_eq!(actual.get_stats_report_key(None), expected_sig);
}

macro_rules! assert_extended_references {
    ($actual:expr) => {
        insta::with_settings!({sort_maps => true}, {
            insta::assert_yaml_snapshot!($actual, {
                // sort referenced enum value sets
                ".referenced_enums.*" => insta::sorted_redaction()
            });
        });
    };
}

macro_rules! assert_enums_from_response {
    ($actual:expr) => {
        insta::with_settings!({sort_maps => true}, {
            insta::assert_yaml_snapshot!($actual, {
                // sort referenced enum value sets
                ".*" => insta::sorted_redaction()
            });
        });
    };
}

// Generate usage reporting with the same signature and refs doc, and with enhanced normalization algorithm
fn generate_enhanced(
    doc: &ExecutableDocument,
    operation_name: &Option<String>,
    schema: &Valid<Schema>,
) -> UsageReporting {
    generate_usage_reporting(
        doc,
        doc,
        operation_name,
        schema,
        &ApolloSignatureNormalizationAlgorithm::Enhanced,
    )
}

// Generate extended references (input objects and enum values)
fn generate_extended_refs(
    doc: &Valid<ExecutableDocument>,
    operation_name: Option<String>,
    schema: &Valid<Schema>,
    variables: Option<&Object>,
) -> ExtendedReferenceStats {
    let default_vars = Object::new();
    generate_extended_references(
        Arc::new(doc.clone()),
        operation_name,
        schema,
        variables.unwrap_or(&default_vars),
    )
}

fn enums_from_response(
    query_str: &str,
    operation_name: Option<&str>,
    schema_str: &str,
    response_body_str: &str,
) -> ReferencedEnums {
    let config = Configuration::default();
    let schema = crate::spec::Schema::parse(schema_str, &config).unwrap();
    let query = Query::parse(query_str, operation_name, &schema, &config).unwrap();
    let response_body: Object = serde_json::from_str(response_body_str).unwrap();

    let mut result = ReferencedEnums::new();
    extract_enums_from_response(
        Arc::new(query),
        schema.supergraph_schema(),
        &response_body,
        &mut result,
    );
    result
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
    #[allow(clippy::literal_string_with_formatting_args)]
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
async fn test_extended_references_inline_enums() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_inline_enums.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_extended_refs(&doc, Some("EnumInlineQuery".into()), &schema, None);
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_var_enums() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_var_enums.graphql");
    let query_vars_str = include_str!("testdata/extended_references_var_enums.json");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();
    let vars: Object = serde_json::from_str(query_vars_str).unwrap();

    let generated = generate_extended_refs(&doc, Some("EnumVarQuery".into()), &schema, Some(&vars));
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_fragment_inline_enums() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_fragment_inline_enums.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_extended_refs(
        &doc,
        Some("EnumInlineQueryWithFragment".into()),
        &schema,
        None,
    );
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_fragment_var_enums() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_fragment_var_enums.graphql");
    let query_vars_str = include_str!("testdata/extended_references_fragment_var_enums.json");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();
    let vars: Object = serde_json::from_str(query_vars_str).unwrap();

    let generated = generate_extended_refs(
        &doc,
        Some("EnumVarQueryWithFragment".into()),
        &schema,
        Some(&vars),
    );
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_inline_type() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_inline_type.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();

    let generated =
        generate_extended_refs(&doc, Some("InputTypeInlineQuery".into()), &schema, None);
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_var_type() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_var_type.graphql");
    let query_vars_str = include_str!("testdata/extended_references_var_type.json");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();
    let vars: Object = serde_json::from_str(query_vars_str).unwrap();

    let generated = generate_extended_refs(
        &doc,
        Some("InputTypeVariablesQuery".into()),
        &schema,
        Some(&vars),
    );
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_inline_nested_type() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_inline_nested_type.graphql");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();

    let generated = generate_extended_refs(
        &doc,
        Some("NestedInputTypeInlineQuery".into()),
        &schema,
        None,
    );
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_extended_references_var_nested_type() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/extended_references_var_nested_type.graphql");
    let query_vars_str = include_str!("testdata/extended_references_var_nested_type.json");

    let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
    let doc = ExecutableDocument::parse_and_validate(&schema, query_str, "query.graphql").unwrap();
    let vars: Object = serde_json::from_str(query_vars_str).unwrap();

    let generated = generate_extended_refs(
        &doc,
        Some("NestedInputTypeVarsQuery".into()),
        &schema,
        Some(&vars),
    );
    assert_extended_references!(&generated);
}

#[test(tokio::test)]
async fn test_enums_from_response_complex_response_type() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/enums_from_response_complex_response_type.graphql");
    let response_str =
        include_str!("testdata/enums_from_response_complex_response_type_response.json");
    let op_name = Some("EnumResponseQuery");

    let generated = enums_from_response(query_str, op_name, schema_str, response_str);
    assert_enums_from_response!(&generated);
}

#[test(tokio::test)]
async fn test_enums_from_response_fragments() {
    let schema_str = include_str!("testdata/schema_interop.graphql");
    let query_str = include_str!("testdata/enums_from_response_fragments.graphql");
    let response_str = include_str!("testdata/enums_from_response_fragments_response.json");
    let op_name = Some("EnumResponseQueryFragments");

    let generated = enums_from_response(query_str, op_name, schema_str, response_str);
    assert_enums_from_response!(&generated);
}

#[test]
fn apollo_operation_id_hash() {
    let usage_reporting = UsageReporting {
        operation_name: Some("IgnitionMeQuery".to_string()),
        operation_signature: Some("query IgnitionMeQuery{me{id}}".to_string()),
        error_key: None,
        referenced_fields_by_type: HashMap::new(),
    };

    assert_eq!(
        "d1554552698157b05c2a462827fb4367a4548ee5",
        usage_reporting.get_operation_id()
    );
}

// The Apollo operation ID hash for these errors is based on a slightly different string. E.g. instead of hashing
// "## GraphQLValidationFailure\n" we should hash "# # GraphQLValidationFailure".
#[test]
fn apollo_error_operation_id_hash() {
    assert_eq!(
        "ea4f152696abedca148b016d72df48842b713697",
        UsageReporting::for_error("GraphQLValidationFailure".into()).get_operation_id()
    );
    assert_eq!(
        "3f410834f13153f401ffe73f7e454aa500d10bf7",
        UsageReporting::for_error("GraphQLParseFailure".into()).get_operation_id()
    );
    assert_eq!(
        "7486043da2085fed407d942508a572ef88dc8120",
        UsageReporting::for_error("GraphQLUnknownOperationName".into()).get_operation_id()
    );
}
