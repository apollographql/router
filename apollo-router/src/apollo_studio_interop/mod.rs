//! Generation of usage reporting fields
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::DirectiveList;
use apollo_compiler::ast::Name;
use apollo_compiler::ast::OperationType;
use apollo_compiler::ast::Value;
use apollo_compiler::ast::VariableDefinition;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Fragment;
use apollo_compiler::executable::FragmentSpread;
use apollo_compiler::executable::InlineFragment;
use apollo_compiler::executable::Operation;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use router_bridge::planner::ReferencedFieldsForType;
use router_bridge::planner::UsageReporting;

/// The result of the generate_usage_reporting function which contains a UsageReporting struct and
/// functions that allow comparison with another ComparableUsageReporting or UsageReporting object.
pub(crate) struct ComparableUsageReporting {
    /// The UsageReporting fields
    pub(crate) result: UsageReporting,
}

/// Enum specifying the result of a comparison.
pub(crate) enum UsageReportingComparisonResult {
    /// The UsageReporting instances are the same
    Equal,
    /// The stats_report_key in the UsageReporting instances are different
    StatsReportKeyNotEqual,
    /// The referenced_fields in the UsageReporting instances are different. When comparing referenced
    /// fields, we ignore the ordering of field names.
    ReferencedFieldsNotEqual,
    /// Both the stats_report_key and referenced_fields in the UsageReporting instances are different.
    BothNotEqual,
}

impl ComparableUsageReporting {
    /// Compare this to another UsageReporting.
    pub(crate) fn compare(&self, other: &UsageReporting) -> UsageReportingComparisonResult {
        let sig_equal = self.result.stats_report_key == other.stats_report_key;
        let refs_equal = self.compare_referenced_fields(&other.referenced_fields_by_type);
        match (sig_equal, refs_equal) {
            (true, true) => UsageReportingComparisonResult::Equal,
            (false, true) => UsageReportingComparisonResult::StatsReportKeyNotEqual,
            (true, false) => UsageReportingComparisonResult::ReferencedFieldsNotEqual,
            (false, false) => UsageReportingComparisonResult::BothNotEqual,
        }
    }

    fn compare_referenced_fields(
        &self,
        other_ref_fields: &HashMap<String, ReferencedFieldsForType>,
    ) -> bool {
        let self_ref_fields = &self.result.referenced_fields_by_type;
        if self_ref_fields.len() != other_ref_fields.len() {
            return false;
        }

        for (name, self_refs) in self_ref_fields.iter() {
            let maybe_other_refs = other_ref_fields.get(name);
            if let Some(other_refs) = maybe_other_refs {
                if self_refs.is_interface != other_refs.is_interface {
                    return false;
                }

                let self_field_names_set: HashSet<_> =
                    self_refs.field_names.clone().into_iter().collect();
                let other_field_names_set: HashSet<_> =
                    other_refs.field_names.clone().into_iter().collect();
                if self_field_names_set != other_field_names_set {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

/// Generate a ComparableUsageReporting containing the stats_report_key (a normalized version of the operation signature)
/// and referenced fields of an operation. The document used to generate the signature and for the references can be
/// different to handle cases where the operation has been filtered, but we want to keep the same signature.
pub(crate) fn generate_usage_reporting(
    signature_doc: &ExecutableDocument,
    references_doc: &ExecutableDocument,
    operation_name: &Option<String>,
    schema: &Valid<Schema>,
) -> ComparableUsageReporting {
    let mut generator = UsageReportingGenerator {
        signature_doc,
        references_doc,
        operation_name,
        schema,
        fragments_map: HashMap::new(),
        fields_by_type: HashMap::new(),
        fields_by_interface: HashMap::new(),
        fragment_spread_set: HashSet::new(),
    };

    generator.generate()
}

struct UsageReportingGenerator<'a> {
    signature_doc: &'a ExecutableDocument,
    references_doc: &'a ExecutableDocument,
    operation_name: &'a Option<String>,
    schema: &'a Valid<Schema>,
    fragments_map: HashMap<String, Node<Fragment>>,
    fields_by_type: HashMap<String, HashSet<String>>,
    fields_by_interface: HashMap<String, bool>,
    fragment_spread_set: HashSet<Name>,
}

impl UsageReportingGenerator<'_> {
    fn generate(&mut self) -> ComparableUsageReporting {
        ComparableUsageReporting {
            result: UsageReporting {
                stats_report_key: self.generate_stats_report_key(),
                referenced_fields_by_type: self.generate_apollo_reporting_refs(),
            },
        }
    }

    fn generate_stats_report_key(&mut self) -> String {
        self.fragments_map.clear();

        match self
            .signature_doc
            .get_operation(self.operation_name.as_deref())
            .ok()
        {
            None => "".to_string(),
            Some(operation) => {
                self.extract_signature_fragments(&operation.selection_set);
                self.format_operation_for_report(operation)
            }
        }
    }

    fn extract_signature_fragments(&mut self, selection_set: &SelectionSet) {
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.extract_signature_fragments(&field.selection_set);
                }
                Selection::InlineFragment(fragment) => {
                    self.extract_signature_fragments(&fragment.selection_set);
                }
                Selection::FragmentSpread(fragment_node) => {
                    let fragment_name = fragment_node.fragment_name.to_string();
                    if let Entry::Vacant(e) = self.fragments_map.entry(fragment_name) {
                        if let Some(fragment) = self
                            .signature_doc
                            .fragments
                            .get(&fragment_node.fragment_name)
                        {
                            e.insert(fragment.clone());
                        }
                    }
                }
            }
        }
    }

