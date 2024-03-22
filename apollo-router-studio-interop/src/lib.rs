use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use apollo_compiler::ast::Name;
use apollo_compiler::ast::OperationType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use router_bridge::planner::ReferencedFieldsForType;

fn format_operation_for_report(operation: &Node<apollo_compiler::executable::Operation>, fragments: &HashMap<String, Node<apollo_compiler::executable::Fragment>>) -> String {
    // Sorted list of fragments goes first
    let mut sorted_fragments: Vec<_> = fragments.into_iter().collect();
    sorted_fragments.sort_by_key(|&(k, _)| k);

    let mut body_sections = sorted_fragments.into_iter()
        .map(|(_name, fragment)| ApolloReportingSignatureFormatter::Fragment(fragment).to_string())
        .collect::<Vec<_>>();

    // Then the operation
    body_sections.push(ApolloReportingSignatureFormatter::Operation(operation).to_string());

    let body = body_sections.join("\n");

    // This is based on apollo-utils packages/printWithReducedWhitespace/src/index.ts
    // Note that we don't need to do the StringValue -> hex -> StringValue processing because all StringValues
    // are replaced with an empty string before we get here.
    // todo can we just update the generation code so we don't need to strip any whitespace? Try this after fuzzing.
    let stripped_body = regex::Regex::new(r"\s+").unwrap()
        .replace_all(&body, " ").to_string();
    let stripped_body = regex::Regex::new(r"([^_a-zA-Z0-9]) ").unwrap()
        .replace_all(&stripped_body, "$1").to_string();
    let stripped_body = regex::Regex::new(r" ([^_a-zA-Z0-9])").unwrap()
        .replace_all(&stripped_body, "$1").to_string();

    let op_name = match &operation.name {
        None => "-".into(),
        Some(node) => node.to_string(),
    };
    format!("# {}\n{}", op_name, stripped_body)
}

pub fn generate_apollo_reporting_signature(doc: &ExecutableDocument, operation_name: Option<String>) -> String {
    match doc.get_operation(operation_name.as_deref()).ok() {
        None => {
            // todo Print the whole document after transforming (that's what the JS impl does).
            // See apollo-utils packages/dropUnusedDefinitions/src/index.ts
            // Or return an error - if the operation can't be found, would we have thrown an error before getting here?
            "".to_string()
        },
        Some(operation) => {
            // todo figure out used fragments - do this in a "op and ref fields processor" class while generating refs to minimise 
            // the number of times we loop through the whole operation
            let mut seen_fragments: HashMap<String, Node<apollo_compiler::executable::Fragment>> = HashMap::new();

            fn extract_frags(selection_set: &apollo_compiler::executable::SelectionSet, doc: &ExecutableDocument, seen_fragments: &mut HashMap<String, Node<apollo_compiler::executable::Fragment>>) {
                for selection in &selection_set.selections {
                    match selection {
                        apollo_compiler::executable::Selection::Field(field) => {
                            extract_frags(&field.selection_set, doc, seen_fragments);
                        },
                        apollo_compiler::executable::Selection::InlineFragment(fragment) => {
                            extract_frags(&fragment.selection_set, doc, seen_fragments);
                        },
                        apollo_compiler::executable::Selection::FragmentSpread(fragment_node) => {
                            if !seen_fragments.contains_key(&fragment_node.fragment_name.to_string()) {
                                if let Some(fragment) = doc.fragments.get(&fragment_node.fragment_name) {
                                    seen_fragments.insert(fragment_node.fragment_name.to_string(), fragment.clone());
                                }
                            }
                        }
                    }
                }
            }    
            extract_frags(&operation.selection_set, doc, &mut seen_fragments);

            format_operation_for_report(&operation, &seen_fragments)
        }
    }
    
}

