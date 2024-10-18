//! Generation of usage reporting fields
use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::fmt::Write;
use std::ops::AddAssign;
use std::sync::Arc;

use apollo_compiler::ast::Argument;
use apollo_compiler::ast::DirectiveList;
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
use apollo_compiler::schema::ExtendedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::Schema;
use router_bridge::planner::ReferencedFieldsForType;
use router_bridge::planner::UsageReporting;
use serde::Serialize;

use crate::json_ext::Object;
use crate::json_ext::Value as JsonValue;
use crate::plugins::telemetry::config::ApolloSignatureNormalizationAlgorithm;
use crate::spec::Fragments;
use crate::spec::Query;
use crate::spec::Selection as SpecSelection;

/// The stats for a single execution of an input object field.
#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct InputObjectFieldStats {
    /// True if the input object field was referenced.
    pub(crate) referenced: bool,
    /// True if the input object field was referenced but the value was null.
    pub(crate) null_reference: bool,
    /// True if the input object field was missing or undefined.
    pub(crate) undefined_reference: bool,
}

/// The stats for a an input object field across multiple executions.
#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct AggregatedInputObjectFieldStats {
    /// The number of executions where the field was referenced.
    pub(crate) referenced: u64,
    /// The number of executions where the field was referenced with a null value.
    pub(crate) null_reference: u64,
    /// The number of executions where the field was missing or undefined.
    pub(crate) undefined_reference: u64,
}

pub(crate) type ReferencedEnums = HashMap<String, HashSet<String>>;

/// The result of the generate_extended_references function which contains input object field and
/// enum value stats for a single execution.
#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct ExtendedReferenceStats {
    /// A map of parent type to a map of field name to stats
    pub(crate) referenced_input_fields: HashMap<String, HashMap<String, InputObjectFieldStats>>,
    /// A map of enum name to a set of enum values that were referenced
    pub(crate) referenced_enums: ReferencedEnums,
}

/// The aggregation of ExtendedReferenceStats across a number of executions.
#[derive(Clone, Default, Debug, Serialize)]
pub(crate) struct AggregatedExtendedReferenceStats {
    /// A map of parent type to a map of field name to aggregated stats
    pub(crate) referenced_input_fields:
        HashMap<String, HashMap<String, AggregatedInputObjectFieldStats>>,
    /// A map of enum name to a map of enum values to the number of executions that referenced them.
    pub(crate) referenced_enums: HashMap<String, HashMap<String, u64>>,
}

impl AggregatedExtendedReferenceStats {
    pub(crate) fn is_empty(&self) -> bool {
        self.referenced_input_fields.is_empty() && self.referenced_enums.is_empty()
    }
}

impl AddAssign<ExtendedReferenceStats> for AggregatedExtendedReferenceStats {
    fn add_assign(&mut self, other_stats: ExtendedReferenceStats) {
        // Not using entry API here due to performance impact

        // Merge input object references
        for (type_name, type_stats) in other_stats.referenced_input_fields.iter() {
            let field_name_stats = match self.referenced_input_fields.get_mut(type_name) {
                Some(existing_stats) => existing_stats,
                None => {
                    self.referenced_input_fields
                        .insert(type_name.to_string(), HashMap::new());
                    self.referenced_input_fields.get_mut(type_name).unwrap()
                }
            };

            for (field_name, field_stats) in type_stats.iter() {
                let ref_count = if field_stats.referenced { 1 } else { 0 };
                let null_ref_count = if field_stats.null_reference { 1 } else { 0 };
                let undefined_ref_count = if field_stats.undefined_reference {
                    1
                } else {
                    0
                };

                match field_name_stats.get_mut(field_name) {
                    Some(existing_stats) => {
                        existing_stats.referenced += ref_count;
                        existing_stats.null_reference += null_ref_count;
                        existing_stats.undefined_reference += undefined_ref_count;
                    }
                    None => {
                        field_name_stats.insert(
                            field_name.to_string(),
                            AggregatedInputObjectFieldStats {
                                referenced: ref_count,
                                null_reference: null_ref_count,
                                undefined_reference: undefined_ref_count,
                            },
                        );
                    }
                };
            }
        }

        *self += other_stats.referenced_enums;
    }
}