    fn format_operation_for_report(&self, operation: &Node<Operation>) -> String {
        // The result in the name of the operation
        let op_name = match &operation.name {
            None => "-".into(),
            Some(node) => node.to_string(),
        };
        let mut result = format!("# {}\n", op_name);

        // Followed by a sorted list of fragments
        let mut sorted_fragments: Vec<_> = self.fragments_map.iter().collect();
        sorted_fragments.sort_by_key(|&(k, _)| k);

        sorted_fragments.into_iter().for_each(|(_, f)| {
            result.push_str(&ApolloReportingSignatureFormatter::Fragment(f).to_string())
        });

        // Followed by the operation
        result.push_str(&ApolloReportingSignatureFormatter::Operation(operation).to_string());

        result
    }

    fn generate_apollo_reporting_refs(&mut self) -> HashMap<String, ReferencedFieldsForType> {
        self.fragments_map.clear();
        self.fields_by_type.clear();
        self.fields_by_interface.clear();

        match self
            .references_doc
            .get_operation(self.operation_name.as_deref())
            .ok()
        {
            None => HashMap::new(),
            Some(operation) => {
                let operation_type = match operation.operation_type {
                    OperationType::Query => "Query",
                    OperationType::Mutation => "Mutation",
                    OperationType::Subscription => "Subscription",
                };
                self.extract_fields(&operation_type.into(), &operation.selection_set);

                self.fields_by_type
                    .iter()
                    .filter_map(|(type_name, field_names)| {
                        if field_names.is_empty() {
                            None
                        } else {
                            let refs = ReferencedFieldsForType {
                                field_names: field_names.iter().cloned().collect(),
                                is_interface: *self
                                    .fields_by_interface
                                    .get(type_name)
                                    .unwrap_or(&false),
                            };

                            Some((type_name.clone(), refs))
                        }
                    })
                    .collect()
            }
        }
    }

    fn extract_fields(&mut self, parent_type: &String, selection_set: &SelectionSet) {
        if !self.fields_by_interface.contains_key(parent_type) {
            let field_schema_type = self.schema.types.get(parent_type.as_str());
            let is_interface = field_schema_type.is_some_and(|t| t.is_interface());
            self.fields_by_interface
                .insert(parent_type.clone(), is_interface);
        }

        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.fields_by_type
                        .entry(parent_type.clone())
                        .or_default()
                        .insert(field.name.to_string());

                    let field_type = field.selection_set.ty.to_string();
                    self.extract_fields(&field_type, &field.selection_set);
                }
                Selection::InlineFragment(fragment) => {
                    let frag_type_name = match fragment.type_condition.clone() {
                        Some(fragment_type) => fragment_type.to_string(),
                        None => parent_type.clone(),
                    };
                    self.extract_fields(&frag_type_name, &fragment.selection_set);
                }
                Selection::FragmentSpread(fragment) => {
                    if !self.fragment_spread_set.contains(&fragment.fragment_name) {
                        self.fragment_spread_set
                            .insert(fragment.fragment_name.clone());

                        if let Some(fragment) =
                            self.references_doc.fragments.get(&fragment.fragment_name)
                        {
                            let fragment_type = fragment.selection_set.ty.to_string();
                            self.extract_fields(&fragment_type, &fragment.selection_set);
                        }
                    }
                }
            }
        }
    }
}