pub fn generate_apollo_reporting_refs(doc: &ExecutableDocument, operation_name: Option<String>, schema: &Valid<apollo_compiler::Schema>) -> HashMap<String, ReferencedFieldsForType> {
    match doc.get_operation(operation_name.as_deref()).ok() {
        None => HashMap::new(), // todo the existing implementation seems to return the ref fields from the whole document
        Some(operation) => {
            let mut fields_by_type: HashMap<String, HashSet<String>> = HashMap::new();
            let mut fields_by_interface: HashMap<String, bool> = HashMap::new();
            let mut seen_fragments: HashSet<Name> = HashSet::new();

            fn extract_fields(parent_type: &String, selection_set: &apollo_compiler::executable::SelectionSet, doc: &ExecutableDocument, schema: &Valid<apollo_compiler::Schema>, fields_by_type: &mut HashMap<String, HashSet<String>>, fields_by_interface: &mut HashMap<String, bool>, seen_fragments: &mut HashSet<Name>) {
                if !fields_by_interface.contains_key(parent_type) {
                    let field_schema_type = schema.types.get(parent_type.as_str());
                    let is_interface = field_schema_type.is_some_and(|t| t.is_interface());
                    fields_by_interface.insert(parent_type.clone(), is_interface);
                }

                for selection in &selection_set.selections {
                    match selection {
                        apollo_compiler::executable::Selection::Field(field) => {
                            fields_by_type
                                .entry(parent_type.clone())
                                .or_insert(HashSet::new())
                                .insert(field.name.to_string());

                            let field_type = field.selection_set.ty.to_string();
                            extract_fields(
                                &field_type, 
                                &field.selection_set, 
                                doc, 
                                schema, 
                                fields_by_type, 
                                fields_by_interface, 
                                seen_fragments
                            );
                        },
                        apollo_compiler::executable::Selection::InlineFragment(fragment) => {
                            if let Some(fragment_type) = &fragment.type_condition {
                                let frag_type_name = fragment_type.to_string();
                                extract_fields(
                                    &frag_type_name, 
                                    &fragment.selection_set, 
                                    doc, 
                                    schema, 
                                    fields_by_type, 
                                    fields_by_interface, 
                                    seen_fragments
                                );
                            }
                        },
                        apollo_compiler::executable::Selection::FragmentSpread(fragment) => {
                            if !seen_fragments.contains(&fragment.fragment_name) {
                                seen_fragments.insert(fragment.fragment_name.clone());

                                if let Some(fragment) = doc.fragments.get(&fragment.fragment_name) {
                                    let fragment_type = fragment.selection_set.ty.to_string();
                                    extract_fields(
                                        &fragment_type, 
                                        &fragment.selection_set, 
                                        doc, 
                                        schema, 
                                        fields_by_type, 
                                        fields_by_interface, 
                                        seen_fragments
                                    );
                                }
                            }
                        }
                    }
                }
            }

            let operation_type = match operation.operation_type {
                OperationType::Query => "Query",
                OperationType::Mutation => "Mutation",
                OperationType::Subscription => "Subscription",
            };
            extract_fields(&operation_type.into(), &operation.selection_set, doc, schema, &mut fields_by_type, &mut fields_by_interface, &mut seen_fragments);

            fields_by_type.iter()
                .filter_map(|(type_name, field_names)| {
                    if field_names.is_empty() {
                        None
                    } else {
                        let refs = ReferencedFieldsForType {
                            field_names: field_names.iter().cloned().collect(),
                            is_interface: *fields_by_interface.get(type_name).unwrap_or(&false),
                        };

                        Some((type_name.clone(), refs))
                    }
                })
                .collect()
        }
    }
}

enum ApolloReportingSignatureFormatter<'a> {
    Operation(&'a Node<apollo_compiler::executable::Operation>),
    Fragment(&'a Node<apollo_compiler::executable::Fragment>),
    Argument(&'a Node<apollo_compiler::ast::Argument>),
}

impl<'a> fmt::Display for ApolloReportingSignatureFormatter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApolloReportingSignatureFormatter::Operation(operation) => format_operation(operation, f),
            ApolloReportingSignatureFormatter::Fragment(fragment) => format_fragment(fragment, f),
            ApolloReportingSignatureFormatter::Argument(argument) => format_argument(argument, f),
        }
    }
}

fn format_operation<'a>(operation: &Node<apollo_compiler::executable::Operation>, f: &mut fmt::Formatter) -> fmt::Result {
    let shorthand = operation.operation_type == OperationType::Query
        && operation.name.is_none()
        && operation.variables.is_empty()
        && operation.directives.is_empty();

    if !shorthand {
        f.write_str(operation.operation_type.name())?;
        if let Some(name) = &operation.name {
            write!(f, " {}", name)?;
        }

        // print variables sorted by name
        if !operation.variables.is_empty() {
            f.write_str("(")?;
            let mut sorted_variables = operation.variables.clone();
            sorted_variables.sort_by(|a, b| a.name.cmp(&b.name));
            for (index, variable) in sorted_variables.iter().enumerate() {
                if index != 0 {
                    f.write_str(",")?;
                }
                f.write_str(&(variable.to_string()))?;
            }
            f.write_str(")")?;
        }

        // In the JS implementation, only the fragment directives are sorted
        format_directives(&operation.directives, false, f)?;
    }

    format_selection_set(&operation.selection_set, f)
}