impl AddAssign<ReferencedEnums> for AggregatedExtendedReferenceStats {
    fn add_assign(&mut self, other_enum_stats: ReferencedEnums) {
        // Not using entry API here due to performance impact
        for (enum_name, enum_values) in other_enum_stats.iter() {
            let enum_name_stats = match self.referenced_enums.get_mut(enum_name) {
                Some(existing_stats) => existing_stats,
                None => {
                    self.referenced_enums
                        .insert(enum_name.to_string(), HashMap::new());
                    self.referenced_enums
                        .get_mut(enum_name)
                        .expect("value is expected to be in map")
                }
            };

            for enum_value in enum_values.iter() {
                match enum_name_stats.get_mut(enum_value) {
                    Some(existing_stats) => *existing_stats += 1,
                    None => {
                        enum_name_stats.insert(enum_value.to_string(), 1);
                    }
                };
            }
        }
    }
}

/// The result of the generate_usage_reporting function which contains a UsageReporting struct and
/// functions that allow comparison with another ComparableUsageReporting or UsageReporting object.
pub(crate) struct ComparableUsageReporting {
    /// The UsageReporting fields
    pub(crate) result: UsageReporting,
}

/// Generate a ComparableUsageReporting containing the stats_report_key (a normalized version of the operation signature)
/// and referenced fields of an operation. The document used to generate the signature and for the references can be
/// different to handle cases where the operation has been filtered, but we want to keep the same signature.
pub(crate) fn generate_usage_reporting(
    signature_doc: &ExecutableDocument,
    references_doc: &ExecutableDocument,
    operation_name: &Option<String>,
    schema: &Valid<Schema>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
) -> ComparableUsageReporting {
    let mut generator = UsageGenerator {
        signature_doc,
        references_doc,
        operation_name,
        schema,
        normalization_algorithm,
        variables: &Object::new(),
        fragments_map: HashMap::new(),
        fields_by_type: HashMap::new(),
        fields_by_interface: HashMap::new(),
        enums_by_name: HashMap::new(),
        input_field_references: HashMap::new(),
        fragment_spread_set: HashSet::new(),
    };

    generator.generate_usage_reporting()
}

pub(crate) fn generate_extended_references(
    doc: Arc<Valid<ExecutableDocument>>,
    operation_name: Option<String>,
    schema: &Valid<Schema>,
    variables: &Object,
) -> ExtendedReferenceStats {
    let mut generator = UsageGenerator {
        signature_doc: &doc,
        references_doc: &doc,
        operation_name: &operation_name,
        schema,
        normalization_algorithm: &ApolloSignatureNormalizationAlgorithm::default(),
        variables,
        fragments_map: HashMap::new(),
        fields_by_type: HashMap::new(),
        fields_by_interface: HashMap::new(),
        enums_by_name: HashMap::new(),
        input_field_references: HashMap::new(),
        fragment_spread_set: HashSet::new(),
    };

    generator.generate_extended_references()
}

pub(crate) fn extract_enums_from_response(
    query: Arc<Query>,
    schema: &Valid<Schema>,
    response_body: &Object,
    existing_refs: &mut ReferencedEnums,
) {
    extract_enums_from_selection_set(
        &query.operation.selection_set,
        &query.fragments,
        schema,
        response_body,
        existing_refs,
    );
}

fn add_enum_value_to_map(
    enum_name: &Name,
    enum_value: &JsonValue,
    referenced_enums: &mut ReferencedEnums,
) {
    match enum_value {
        JsonValue::String(val_str) => {
            // Not using entry API here due to performance impact
            let enum_name_stats = match referenced_enums.get_mut(enum_name.as_str()) {
                Some(existing_stats) => existing_stats,
                None => {
                    referenced_enums.insert(enum_name.to_string(), HashSet::new());
                    referenced_enums
                        .get_mut(enum_name.as_str())
                        .expect("value is expected to be in map")
                }
            };

            enum_name_stats.insert(val_str.as_str().to_string());
        }
        JsonValue::Array(val_list) => {
            for val in val_list {
                add_enum_value_to_map(enum_name, val, referenced_enums);
            }
        }
        _ => {}
    }
}