enum ApolloReportingSignatureFormatter<'a> {
    Operation(&'a Node<Operation>),
    Fragment(&'a Node<Fragment>),
    Argument(&'a Node<Argument>),
    Field(&'a Node<Field>),
}

impl<'a> fmt::Display for ApolloReportingSignatureFormatter<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ApolloReportingSignatureFormatter::Operation(operation) => {
                format_operation(operation, f)
            }
            ApolloReportingSignatureFormatter::Fragment(fragment) => format_fragment(fragment, f),
            ApolloReportingSignatureFormatter::Argument(argument) => format_argument(argument, f),
            ApolloReportingSignatureFormatter::Field(field) => format_field(field, f),
        }
    }
}

fn format_operation(operation: &Node<Operation>, f: &mut fmt::Formatter) -> fmt::Result {
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
                format_variable(variable, f)?;
            }
            f.write_str(")")?;
        }

        // In the JS implementation, only the fragment directives are sorted
        format_directives(&operation.directives, false, f)?;
    }

    format_selection_set(&operation.selection_set, f)
}

fn format_selection_set(selection_set: &SelectionSet, f: &mut fmt::Formatter) -> fmt::Result {
    // print selection set sorted by name with fields followed by named fragments followed by inline fragments
    let mut fields: Vec<&Node<Field>> = Vec::new();
    let mut named_fragments: Vec<&Node<FragmentSpread>> = Vec::new();
    let mut inline_fragments: Vec<&Node<InlineFragment>> = Vec::new();
    for selection in selection_set.selections.iter() {
        match selection {
            Selection::Field(field) => {
                fields.push(field);
            }
            Selection::FragmentSpread(fragment_spread) => {
                named_fragments.push(fragment_spread);
            }
            Selection::InlineFragment(inline_fragment) => {
                inline_fragments.push(inline_fragment);
            }
        }
    }

    if !fields.is_empty() || !named_fragments.is_empty() || !inline_fragments.is_empty() {
        fields.sort_by(|&a, &b| a.name.cmp(&b.name));
        named_fragments.sort_by(|&a, &b| a.fragment_name.cmp(&b.fragment_name));
        // Note that inline fragments are not sorted in the JS implementation

        f.write_str("{")?;

        for (i, &field) in fields.iter().enumerate() {
            let field_str = ApolloReportingSignatureFormatter::Field(field).to_string();
            f.write_str(&field_str)?;

            // We need to insert a space if this is not the last field and it ends in an alphanumeric character
            if i < fields.len() - 1
                && field_str
                    .chars()
                    .last()
                    .map_or(false, |c| c.is_alphanumeric())
            {
                f.write_str(" ")?;
            }
        }

        for &frag in named_fragments.iter() {
            format_fragment_spread(frag, f)?;
        }

        for &frag in inline_fragments.iter() {
            format_inline_fragment(frag, f)?;
        }

        f.write_str("}")?;
    }

    Ok(())
}

fn format_variable(arg: &Node<VariableDefinition>, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "${}:{}", arg.name, arg.ty)?;
    if let Some(value) = &arg.default_value {
        f.write_str("=")?;
        format_value(value, f)?;
    }
    format_directives(&arg.directives, false, f)
}

fn format_argument(arg: &Node<Argument>, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}:", arg.name)?;
    format_value(&arg.value, f)
}