fn format_selection_set<'a>(selection_set: &apollo_compiler::executable::SelectionSet, f: &mut fmt::Formatter) -> fmt::Result {
    // print selection set sorted by name with fields followed by named fragments followed by inline fragments
    let mut fields: Vec<&Node<apollo_compiler::executable::Field>> = Vec::new();
    let mut named_fragments: Vec<&Node<apollo_compiler::executable::FragmentSpread>> = Vec::new();
    let mut inline_fragments: Vec<&Node<apollo_compiler::executable::InlineFragment>> = Vec::new();
    for selection in selection_set.selections.iter() {       
        match selection {
            apollo_compiler::executable::Selection::Field(field) => {
                fields.push(field);
            }
            apollo_compiler::executable::Selection::FragmentSpread(fragment_spread) => {
                named_fragments.push(fragment_spread);
            }
            apollo_compiler::executable::Selection::InlineFragment(inline_fragment) => {
                inline_fragments.push(inline_fragment);
            }
        }
    }

    if !fields.is_empty() || !named_fragments.is_empty() || !inline_fragments.is_empty() {
        fields.sort_by(|&a, &b| a.name.cmp(&b.name));
        named_fragments.sort_by(|&a, &b| a.fragment_name.cmp(&b.fragment_name));
        // Note that inline fragments are not sorted in the JS implementation

        f.write_str("{ ")?;

        for &field in fields.iter() {
            f.write_str(" ")?;
            format_field(&field, f)?;
        }

        for &frag in named_fragments.iter() {
            f.write_str(" ")?;
            format_fragment_spread(&frag, f)?;
        }

        for &frag in inline_fragments.iter() {
            f.write_str(" ")?;
            format_inline_fragment(&frag, f)?;
        }

        f.write_str(" } ")?;
    }

    Ok(())
}

fn format_argument<'a>(arg: &Node<apollo_compiler::ast::Argument>, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}:", arg.name.to_string())?;
    format_value(&arg.value, f)
}

fn format_field<'a>(field: &Node<apollo_compiler::executable::Field>, f: &mut fmt::Formatter) -> fmt::Result {
    f.write_str(&field.name)?;
    
    let mut sorted_args = field.arguments.clone();
    if !sorted_args.is_empty() {
        sorted_args.sort_by(|a, b| a.name.cmp(&b.name));

        f.write_str("( ")?;

        // The graphql-js implementation will use newlines and indentation instead of commas if the length of the "arg line" is 
        // over 80 characters. This "arg line" includes the alias followed by ": " if the field has an alias (which is never 
        // the case for now), followed by all argument names and values separated by ": ", surrounded with brackets. Our usage
        // reporting plugin replaces all newlines + indentation with a single space, so we have to replace commas with spaces if 
        // the line length is too long.m
        // When we update this to allow aliases to remain, we can choose to just always use commas, since we won't have to preserve
        // the same operation ID as it will already be changing with the included alias.
        let arg_strings: Vec<String> = sorted_args.iter()
            .map(|a| ApolloReportingSignatureFormatter::Argument(a).to_string())
            .collect();
        let original_line_length = 2 + arg_strings.iter().map(|s| s.len()).sum::<usize>() + (arg_strings.len() * 2);
        let separator = if original_line_length > 80 { " " } else { "," };

        for (index, arg_string) in arg_strings.iter().enumerate() {
            if index != 0 {
                f.write_str(separator)?;
            }
            f.write_str(arg_string)?;
        }
        f.write_str(") ")?;
    }
    
    // In the JS implementation, only the fragment directives are sorted
    format_directives(&field.directives, false, f)?;
    format_selection_set(&field.selection_set, f)
}

fn format_fragment_spread<'a>(fragment_spread: &Node<apollo_compiler::executable::FragmentSpread>, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "...{}", fragment_spread.fragment_name.to_string())?;
    format_directives(&fragment_spread.directives, true, f)
}

fn format_inline_fragment<'a>(inline_fragment: &Node<apollo_compiler::executable::InlineFragment>, f: &mut fmt::Formatter) -> fmt::Result {
    if let Some(type_name) = &inline_fragment.type_condition {
        write!(f, "... on {} ", type_name.to_string())?;
    } else {
        f.write_str("... ")?;
    }

    format_directives(&inline_fragment.directives, true, f)?;
    format_selection_set(&inline_fragment.selection_set, f)

}

fn format_fragment<'a>(fragment: &Node<apollo_compiler::executable::Fragment>, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "fragment {} on {}", &fragment.name.to_string(), &fragment.selection_set.ty.to_string())?;
    format_directives(&fragment.directives, true, f)?;
    format_selection_set(&fragment.selection_set, f)
}

fn format_directives<'a>(directives: &apollo_compiler::executable::DirectiveList, sorted: bool, f: &mut fmt::Formatter) -> fmt::Result {
    let mut sorted_directives = directives.clone();
    if sorted {
        sorted_directives.sort_by(|a, b| a.name.cmp(&b.name));
    }

    for directive in sorted_directives.iter() {
        write!(f, "@ {}", directive.name.to_string())?;

        let mut sorted_args = directive.arguments.clone();
        if !sorted_args.is_empty() {
            sorted_args.sort_by(|a, b| a.name.cmp(&b.name));
    
            f.write_str("(")?;

            for (index, argument) in sorted_args.iter().enumerate() {
                if index != 0 {
                    f.write_str(",")?;
                }
                f.write_str(&ApolloReportingSignatureFormatter::Argument(argument).to_string())?;
            }

            f.write_str(")")?;
        }

        f.write_str(" ")?;
    }

    Ok(())
}