fn extract_enums_from_selection_set(
    selection_set: &[SpecSelection],
    fragments: &Fragments,
    schema: &Valid<Schema>,
    selection_response: &Object,
    result_set: &mut ReferencedEnums,
) {
    for selection in selection_set.iter() {
        match selection {
            SpecSelection::Field {
                name,
                alias,
                field_type,
                selection_set,
                ..
            } => {
                let field_name = alias.as_ref().unwrap_or(name).as_str();
                if let Some(field_value) = selection_response.get(field_name) {
                    let field_type_def = schema.types.get(field_type.0.inner_named_type());

                    // If the value is an enum, we want to add all values to the map
                    if let Some(ExtendedType::Enum(enum_type)) = field_type_def {
                        add_enum_value_to_map(&enum_type.name, field_value, result_set);
                    }
                    // Otherwise if the response value is an object, add any enums from the field's selection set
                    else if let JsonValue::Object(value_object) = field_value {
                        if let Some(selection_set) = selection_set {
                            extract_enums_from_selection_set(
                                selection_set,
                                fragments,
                                schema,
                                value_object,
                                result_set,
                            );
                        }
                    }
                }
            }
            SpecSelection::InlineFragment { selection_set, .. } => {
                extract_enums_from_selection_set(
                    selection_set,
                    fragments,
                    schema,
                    selection_response,
                    result_set,
                );
            }
            SpecSelection::FragmentSpread { name, .. } => {
                if let Some(fragment) = fragments.get(name) {
                    extract_enums_from_selection_set(
                        &fragment.selection_set,
                        fragments,
                        schema,
                        selection_response,
                        result_set,
                    );
                }
            }
        }
    }
}

struct UsageGenerator<'a> {
    signature_doc: &'a ExecutableDocument,
    references_doc: &'a ExecutableDocument,
    operation_name: &'a Option<String>,
    schema: &'a Valid<Schema>,
    normalization_algorithm: &'a ApolloSignatureNormalizationAlgorithm,
    variables: &'a Object,
    fragments_map: HashMap<String, Node<Fragment>>,
    fields_by_type: HashMap<String, HashSet<String>>,
    fields_by_interface: HashMap<String, bool>,
    enums_by_name: HashMap<String, HashSet<String>>,
    input_field_references: HashMap<String, HashMap<String, InputObjectFieldStats>>,
    fragment_spread_set: HashSet<Name>,
}

