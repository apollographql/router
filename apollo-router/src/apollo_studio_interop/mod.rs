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
                self.extract_fields(operation_type, &operation.selection_set);

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

    fn extract_fields(&mut self, parent_type: &str, selection_set: &SelectionSet) {
        if !self.fields_by_interface.contains_key(parent_type) {
            let field_schema_type = self.schema.types.get(parent_type);
            let is_interface = field_schema_type.is_some_and(|t| t.is_interface());
            self.fields_by_interface
                .insert(parent_type.into(), is_interface);
        }

        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    self.fields_by_type
                        .entry(parent_type.into())
                        .or_default()
                        .insert(field.name.to_string());

                    let field_type = field.selection_set.ty.to_string();
                    self.extract_fields(&field_type, &field.selection_set);
                }
                Selection::InlineFragment(fragment) => {
                    let frag_type_name = match fragment.type_condition.clone() {
                        Some(fragment_type) => fragment_type.to_string(),
                        None => parent_type.into(),
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

            // We only need to insert a separating space it's not the last arg and if the string ends in an alphanumeric character.
            // If it's a comma, we always need to insert it if it's not the last arg.
            if index < arg_strings.len() - 1
                && (separator == ","
                    || arg_string
                        .chars()
                        .last()
                        .map_or(true, |c| c.is_alphanumeric()))
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
mod tests;