fn format_value<'a>(value: &apollo_compiler::ast::Value, f: &mut fmt::Formatter) -> fmt::Result {
    use apollo_compiler::ast::Value; // todo do this more often
    match value {
        Value::String(_) => {
            f.write_str("\"\"")
        }
        Value::Float(_) | Value::Int(_) => {
            f.write_str("0")
        }
        Value::Object(_) => {
            f.write_str("{}")
        }
        Value::List(_) => {
            f.write_str("[]")
        }
        rest => {
            f.write_str(&rest.to_string())
        }
    }
}

// The JS implementation builds up a set of field names and converts it to an array using the spread operator, so we can't
// rely on any specific ordering. Before comparing the referenced fields we re-order the type vectors.
fn order_ref_fields_by_type(original: &HashMap<String, ReferencedFieldsForType>) -> HashMap<String, ReferencedFieldsForType> {
    original.iter()
        .map(|(parent_type, ref_fields)| {
            let mut ref_fields_sorted = ref_fields.clone();
            ref_fields_sorted.field_names.sort();
            (parent_type.clone(), ref_fields_sorted)
        })
        .collect()
}

pub fn compare_ref_fields_by_type(left: &HashMap<String, ReferencedFieldsForType>, right: &HashMap<String, ReferencedFieldsForType>) -> bool {
    return order_ref_fields_by_type(left) == order_ref_fields_by_type(right);
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use test_log::test;

    use super::*;

    // todo more/better tests

    #[test(tokio::test)]
    async fn test_sig_and_ref_generation_1() {
        let schema_str = r#"type BasicTypesResponse {
            nullableId: ID
            nonNullId: ID!
            nullableInt: Int
            nonNullInt: Int!
            nullableString: String
            nonNullString: String!
            nullableFloat: Float
            nonNullFloat: Float!
            nullableBoolean: Boolean
            nonNullBoolean: Boolean!
          }
          
          enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }
          
          interface AnInterface {
            sharedField: String!
          }
          
          type InterfaceImplementation1 implements AnInterface {
            sharedField: String!
            implementation1Field: Int!
          }
          
          type InterfaceImplementation2 implements AnInterface {
            sharedField: String!
            implementation2Field: Float!
          }
          
          type UnionType1 {
            unionType1Field: String!
            nullableString: String
          }
          
          type UnionType2 {
            unionType2Field: String!
            nullableString: String
          }
          
          union UnionType = UnionType1 | UnionType2
          
          type ObjectTypeResponse {
            stringField: String!
            intField: Int!
            nullableField: String
          }
          
          input NestedInputType {
            someFloat: Float!
            someNullableFloat: Float
          }
          
          input InputType {
            inputString: String!
            inputInt: Int!
            inputBoolean: Boolean
            nestedType: NestedInputType!
            enumInput: SomeEnum
            listInput: [Int!]!
            nestedTypeList: [NestedInputType]
          }
          
          input NestedEnumInputType {
            someEnum: SomeEnum
          }
          
          input AnotherInputType {
            anotherInput: ID!
          }
          
          input InputTypeWithDefault {
            nonNullId: ID!
            nonNullIdWithDefault: ID! = "id"
            nullableId: ID
            nullableIdWithDefault: ID = "id"
          }
          
          input EnumInputType {
            enumInput: SomeEnum!
            enumListInput: [SomeEnum!]!
            nestedEnumType: [NestedEnumInputType]
          }
          
          type EverythingResponse {
            basicTypes: BasicTypesResponse
            enumResponse: SomeEnum
            interfaceResponse: AnInterface
            interfaceImplementationResponse: InterfaceImplementation2
            unionResponse: UnionType
            unionType2Response: UnionType2
            listOfBools: [Boolean!]!
            listOfInterfaces: [AnInterface]
            listOfUnions: [UnionType]
            objectTypeWithInputField(boolInput: Boolean, secondInput: Boolean!): ObjectTypeResponse
            listOfObjects: [ObjectTypeResponse]
          }
          
          type BasicResponse {
            id: Int!
            nullableId: Int
          }
          
          type Query {
            inputTypeQuery(input: InputType!): EverythingResponse!
            scalarInputQuery(
              listInput: [String!]!, 
              stringInput: String!, 
              nullableStringInput: String, 
              intInput: Int!, 
              floatInput: Float!, 
              boolInput: Boolean!, 
              enumInput: SomeEnum,
              idInput: ID!
            ): EverythingResponse!
            noInputQuery: EverythingResponse!
            basicInputTypeQuery(input: NestedInputType!): EverythingResponse!
            anotherInputTypeQuery(input: AnotherInputType): EverythingResponse!
            enumInputQuery(enumInput: SomeEnum, inputType: EnumInputType): EverythingResponse!
            basicResponseQuery: BasicResponse!
            scalarResponseQuery: String
            defaultArgQuery(stringInput: String! = "default", inputType: AnotherInputType = { anotherInput: "inputDefault" }): BasicResponse!
            inputTypehDefaultQuery(input: InputTypeWithDefault): BasicResponse!
          }"#;

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

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("TransformedQuery".into()));
        let expected_sig = "# TransformedQuery\nfragment Fragment1 on EverythingResponse{basicTypes{nonNullFloat}}fragment Fragment2 on EverythingResponse{basicTypes{nullableFloat}}query TransformedQuery{scalarInputQuery(boolInput:true floatInput:0 idInput:\"\"intInput:0 listInput:[]stringInput:\"\")@skip(if:false)@include(if:true){enumResponse interfaceResponse{sharedField...on InterfaceImplementation2{implementation2Field}...on InterfaceImplementation1{implementation1Field}}objectTypeWithInputField(boolInput:true,secondInput:false){__typename intField stringField}...Fragment1...Fragment2}}";
        assert_eq!(expected_sig, generated_sig);

        let generated_refs = generate_apollo_reporting_refs(&doc, Some("TransformedQuery".into()), &schema);
        let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["scalarInputQuery".into()],
                is_interface: false,
            }),
            ("BasicTypesResponse".into(), ReferencedFieldsForType {
                field_names: vec!["nullableFloat".into(), "nonNullFloat".into()],
                is_interface: false,
            }),
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec![
                    "basicTypes".into(), 
                    "objectTypeWithInputField".into(),
                    "enumResponse".into(),
                    "interfaceResponse".into(),
                ],
                is_interface: false,
            }),
            ("AnInterface".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into()],
                is_interface: true,
            }),
            ("ObjectTypeResponse".into(), ReferencedFieldsForType {
                field_names: vec!["stringField".into(), "__typename".into(), "intField".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation2".into(), ReferencedFieldsForType {
                field_names: vec!["implementation2Field".into()],
                is_interface: false,
            }),
        ]);
        assert!(compare_ref_fields_by_type(&expected_refs, &generated_refs));
    }

    #[test(tokio::test)]
    async fn test_sig_and_ref_generation_2() {
        let schema_str = r#"type BasicTypesResponse {
            nullableId: ID
            nonNullId: ID!
            nullableInt: Int
            nonNullInt: Int!
            nullableString: String
            nonNullString: String!
            nullableFloat: Float
            nonNullFloat: Float!
            nullableBoolean: Boolean
            nonNullBoolean: Boolean!
          }
          
          enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }
          
          interface AnInterface {
            sharedField: String!
          }
          
          type InterfaceImplementation1 implements AnInterface {
            sharedField: String!
            implementation1Field: Int!
          }
          
          type InterfaceImplementation2 implements AnInterface {
            sharedField: String!
            implementation2Field: Float!
          }
          
          type UnionType1 {
            unionType1Field: String!
            nullableString: String
          }
          
          type UnionType2 {
            unionType2Field: String!
            nullableString: String
          }
          
          union UnionType = UnionType1 | UnionType2
          
          type ObjectTypeResponse {
            stringField: String!
            intField: Int!
            nullableField: String
          }
          
          input NestedInputType {
            someFloat: Float!
            someNullableFloat: Float
          }
          
          input InputType {
            inputString: String!
            inputInt: Int!
            inputBoolean: Boolean
            nestedType: NestedInputType!
            enumInput: SomeEnum
            listInput: [Int!]!
            nestedTypeList: [NestedInputType]
          }
          
          input NestedEnumInputType {
            someEnum: SomeEnum
          }
          
          input AnotherInputType {
            anotherInput: ID!
          }
          
          input InputTypeWithDefault {
            nonNullId: ID!
            nonNullIdWithDefault: ID! = "id"
            nullableId: ID
            nullableIdWithDefault: ID = "id"
          }
          
          input EnumInputType {
            enumInput: SomeEnum!
            enumListInput: [SomeEnum!]!
            nestedEnumType: [NestedEnumInputType]
          }
          
          type EverythingResponse {
            basicTypes: BasicTypesResponse
            enumResponse: SomeEnum
            interfaceResponse: AnInterface
            interfaceImplementationResponse: InterfaceImplementation2
            unionResponse: UnionType
            unionType2Response: UnionType2
            listOfBools: [Boolean!]!
            listOfInterfaces: [AnInterface]
            listOfUnions: [UnionType]
            objectTypeWithInputField(boolInput: Boolean, secondInput: Boolean!): ObjectTypeResponse
            listOfObjects: [ObjectTypeResponse]
          }
          
          type BasicResponse {
            id: Int!
            nullableId: Int
          }
          
          type Query {
            inputTypeQuery(input: InputType!): EverythingResponse!
            scalarInputQuery(
              listInput: [String!]!, 
              stringInput: String!, 
              nullableStringInput: String, 
              intInput: Int!, 
              floatInput: Float!, 
              boolInput: Boolean!, 
              enumInput: SomeEnum,
              idInput: ID!
            ): EverythingResponse!
            noInputQuery: EverythingResponse!
            basicInputTypeQuery(input: NestedInputType!): EverythingResponse!
            anotherInputTypeQuery(input: AnotherInputType): EverythingResponse!
            enumInputQuery(enumInput: SomeEnum, inputType: EnumInputType): EverythingResponse!
            basicResponseQuery: BasicResponse!
            scalarResponseQuery: String
            defaultArgQuery(stringInput: String! = "default", inputType: AnotherInputType = { anotherInput: "inputDefault" }): BasicResponse!
            inputTypehDefaultQuery(input: InputTypeWithDefault): BasicResponse!
          }"#;

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

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("Query".into()));
        let expected_sig = "# Query\nquery Query($secondInput:Boolean!){basicInputTypeQuery(input:{}){listOfObjects{stringField}unionResponse{...on UnionType1{nullableString}}unionType2Response{unionType2Field}}noInputQuery{basicTypes{nonNullId nonNullInt}enumResponse interfaceImplementationResponse{implementation2Field sharedField}interfaceResponse{...on InterfaceImplementation1{implementation1Field sharedField}...on InterfaceImplementation2{implementation2Field sharedField}}listOfUnions{...on UnionType1{nullableString}}objectTypeWithInputField(secondInput:$secondInput){intField}}scalarResponseQuery}";
        assert_eq!(expected_sig, generated_sig);

        let generated_refs = generate_apollo_reporting_refs(&doc, Some("Query".into()), &schema);
        let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["scalarResponseQuery".into(), "noInputQuery".into(), "basicInputTypeQuery".into()],
                is_interface: false,
            }),
            ("BasicTypesResponse".into(), ReferencedFieldsForType {
                field_names: vec!["nonNullId".into(), "nonNullInt".into()],
                is_interface: false,
            }),
            ("ObjectTypeResponse".into(), ReferencedFieldsForType {
                field_names: vec!["intField".into(), "stringField".into()],
                is_interface: false,
            }),
            ("UnionType2".into(), ReferencedFieldsForType {
                field_names: vec!["unionType2Field".into()],
                is_interface: false,
            }),
            ("EverythingResponse".into(), ReferencedFieldsForType {
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
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            }),
            ("UnionType1".into(), ReferencedFieldsForType {
                field_names: vec!["nullableString".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation2".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            }),
        ]);
        assert!(compare_ref_fields_by_type(&expected_refs, &generated_refs));
    }

    #[test(tokio::test)]
    async fn test_sig_basic_regex() {
        let schema_str = r#"type BasicResponse {
            id: Int!
            nullableId: Int
          }

          type Query {
            noInputQuery: BasicResponse!
          }"#;

        let query_str = r#"query MyQuery {
            noInputQuery {
              id
            }
          }"#;

        let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
        let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("MyQuery".into()));
        let expected_sig = "# MyQuery\nquery MyQuery{noInputQuery{id}}";
        assert_eq!(expected_sig, generated_sig);
    }

    #[test(tokio::test)]
    async fn test_sig_anonymous_query() {
        let schema_str = r#"type BasicResponse {
            id: Int!
            nullableId: Int
          }
          
          type Query {
            noInputQuery: BasicResponse!
          }"#;

        let query_str = r#"query {
            noInputQuery {
              id
            }
          }"#;


        let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
        let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

        let generated_sig = generate_apollo_reporting_signature(&doc, None);
        let expected_sig = "# -\n{noInputQuery{id}}";
        assert_eq!(expected_sig, generated_sig);
    }

    #[test(tokio::test)]
    async fn test_sig_anonymous_mutation() {
        let schema_str = r#"type BasicResponse {
            id: Int!
            nullableId: Int
          }

          type Query {}
          
          type Mutation {
            noInputMutation: BasicResponse!
          }"#;

        let query_str = r#"mutation {
            noInputMutation {
              id
            }
          }"#;


        let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
        let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

        let generated_sig = generate_apollo_reporting_signature(&doc, None);
        let expected_sig = "# -\nmutation{noInputMutation{id}}";
        assert_eq!(expected_sig, generated_sig);
    }

    #[test(tokio::test)]
    async fn test_sig_anonymous_subscription() {
        let schema_str = r#"type BasicResponse {
            id: Int!
            nullableId: Int
          }

          type Query {}
          
          type Subscription {
            noInputSubscription: BasicResponse!
          }"#;

        let query_str: &str = r#"subscription {
            noInputSubscription {
              id
            }
          }"#;

        let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
        let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

        let generated_sig = generate_apollo_reporting_signature(&doc, None);
        let expected_sig = "# -\nsubscription{noInputSubscription{id}}";
        assert_eq!(expected_sig, generated_sig);
    }

    #[test(tokio::test)]
    async fn test_sig_ordered_fields_and_variables() {
        let schema_str = r#"type BasicResponse {
            id: Int!
            nullableId: Int
            zzz: Int
            aaa: Int
            CCC: Int
          }
          
          type Query {
            scalarInputQuery(
                listInput: [String!]!, 
                stringInput: String!, 
                nullableStringInput: String, 
                intInput: Int!, 
                floatInput: Float!, 
                boolInput: Boolean!,
                idInput: ID!
              ): BasicResponse!
          }"#;

        let query_str = r#"query VariableScalarInputQuery($idInput: ID!, $boolInput: Boolean!, $floatInput: Float!, $intInput: Int!, $listInput: [String!]!, $stringInput: String!, $nullableStringInput: String) {
            scalarInputQuery(
              idInput: $idInput
              boolInput: $boolInput
              floatInput: $floatInput
              intInput: $intInput
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

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("VariableScalarInputQuery".into()));
        let expected_sig = "# VariableScalarInputQuery\nquery VariableScalarInputQuery($boolInput:Boolean!,$floatInput:Float!,$idInput:ID!,$intInput:Int!,$listInput:[String!]!,$nullableStringInput:String,$stringInput:String!){scalarInputQuery(boolInput:$boolInput floatInput:$floatInput idInput:$idInput intInput:$intInput listInput:$listInput nullableStringInput:$nullableStringInput stringInput:$stringInput){CCC aaa id nullableId zzz}}";
        assert_eq!(expected_sig, generated_sig);
    }


    #[test(tokio::test)]
    async fn test_sig_and_ref_with_fragments() {
        let schema_str = r#"interface AnInterface {
            sharedField: String!
          }
          
          type InterfaceImplementation1 implements AnInterface {
            sharedField: String!
            implementation1Field: Int!
          }
          
          type InterfaceImplementation2 implements AnInterface {
            sharedField: String!
            implementation2Field: Float!
          }
          
          type UnionType1 {
            unionType1Field: String!
            nullableString: String
          }
          
          type UnionType2 {
            unionType2Field: String!
            nullableString: String
          }
          
          union UnionType = UnionType1 | UnionType2
        
          type EverythingResponse {
            interfaceResponse: AnInterface
            listOfInterfaces: [AnInterface]
            unionResponse: UnionType
            listOfBools: [Boolean!]!
          }
          
          type Query {
            noInputQuery: EverythingResponse!
          }"#;

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

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("FragmentQuery".into()));
        let expected_sig = "# FragmentQuery\nfragment ZZZFragment on EverythingResponse{listOfInterfaces{sharedField}}fragment aaaFragment on EverythingResponse{listOfInterfaces{sharedField}}fragment aaaInterfaceFragment on InterfaceImplementation1{sharedField}fragment bbbInterfaceFragment on InterfaceImplementation2{implementation2Field sharedField}fragment zzzFragment on EverythingResponse{listOfInterfaces{sharedField}}query FragmentQuery{noInputQuery{interfaceResponse{sharedField...aaaInterfaceFragment...bbbInterfaceFragment...on InterfaceImplementation2{implementation2Field}...{...on InterfaceImplementation1{implementation1Field}}...on InterfaceImplementation1{implementation1Field}}listOfBools unionResponse{...on UnionType2{unionType2Field}...on UnionType1{unionType1Field}}...ZZZFragment...aaaFragment...zzzFragment}}";
        assert_eq!(expected_sig, generated_sig);

        let generated_refs = generate_apollo_reporting_refs(&doc, Some("FragmentQuery".into()), &schema);
        let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
            ("UnionType1".into(), ReferencedFieldsForType {
                field_names: vec!["unionType1Field".into()],
                is_interface: false,
            }),
            ("UnionType2".into(), ReferencedFieldsForType {
                field_names: vec!["unionType2Field".into()],
                is_interface: false,
            }),
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            }),
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec!["listOfInterfaces".into(), "listOfBools".into(), "interfaceResponse".into(), "unionResponse".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation1Field".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            }),
            ("AnInterface".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into()],
                is_interface: true,
            }),
            ("InterfaceImplementation2".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            }),
        ]);
        assert!(compare_ref_fields_by_type(&expected_refs, &generated_refs));
    }

    #[test(tokio::test)]
    async fn test_sig_and_ref_with_directives() {
        let schema_str = r#"directive @withArgs(
            arg1: String = "Default"
            arg2: String
            arg3: Boolean
            arg4: Int
            arg5: [ID]
          ) on QUERY | MUTATION | SUBSCRIPTION | FIELD | FRAGMENT_DEFINITION | FRAGMENT_SPREAD | INLINE_FRAGMENT
          directive @noArgs on QUERY | MUTATION | SUBSCRIPTION | FIELD | FRAGMENT_DEFINITION | FRAGMENT_SPREAD | INLINE_FRAGMENT
          
          enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }

          interface AnInterface {
            sharedField: String!
          }
          
          type InterfaceImplementation1 implements AnInterface {
            sharedField: String!
            implementation1Field: Int!
          }
          
          type InterfaceImplementation2 implements AnInterface {
            sharedField: String!
            implementation2Field: Float!
          }
          
          type UnionType1 {
            unionType1Field: String!
            nullableString: String
          }
          
          type UnionType2 {
            unionType2Field: String!
            nullableString: String
          }
          
          union UnionType = UnionType1 | UnionType2
        
          type EverythingResponse {
            enumResponse: SomeEnum
            interfaceResponse: AnInterface
            unionResponse: UnionType
          }
          
          type Query {
            noInputQuery: EverythingResponse!
          }"#;

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
                ... Fragment1 @withArgs(arg2: "" arg1: "test") @noArgs
                ... Fragment2
              }
            }
          }"#;

        let schema = Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
        let doc = ExecutableDocument::parse(&schema, query_str, "query.graphql").unwrap();

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("DirectiveQuery".into()));
        let expected_sig = "# DirectiveQuery\nfragment Fragment1 on InterfaceImplementation1{implementation1Field sharedField}fragment Fragment2 on InterfaceImplementation2@noArgs@withArgs(arg1:\"\",arg2:\"\",arg3:true,arg4:0,arg5:[]){implementation2Field sharedField}query DirectiveQuery@withArgs(arg1:\"\",arg2:\"\")@noArgs{noInputQuery{enumResponse@withArgs(arg3:false,arg4:0,arg5:[])@noArgs interfaceResponse{...Fragment1@noArgs@withArgs(arg1:\"\",arg2:\"\")...Fragment2}unionResponse{...on UnionType1@noArgs@withArgs(arg1:\"\",arg2:\"\"){unionType1Field}}}}";
        assert_eq!(expected_sig, generated_sig);

        let generated_refs = generate_apollo_reporting_refs(&doc, Some("DirectiveQuery".into()), &schema);
        let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
            ("UnionType1".into(), ReferencedFieldsForType {
                field_names: vec!["unionType1Field".into()],
                is_interface: false,
            }),
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["noInputQuery".into()],
                is_interface: false,
            }),
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec!["enumResponse".into(), "interfaceResponse".into(), "unionResponse".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation1Field".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation1".into(), ReferencedFieldsForType {
                field_names: vec!["implementation1Field".into(), "sharedField".into()],
                is_interface: false,
            }),
            ("InterfaceImplementation2".into(), ReferencedFieldsForType {
                field_names: vec!["sharedField".into(), "implementation2Field".into()],
                is_interface: false,
            }),
        ]);
        assert!(compare_ref_fields_by_type(&expected_refs, &generated_refs));
    }

    #[test(tokio::test)]
    async fn test_sig_and_ref_with_aliases() {
        let schema_str = r#"enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }
        
          type EverythingResponse {
            enumResponse: SomeEnum
          }
          
          type Query {
            enumInputQuery(enumInput: SomeEnum): EverythingResponse!
          }"#;

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

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("AliasQuery".into()));
        let expected_sig = "# AliasQuery\nquery AliasQuery{enumInputQuery(enumInput:SOME_VALUE_1){enumResponse}enumInputQuery(enumInput:SOME_VALUE_2){enumResponse}enumInputQuery(enumInput:SOME_VALUE_3){enumResponse}}";
        assert_eq!(expected_sig, generated_sig);

        let generated_refs = generate_apollo_reporting_refs(&doc, Some("AliasQuery".into()), &schema);
        let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            }),
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["enumInputQuery".into()],
                is_interface: false,
            }),
        ]);
        assert!(compare_ref_fields_by_type(&expected_refs, &generated_refs));
    }

    #[test(tokio::test)]
    async fn test_sig_and_ref_with_inline_values() {
        let schema_str = r#"enum SomeEnum {
            SOME_VALUE_1
            SOME_VALUE_2
            SOME_VALUE_3
          }
          
          input InputType {
            inputString: String!
            inputInt: Int!
            inputBoolean: Boolean
            nestedType: NestedInputType!
            enumInput: SomeEnum
            listInput: [Int!]!
            nestedTypeList: [NestedInputType]
          }
          
          input NestedInputType {
            someFloat: Float!
            someNullableFloat: Float
          }
        
          type EverythingResponse {
            enumResponse: SomeEnum
          }
          
          type Query {
            inputTypeQuery(input: InputType!): EverythingResponse!
          }"#;

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

        let generated_sig = generate_apollo_reporting_signature(&doc, Some("InlineInputTypeQuery".into()));
        let expected_sig = "# InlineInputTypeQuery\nquery InlineInputTypeQuery{inputTypeQuery(input:{}){enumResponse}}";
        assert_eq!(expected_sig, generated_sig);

        let generated_refs = generate_apollo_reporting_refs(&doc, Some("InlineInputTypeQuery".into()), &schema);
        let expected_refs: HashMap<String, ReferencedFieldsForType> = HashMap::from([
            ("EverythingResponse".into(), ReferencedFieldsForType {
                field_names: vec!["enumResponse".into()],
                is_interface: false,
            }),
            ("Query".into(), ReferencedFieldsForType {
                field_names: vec!["inputTypeQuery".into()],
                is_interface: false,
            }),
        ]);
        assert!(compare_ref_fields_by_type(&expected_refs, &generated_refs));
    }
}