impl UsageGenerator<'_> {
    fn generate_usage_reporting(&mut self) -> ComparableUsageReporting {
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
            .operations
            .get(self.operation_name.as_deref())
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
                            self.extract_signature_fragments(&fragment.selection_set);
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
            let formatter = SignatureFormatterWithAlgorithm {
                formatter: &ApolloReportingSignatureFormatter::Fragment(f),
                normalization_algorithm: self.normalization_algorithm,
            };
            write!(&mut result, "{formatter}").expect("infallible");
        });

        // Followed by the operation
        let formatter = SignatureFormatterWithAlgorithm {
            formatter: &ApolloReportingSignatureFormatter::Operation(operation),
            normalization_algorithm: self.normalization_algorithm,
        };
        write!(&mut result, "{formatter}").expect("infallible");

        result
    }

    fn generate_apollo_reporting_refs(&mut self) -> HashMap<String, ReferencedFieldsForType> {
        self.fragment_spread_set.clear();
        self.fields_by_type.clear();
        self.fields_by_interface.clear();

        match self
            .references_doc
            .operations
            .get(self.operation_name.as_deref())
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
                            // These fields don't strictly need to be sorted, but doing it here means we don't have to
                            // update all our tests and snapshots to compare the sorted version of the data.
                            let mut sorted_field_names =
                                field_names.iter().cloned().collect::<Vec<_>>();
                            sorted_field_names.sort();
                            let refs = ReferencedFieldsForType {
                                field_names: sorted_field_names,
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

    fn generate_extended_references(&mut self) -> ExtendedReferenceStats {
        self.fragment_spread_set.clear();
        self.enums_by_name.clear();
        self.input_field_references.clear();

        if let Ok(operation) = self
            .references_doc
            .operations
            .get(self.operation_name.as_deref())
        {
            self.process_extended_refs_for_selection_set(&operation.selection_set);
        }

        ExtendedReferenceStats {
            referenced_input_fields: self.input_field_references.clone(),
            referenced_enums: self.enums_by_name.clone(),
        }
    }

    fn add_enum_reference(&mut self, enum_name: String, enum_value: String) {
        // Not using entry API here due to performance impact
        let enum_name_stats = match self.enums_by_name.get_mut(&enum_name) {
            Some(existing_stats) => existing_stats,
            None => {
                self.enums_by_name
                    .insert(enum_name.to_string(), HashSet::new());
                self.enums_by_name
                    .get_mut(&enum_name)
                    .expect("value is expected to be in map")
            }
        };

        enum_name_stats.insert(enum_value.to_string());
    }

    fn add_input_object_reference(
        &mut self,
        type_name: String,
        field_name: String,
        is_referenced: bool,
        is_null_reference: bool,
    ) {
        // Not using entry API here due to performance impact
        let type_name_stats = match self.input_field_references.get_mut(&type_name) {
            Some(existing_stats) => existing_stats,
            None => {
                self.input_field_references
                    .insert(type_name.to_string(), HashMap::new());
                self.input_field_references
                    .get_mut(&type_name)
                    .expect("value is expected to be in map")
            }
        };

        match type_name_stats.get_mut(&field_name) {
            Some(stats) => {
                stats.referenced = stats.referenced || is_referenced;
                stats.null_reference = stats.null_reference || is_null_reference;
                stats.undefined_reference = stats.undefined_reference || !is_referenced;
            }
            None => {
                type_name_stats.insert(
                    field_name.to_string(),
                    InputObjectFieldStats {
                        referenced: is_referenced,
                        null_reference: is_null_reference,
                        undefined_reference: !is_referenced,
                    },
                );
            }
        };
    }

    fn process_extended_refs_for_selection_set(&mut self, selection_set: &SelectionSet) {
        for selection in &selection_set.selections {
            match selection {
                Selection::Field(field) => {
                    for arg in &field.arguments {
                        if let Some(arg_def) = field.definition.argument_by_name(arg.name.as_str())
                        {
                            let type_name = arg_def.ty.inner_named_type();
                            self.process_extended_refs_for_value(type_name.to_string(), &arg.value);
                        }
                    }
                }
                Selection::InlineFragment(fragment) => {
                    self.process_extended_refs_for_selection_set(&fragment.selection_set);
                }
                Selection::FragmentSpread(fragment) => {
                    if !self.fragment_spread_set.contains(&fragment.fragment_name) {
                        self.fragment_spread_set
                            .insert(fragment.fragment_name.clone());

                        if let Some(fragment) =
                            self.references_doc.fragments.get(&fragment.fragment_name)
                        {
                            self.process_extended_refs_for_selection_set(&fragment.selection_set);
                        }
                    }
                }
            }
        }
    }

    fn process_extended_refs_for_value(&mut self, type_name: String, value: &Node<Value>) {
        match value.as_ref() {
            Value::Enum(enum_value) => {
                self.add_enum_reference(type_name.clone(), enum_value.to_string());
            }
            Value::List(list_values) => {
                for list_value in list_values {
                    self.process_extended_refs_for_value(type_name.to_string(), list_value);
                }
            }
            Value::Object(obj_value) => {
                self.process_extended_refs_for_object(type_name.to_string(), obj_value);
            }
            Value::Variable(var_name) => {
                let var_value = self.variables.get(var_name.to_string().as_str());
                self.process_extended_refs_for_variable(type_name.to_string(), var_value);
            }
            _ => (),
        }
    }

    fn process_extended_refs_for_object(
        &mut self,
        type_name: String,
        obj_value: &[(Name, Node<Value>)],
    ) {
        // For object references, we're only interested in input object types
        if let Some(ExtendedType::InputObject(input_object_type)) =
            self.schema.types.get(type_name.to_string().as_str())
        {
            let obj_value_map: HashMap<String, &Node<Value>> = obj_value
                .iter()
                .map(|(name, val)| (name.to_string(), val))
                .collect();
            for (field_name, field_def) in &input_object_type.fields {
                let field_type = field_def.ty.inner_named_type().to_string();
                let maybe_field_val = obj_value_map.get(&field_name.to_string());

                self.add_input_object_reference(
                    type_name.to_string(),
                    field_name.to_string(),
                    maybe_field_val.is_some(),
                    maybe_field_val.is_some_and(|v| v.is_null()),
                );

                if let Some(field_val) = maybe_field_val {
                    self.process_extended_refs_for_value(field_type, field_val);
                }
            }
        }
    }

    fn process_extended_refs_for_variable(
        &mut self,
        type_name: String,
        var_value: Option<&JsonValue>,
    ) {
        match self.schema.types.get(type_name.to_string().as_str()) {
            Some(ExtendedType::InputObject(input_object_type)) => {
                match var_value {
                    // For input objects, we store input object references and process each of the field variables
                    Some(JsonValue::Object(json_obj)) => {
                        let var_value_map: HashMap<String, &JsonValue> = json_obj
                            .iter()
                            .map(|(name, val)| (name.as_str().to_string(), val))
                            .collect();

                        for (field_name, field_def) in &input_object_type.fields {
                            let field_type = field_def.ty.inner_named_type().to_string();
                            let maybe_field_val = var_value_map.get(&field_name.to_string());

                            self.add_input_object_reference(
                                type_name.to_string(),
                                field_name.to_string(),
                                maybe_field_val.is_some(),
                                maybe_field_val.is_some_and(|v| v.is_null()),
                            );

                            if let Some(&field_val) = maybe_field_val {
                                self.process_extended_refs_for_variable(
                                    field_type,
                                    Some(field_val),
                                );
                            }
                        }
                    }
                    // For arrays of objects, we process each array value separately
                    Some(JsonValue::Array(json_array)) => {
                        for array_val in json_array {
                            self.process_extended_refs_for_variable(
                                type_name.clone(),
                                Some(array_val),
                            );
                        }
                    }
                    _ => {}
                }
            }
            Some(ExtendedType::Enum(enum_type)) => match var_value {
                Some(JsonValue::String(enum_value)) => {
                    self.add_enum_reference(
                        enum_type.name.to_string(),
                        enum_value.as_str().to_string(),
                    );
                }
                Some(JsonValue::Array(array_values)) => {
                    for array_val in array_values {
                        self.process_extended_refs_for_variable(type_name.clone(), Some(array_val));
                    }
                }
                _ => {}
            },
            _ => {}
        };
    }
}

enum ApolloReportingSignatureFormatter<'a> {
    Operation(&'a Node<Operation>),
    Fragment(&'a Node<Fragment>),
    Argument(&'a Node<Argument>),
    Field(&'a Node<Field>),
}

struct SignatureFormatterWithAlgorithm<'a> {
    formatter: &'a ApolloReportingSignatureFormatter<'a>,
    normalization_algorithm: &'a ApolloSignatureNormalizationAlgorithm,
}

impl<'a> fmt::Display for SignatureFormatterWithAlgorithm<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self.formatter {
            ApolloReportingSignatureFormatter::Operation(operation) => {
                format_operation(operation, self.normalization_algorithm, f)
            }
            ApolloReportingSignatureFormatter::Fragment(fragment) => {
                format_fragment(fragment, self.normalization_algorithm, f)
            }
            ApolloReportingSignatureFormatter::Argument(argument) => {
                format_argument(argument, self.normalization_algorithm, f)
            }
            ApolloReportingSignatureFormatter::Field(field) => {
                format_field(field, self.normalization_algorithm, f)
            }
        }
    }
}