fn format_field(field: &Node<Field>, f: &mut fmt::Formatter) -> fmt::Result {
    f.write_str(&field.name)?;

    let mut sorted_args = field.arguments.clone();
    if !sorted_args.is_empty() {
        sorted_args.sort_by(|a, b| a.name.cmp(&b.name));

        f.write_str("(")?;

        // The graphql-js implementation will use newlines and indentation instead of commas if the length of the "arg line" is
        // over 80 characters. This "arg line" includes the alias followed by ": " if the field has an alias (which is never
        // the case for now), followed by all argument names and values separated by ": ", surrounded with brackets. Our usage
        // reporting plugin replaces all newlines + indentation with a single space, so we have to replace commas with spaces if
        // the line length is too long.
        let arg_strings: Vec<String> = sorted_args
            .iter()
            .map(|a| ApolloReportingSignatureFormatter::Argument(a).to_string())
            .collect();
        // Adjust for incorrect spacing generated by the argument formatter - 2 extra characters for the surrounding brackets, plus
        // 2 extra characters per argument for the separating space and the space between the argument name and type.
        let original_line_length =
            2 + arg_strings.iter().map(|s| s.len()).sum::<usize>() + (arg_strings.len() * 2);
        let separator = if original_line_length > 80 { " " } else { "," };

        for (index, arg_string) in arg_strings.iter().enumerate() {
            f.write_str(arg_string)?;

            // We only need to insert a separating space it's not the last arg and if the string ends in an alphanumeric character
            if index < arg_strings.len() - 1
                && arg_string
                    .chars()
                    .last()
                    .map_or(true, |c| c.is_alphanumeric())
            {
                f.write_str(separator)?;
            }
        }
        f.write_str(")")?;
    }

    // In the JS implementation, only the fragment directives are sorted
    format_directives(&field.directives, false, f)?;
    format_selection_set(&field.selection_set, f)
}

fn format_fragment_spread(
    fragment_spread: &Node<FragmentSpread>,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    write!(f, "...{}", fragment_spread.fragment_name)?;
    format_directives(&fragment_spread.directives, true, f)
}

fn format_inline_fragment(
    inline_fragment: &Node<InlineFragment>,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    if let Some(type_name) = &inline_fragment.type_condition {
        write!(f, "...on {}", type_name)?;
    } else {
        f.write_str("...")?;
    }

    format_directives(&inline_fragment.directives, true, f)?;
    format_selection_set(&inline_fragment.selection_set, f)
}

fn format_fragment(fragment: &Node<Fragment>, f: &mut fmt::Formatter) -> fmt::Result {
    write!(
        f,
        "fragment {} on {}",
        &fragment.name.to_string(),
        &fragment.selection_set.ty.to_string()
    )?;
    format_directives(&fragment.directives, true, f)?;
    format_selection_set(&fragment.selection_set, f)
}

fn format_directives(
    directives: &DirectiveList,
    sorted: bool,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    let mut sorted_directives = directives.clone();
    if sorted {
        sorted_directives.sort_by(|a, b| a.name.cmp(&b.name));
    }

    for directive in sorted_directives.iter() {
        write!(f, "@{}", directive.name)?;

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
    }

    Ok(())
}

fn format_value(value: &Value, f: &mut fmt::Formatter) -> fmt::Result {
    match value {
        Value::String(_) => f.write_str("\"\""),
        Value::Float(_) | Value::Int(_) => f.write_str("0"),
        Value::Object(_) => f.write_str("{}"),
        Value::List(_) => f.write_str("[]"),
        rest => f.write_str(&rest.to_string()),
    }
}

#[cfg(test)]
mod tests {
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
        let planner = Planner::<serde_json::Value>::new(
            schema_str.to_string(),
            QueryPlannerConfig::default(),
        )
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

        let generated =
            generate_usage_reporting(&doc, &doc, &Some("TransformedQuery".into()), &schema);

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

        let schema: Valid<Schema> =
            Schema::parse_and_validate(schema_str, "schema.graphql").unwrap();
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

        let generated =
            generate_usage_reporting(&doc, &doc, &Some("FragmentQuery".into()), &schema);

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

        let generated =
            generate_usage_reporting(&doc, &doc, &Some("DirectiveQuery".into()), &schema);

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

        let generated =
            generate_usage_reporting(&doc, &doc, &Some("QueryWithVars".into()), &schema);

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

        let generated =
            generate_usage_reporting(&doc, &doc, &Some("QueryArgLength".into()), &schema);

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

        let generated =
            generate_usage_reporting(&doc, &doc, &Some("QueryArgLength".into()), &schema);

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
                stats_report_key:
                    "# NamedQuery\nquery NamedQuery {basicResponseQuery{field1 field2}}".into(),
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
}