fn is_enhanced(normalization_algorithm: &ApolloSignatureNormalizationAlgorithm) -> bool {
    matches!(
        normalization_algorithm,
        ApolloSignatureNormalizationAlgorithm::Enhanced
    )
}

fn format_operation(
    operation: &Node<Operation>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
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
                format_variable(variable, normalization_algorithm, f)?;
            }
            f.write_str(")")?;
        }

        // In the JS implementation, only the fragment directives are sorted (this is overridden in enhanced mode)
        format_directives(&operation.directives, false, normalization_algorithm, f)?;
    }

    format_selection_set(&operation.selection_set, normalization_algorithm, f)
}

fn format_selection_set(
    selection_set: &SelectionSet,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
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
        if is_enhanced(normalization_algorithm) {
            // in enhanced mode we display aliases so we show non-aliased field sorted by name first, then aliased fields sorted by alias
            fields.sort_by(|&a, &b| {
                match (a.alias.as_ref(), b.alias.as_ref()) {
                    (None, None) => a.name.cmp(&b.name), // when both are non-aliased, sort by field name
                    (Some(alias_a), Some(alias_b)) => alias_a.cmp(alias_b), // when both are aliased, sort by alias
                    // when one is aliased and on isn't, the non-aliased field comes first
                    (Some(_), None) => Ordering::Greater,
                    (None, Some(_)) => Ordering::Less,
                }
            });
        } else {
            // otherwise we just sort by field name (and remove aliases in the field)
            fields.sort_by(|&a, &b| a.name.cmp(&b.name));
        }

        // named fragments are always sorted
        named_fragments.sort_by(|&a, &b| a.fragment_name.cmp(&b.fragment_name));

        // in enhanced mode we sort inline fragments
        if is_enhanced(normalization_algorithm) {
            inline_fragments.sort_by(|&a, &b| {
                let a_name = a.type_condition.as_ref().map(|t| t.as_str()).unwrap_or("");
                let b_name = b.type_condition.as_ref().map(|t| t.as_str()).unwrap_or("");
                a_name.cmp(b_name)
            });
        }

        f.write_str("{")?;

        for (i, &field) in fields.iter().enumerate() {
            let formatter = SignatureFormatterWithAlgorithm {
                formatter: &ApolloReportingSignatureFormatter::Field(field),
                normalization_algorithm,
            };
            let field_str = format!("{}", formatter);
            f.write_str(&field_str)?;

            // We need to insert a space if this is not the last field and it ends in an alphanumeric character.
            let use_separator = field_str
                .chars()
                .last()
                .map_or(false, |c| c.is_alphanumeric() || c == '_');
            if i < fields.len() - 1 && use_separator {
                f.write_str(" ")?;
            }
        }

        for &frag in named_fragments.iter() {
            format_fragment_spread(frag, normalization_algorithm, f)?;
        }

        for &frag in inline_fragments.iter() {
            format_inline_fragment(frag, normalization_algorithm, f)?;
        }

        f.write_str("}")?;
    }

    Ok(())
}

fn format_variable(
    arg: &Node<VariableDefinition>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    write!(f, "${}:{}", arg.name, arg.ty)?;
    if let Some(value) = &arg.default_value {
        f.write_str("=")?;
        format_value(value, normalization_algorithm, f)?;
    }

    // The JS implementation doesn't sort directives (this is overridden in enhanced mode)
    format_directives(&arg.directives, false, normalization_algorithm, f)
}

fn format_argument(
    arg: &Node<Argument>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    write!(f, "{}:", arg.name)?;
    format_value(&arg.value, normalization_algorithm, f)
}

fn format_field(
    field: &Node<Field>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    if is_enhanced(normalization_algorithm) {
        if let Some(alias) = &field.alias {
            write!(f, "{alias}:")?;
        }
    }

    f.write_str(&field.name)?;

    let mut sorted_args = field.arguments.clone();
    if !sorted_args.is_empty() {
        sorted_args.sort_by(|a, b| a.name.cmp(&b.name));

        f.write_str("(")?;

        let arg_strings: Vec<String> = sorted_args
            .iter()
            .map(|a| {
                let formatter = SignatureFormatterWithAlgorithm {
                    formatter: &ApolloReportingSignatureFormatter::Argument(a),
                    normalization_algorithm,
                };
                format!("{}", formatter)
            })
            .collect();

        let separator = get_arg_separator(&field.name, &arg_strings, normalization_algorithm);

        for (index, arg_string) in arg_strings.iter().enumerate() {
            f.write_str(arg_string)?;

            // We only need to insert a separating space it's not the last arg and if the string ends in an alphanumeric character.
            // If it's a comma, we always need to insert it if it's not the last arg.
            if index < arg_strings.len() - 1
                && (separator == ','
                    || arg_string
                        .chars()
                        .last()
                        .map_or(true, |c| c.is_alphanumeric() || c == '_'))
            {
                write!(f, "{}", separator)?;
            }
        }
        f.write_str(")")?;
    }

    // In the JS implementation, only the fragment directives are sorted (this is overridden in enhanced mode)
    format_directives(&field.directives, false, normalization_algorithm, f)?;
    format_selection_set(&field.selection_set, normalization_algorithm, f)
}

fn format_inline_fragment(
    inline_fragment: &Node<InlineFragment>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    if let Some(type_name) = &inline_fragment.type_condition {
        write!(f, "...on {}", type_name)?;
    } else {
        f.write_str("...")?;
    }

    format_directives(
        &inline_fragment.directives,
        true,
        normalization_algorithm,
        f,
    )?;
    format_selection_set(&inline_fragment.selection_set, normalization_algorithm, f)
}

fn format_fragment(
    fragment: &Node<Fragment>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    write!(
        f,
        "fragment {} on {}",
        &fragment.name.to_string(),
        &fragment.selection_set.ty.to_string()
    )?;
    format_directives(&fragment.directives, true, normalization_algorithm, f)?;
    format_selection_set(&fragment.selection_set, normalization_algorithm, f)
}

fn format_directives(
    directives: &DirectiveList,
    sorted: bool,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    let mut sorted_directives = directives.clone();

    // In enhanced mode, we always want to sort
    if sorted || is_enhanced(normalization_algorithm) {
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
                let formatter = SignatureFormatterWithAlgorithm {
                    formatter: &ApolloReportingSignatureFormatter::Argument(argument),
                    normalization_algorithm,
                };
                write!(f, "{}", formatter)?;
            }

            f.write_str(")")?;
        }
    }

    Ok(())
}

fn format_value(
    value: &Value,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    match value {
        Value::String(_) => f.write_str("\"\""),
        Value::Float(_) | Value::Int(_) => f.write_str("0"),
        Value::Object(o) => {
            if is_enhanced(normalization_algorithm) {
                f.write_str("{")?;
                for (index, (name, val)) in o.iter().enumerate() {
                    if index != 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{}:", name)?;
                    format_value(val, normalization_algorithm, f)?;
                }
                f.write_str("}")
            } else {
                f.write_str("{}")
            }
        }
        Value::List(_) => f.write_str("[]"),
        rest => f.write_str(&rest.to_string()),
    }
}

// Figure out which separator to use between arguments
fn get_arg_separator(
    field_name: &Name,
    arg_strings: &[String],
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
) -> char {
    // In enhanced mode, we just always use a comma
    if is_enhanced(normalization_algorithm) {
        return ',';
    }

    // The graphql-js implementation will use newlines and indentation instead of commas if the length of the "arg line" is
    // over 80 characters. This "arg line" includes the alias followed by ": " if the field has an alias (which is never
    // the case for any signatures that the JS implementation formatted), followed by the field name, followed by all argument
    // names and values separated by ": ", surrounded with brackets. Our usage reporting plugin replaces all newlines +
    // indentation with a single space, so we have to replace commas with spaces if the line length is too long.
    // We adjust for incorrect spacing generated by the argument formatter here. We end summing up:
    // * the length of field name
    // * 2 extra characters for the surrounding brackets
    // * the length of all formatted arguments
    // * one extra character per argument since the JS implementation inserts a space between the argument name and value
    // * two extra character per argument except the last one since the JS implementation inserts a separating comma and space
    //   between arguments (but not the last one)
    let original_line_length = field_name.len()
        + 2
        + arg_strings.iter().map(|s| s.len()).sum::<usize>()
        + arg_strings.len()
        + ((arg_strings.len() - 1) * 2);
    if original_line_length > 80 {
        ' '
    } else {
        ','
    }
}

fn format_fragment_spread(
    fragment_spread: &Node<FragmentSpread>,
    normalization_algorithm: &ApolloSignatureNormalizationAlgorithm,
    f: &mut fmt::Formatter,
) -> fmt::Result {
    write!(f, "...{}", fragment_spread.fragment_name)?;
    format_directives(
        &fragment_spread.directives,
        true,
        normalization_algorithm,
        f,
    )
}

#[cfg(test)]
mod tests;
