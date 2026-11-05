use std::sync::Arc;

use ahash::HashMap;
use ahash::HashMapExt;
use apollo_compiler::Schema;
use apollo_compiler::ast::FieldDefinition;
use apollo_compiler::ast::NamedType;
use apollo_compiler::ast::Type;
use apollo_compiler::ast::Value as AstValue;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;
use apollo_compiler::executable::SelectionSet;
use apollo_compiler::parser::Parser;
use apollo_compiler::validation::Valid;
use indexmap::IndexSet;
use serde_json_bytes::Value as JsonValue;
use tower::BoxError;

use crate::json_ext::Object;
use crate::json_ext::ValueExt;
use crate::plugins::demand_control::DemandControlError;
use crate::plugins::demand_control::cost_calculator::schema::ListSizeDirectiveEntry;

// Traverses a nested AST value by path segments.
// Given path `["pagination", "count"]`, returns the value at `{pagination: {count: <value>}}`.
fn traverse_ast_value<'a>(value: &'a AstValue, path: &[impl AsRef<str>]) -> Option<&'a AstValue> {
    path.iter()
        .try_fold(value, |current, segment| match current {
            AstValue::Object(fields) => fields
                .iter()
                .find(|(name, _)| name.as_str() == segment.as_ref())
                .map(|(_, node)| node.as_ref()),
            _ => None,
        })
}

// Traverses a nested JSON value by path segments.
fn traverse_json_value<'a>(
    value: &'a JsonValue,
    path: &[impl AsRef<str>],
) -> Option<&'a JsonValue> {
    path.iter()
        .try_fold(value, |current, segment| current.get(segment.as_ref()))
}

// Infers a size value from an AST argument value.
//
// `declared_type` is the schema type of the argument (or nested input field) the value is
// supplied for. Per GraphQL input coercion, a single non-list value supplied for a list type is
// coerced to a list of size one, so for a list-typed slicing argument a single value means a
// list size of 1 rather than the value's numeric magnitude.
//
// Returns:
// - `Some(n)` for integer values of non-list arguments (e.g., `first: 10` → 10)
// - `Some(len)` for array values (e.g., `ids: ["a", "b"]` → 2)
// - `Some(1)` for a single non-null value supplied for a list-typed argument
// - Resolves variable references through the provided variables map
// - `None` for null, missing, or unsupported value types
fn infer_size_from_argument(
    value: Option<&AstValue>,
    declared_type: &Type,
    variables: &Object,
) -> Option<i32> {
    match value? {
        AstValue::List(items) => Some(items.len() as i32),
        AstValue::Variable(var_name) => {
            infer_size_from_variable(variables.get(var_name.as_str()), declared_type)
        }
        AstValue::Null => None,
        // A single value supplied for a list-typed argument coerces to a list of one item.
        _ if declared_type.is_list() => Some(1),
        // A slicing argument is a count of elements, so it can never be negative. Clamp to a
        // non-negative lower bound: a negative value would otherwise contribute negative cost
        // and deflate the estimate below what the operation actually costs.
        AstValue::Int(n) => n.try_to_i32().ok().map(|n| n.max(0)),
        _ => None,
    }
}

// Infers a size value from a JSON variable value. See `infer_size_from_argument`.
fn infer_size_from_variable(value: Option<&JsonValue>, declared_type: &Type) -> Option<i32> {
    match value? {
        JsonValue::Array(items) => Some(items.len() as i32),
        JsonValue::Null => None,
        // A single value supplied for a list-typed argument coerces to a list of one item.
        _ if declared_type.is_list() => Some(1),
        // Clamp to a non-negative lower bound for the same reason as literal integer arguments.
        other => other.as_i32().map(|n| n.max(0)),
    }
}

fn resolve_nested_size(
    value: &AstValue,
    path: &[String],
    declared_type: &Type,
    variables: &Object,
) -> Option<i32> {
    match value {
        AstValue::Object(_) => {
            infer_size_from_argument(traverse_ast_value(value, path), declared_type, variables)
        }
        AstValue::Variable(var_name) => infer_size_from_variable(
            variables
                .get(var_name.as_str())
                .and_then(|v| traverse_json_value(v, path)),
            declared_type,
        ),
        _ => None,
    }
}

/// One `slicingArguments` entry of a `@listSize` directive, resolved against the schema at load
/// time: the argument (or dotted path through input objects) the size is read from, and its
/// declared type, which decides how a single value is interpreted (see
/// `infer_size_from_argument`).
#[derive(Clone, Debug)]
pub(in crate::plugins::demand_control) struct SlicingArgument {
    argument_name: String,
    nested_path: Vec<String>,
    declared_type: Type,
}

impl SlicingArgument {
    /// Resolves one slicing argument path such as `first` or `input.pagination.first` against
    /// the field's arguments and the input object types the path leads through. Returns `None`
    /// if any step of the path is not defined in the schema.
    fn resolve(schema: &Schema, field_definition: &FieldDefinition, path: &str) -> Option<Self> {
        let mut segments = path.split('.');
        let argument_name = segments.next()?;
        let argument = field_definition
            .arguments
            .iter()
            .find(|arg| arg.name.as_str() == argument_name)?;
        let mut declared_type: &Type = &argument.ty;
        let mut nested_path = Vec::new();
        for segment in segments {
            let input_object = schema.get_input_object(declared_type.inner_named_type())?;
            declared_type = &input_object.fields.get(segment)?.ty;
            nested_path.push(segment.to_string());
        }
        Some(Self {
            argument_name: argument_name.to_string(),
            nested_path,
            declared_type: declared_type.clone(),
        })
    }

    /// Resolves every path in a directive's `slicingArguments`. A path that does not resolve
    /// against the schema is logged and dropped, so it never contributes a size (the same
    /// outcome as a slicing argument that is absent from the operation).
    pub(in crate::plugins::demand_control) fn from_strings(
        schema: &Schema,
        parent_type_name: &NamedType,
        field_definition: &FieldDefinition,
        paths: &IndexSet<String>,
    ) -> Vec<Self> {
        paths
            .iter()
            .filter_map(|path| {
                let resolved = Self::resolve(schema, field_definition, path);
                if resolved.is_none() {
                    tracing::warn!(
                        "Slicing argument `{path}` of @listSize on `{parent_type_name}.{field_name}` does not resolve to an argument or input field in the schema; it will be ignored",
                        field_name = field_definition.name,
                    );
                }
                resolved
            })
            .collect()
    }

    // Resolves this slicing argument's size from the operation's (merged) arguments and
    // variables.
    fn size(&self, args: &HashMap<&str, &AstValue>, variables: &Object) -> Option<i32> {
        let value = args.get(self.argument_name.as_str())?;
        if self.nested_path.is_empty() {
            infer_size_from_argument(Some(*value), &self.declared_type, variables)
        } else {
            resolve_nested_size(value, &self.nested_path, &self.declared_type, variables)
        }
    }
}

// Collects slicing argument sizes from both default values and actual query arguments.
// Actual values override defaults when both are present.
fn collect_slicing_sizes(
    field: &Field,
    slicing_arguments: &[SlicingArgument],
    variables: &Object,
) -> Vec<i32> {
    // Merge default and actual argument values (actuals take precedence)
    let defaults = field
        .definition
        .arguments
        .iter()
        .filter_map(|arg| arg.default_value.as_deref().map(|v| (arg.name.as_str(), v)));
    let actuals = field
        .arguments
        .iter()
        .map(|arg| (arg.name.as_str(), arg.value.as_ref()));
    let args: HashMap<&str, &AstValue> = defaults.chain(actuals).collect();

    slicing_arguments
        .iter()
        .filter_map(|slicing_argument| slicing_argument.size(&args, variables))
        .collect()
}

pub(in crate::plugins::demand_control) struct IncludeDirective {
    pub(in crate::plugins::demand_control) is_included: bool,
}

impl IncludeDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
    ) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("include")
            .and_then(|skip| skip.specified_argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_included: cond });

        Ok(directive)
    }
}

#[derive(Clone, Debug)]
pub(in crate::plugins::demand_control) struct SizedFields {
    /// Field names we treat as the list (apply size to) at this level.
    list_field_names: IndexSet<String>,
    /// Precomputed nested SizedFields per field name. Built once at schema load; descend() is a lookup.
    descend_map: HashMap<String, Arc<SizedFields>>,
}

impl SizedFields {
    /// Validates one path: at every level, at most one leaf (field with no sub-selections).
    fn validate_one_leaf_per_path(
        selection_set: &SelectionSet,
        field_set_str: &str,
    ) -> Result<(), DemandControlError> {
        let leaf_count = selection_set
            .selections
            .iter()
            .filter(|s| matches!(s, Selection::Field(f) if f.selection_set.selections.is_empty()))
            .count();
        if leaf_count > 1 {
            return Err(DemandControlError::QueryParseFailure(format!(
                "sizedFields entry '{}' must specify at most one list field per path (found {}).",
                field_set_str, leaf_count
            )));
        }
        for s in &selection_set.selections {
            if let Selection::Field(f) = s
                && !f.selection_set.selections.is_empty()
            {
                Self::validate_one_leaf_per_path(&f.selection_set, field_set_str)?;
            }
        }
        Ok(())
    }

    pub(in crate::plugins::demand_control) fn from_strings(
        schema: &Valid<Schema>,
        return_type: &NamedType,
        field_names: &IndexSet<String>,
    ) -> Result<Self, DemandControlError> {
        let selections: Vec<SelectionSet> = field_names
            .iter()
            .map(|field_set_str| {
                let parsed = Parser::new()
                    .parse_field_set(schema, return_type.clone(), field_set_str, "")
                    .map_err(|e| {
                        DemandControlError::QueryParseFailure(format!(
                            "Failed to parse sizedFields entry '{}': {}",
                            field_set_str, e
                        ))
                    })?;
                let selection_set = parsed.selection_set.clone();
                Self::validate_one_leaf_per_path(&selection_set, field_set_str)?;
                Ok(selection_set)
            })
            .collect::<Result<Vec<SelectionSet>, DemandControlError>>()?;

        let raw_descend = Self::build_descend_map_raw(&selections);
        let list_field_names = Self::list_field_names_from_selections(&selections, &raw_descend);
        let descend_map = Self::build_nested_sized_fields(raw_descend);
        Ok(SizedFields {
            list_field_names,
            descend_map,
        })
    }

    /// Build list_field_names from selections, excluding any name that is also a container.
    fn list_field_names_from_selections(
        selection_sets: &[SelectionSet],
        raw_descend: &HashMap<String, Vec<SelectionSet>>,
    ) -> IndexSet<String> {
        let leaf_field_names = Self::collect_leaf_names(selection_sets);
        leaf_field_names
            .iter()
            .filter(|name| !raw_descend.contains_key(name.as_str()))
            .cloned()
            .collect()
    }

    /// Recursively build SizedFields for each nested level so descend() is a lookup at request time.
    fn build_nested_sized_fields(
        raw_descend: HashMap<String, Vec<SelectionSet>>,
    ) -> HashMap<String, Arc<SizedFields>> {
        raw_descend
            .into_iter()
            .filter_map(|(name, nested_selections)| {
                if nested_selections.is_empty() {
                    return None;
                }
                let nested_raw = Self::build_descend_map_raw(&nested_selections);
                let list_field_names =
                    Self::list_field_names_from_selections(&nested_selections, &nested_raw);
                let descend_map = Self::build_nested_sized_fields(nested_raw);
                Some((
                    name,
                    Arc::new(SizedFields {
                        list_field_names,
                        descend_map,
                    }),
                ))
            })
            .collect()
    }

    fn collect_leaf_names(selection_sets: &[SelectionSet]) -> IndexSet<String> {
        let mut names = IndexSet::new();
        for selection_set in selection_sets {
            Self::collect_leaf_names_from_set(selection_set, &mut names);
        }
        names
    }

    fn collect_leaf_names_from_set(selection_set: &SelectionSet, out: &mut IndexSet<String>) {
        for s in &selection_set.selections {
            if let Selection::Field(f) = s {
                if f.selection_set.selections.is_empty() {
                    out.insert(f.name.as_str().to_string());
                } else {
                    Self::collect_leaf_names_from_set(&f.selection_set, out);
                }
            }
        }
    }

    /// Shallow pass: field name -> nested selection sets (one level only).
    fn build_descend_map_raw(
        selection_sets: &[SelectionSet],
    ) -> HashMap<String, Vec<SelectionSet>> {
        let mut map = HashMap::new();
        for selection_set in selection_sets {
            for s in &selection_set.selections {
                if let Selection::Field(f) = s
                    && !f.selection_set.selections.is_empty()
                {
                    map.entry(f.name.as_str().to_string())
                        .or_insert_with(Vec::new)
                        .push(f.selection_set.clone());
                }
            }
        }
        map
    }

    /// True only if this field name is a leaf in our paths and not also a container at this level.
    pub(in crate::plugins::demand_control) fn is_leaf(&self, field_name: &str) -> bool {
        self.list_field_names.contains(field_name)
    }

    /// Returns nested SizedFields for the given field (for descending into "results { page }").
    pub(in crate::plugins::demand_control) fn descend(
        &self,
        field_name: &str,
    ) -> Option<Arc<Self>> {
        self.descend_map.get(field_name).cloned()
    }
}

#[derive(Clone, Debug)]
pub(in crate::plugins::demand_control) struct ListSizeDirective {
    pub(in crate::plugins::demand_control) expected_size: Option<i32>,
    pub(in crate::plugins::demand_control) sized_fields: Option<Arc<SizedFields>>,
}

impl ListSizeDirective {
    /// Build a ListSizeDirective at request time from the slicing arguments and sizedFields
    /// pre-resolved at schema load.
    pub(in crate::plugins::demand_control) fn new(
        entry: &ListSizeDirectiveEntry,
        field: &Field,
        variables: &Object,
    ) -> Result<Self, DemandControlError> {
        let parsed = &entry.directive;
        let expected_size = match entry.slicing_arguments.as_deref() {
            Some(slicing_arguments) => {
                let slicing_sizes = collect_slicing_sizes(field, slicing_arguments, variables);

                if parsed.require_one_slicing_argument && slicing_sizes.len() != 1 {
                    return Err(DemandControlError::QueryParseFailure(format!(
                        "Exactly one slicing argument is required, but found {}",
                        slicing_sizes.len()
                    )));
                }

                slicing_sizes.into_iter().max().or(parsed.assumed_size)
            }
            None => parsed.assumed_size,
        };

        Ok(Self {
            expected_size,
            sized_fields: entry.parsed_sized_fields.clone(),
        })
    }

    pub(in crate::plugins::demand_control) fn size_of(&self, field: &Field) -> Option<i32> {
        if self
            .sized_fields
            .as_deref()
            .is_some_and(|sf| sf.is_leaf(field.name.as_str()))
        {
            self.expected_size
        } else {
            None
        }
    }

    /// Returns a directive scoped to the given nested field (e.g. from `results { page }` to the selection under `results`).
    pub(in crate::plugins::demand_control) fn descend(&self, field_name: &str) -> Option<Self> {
        let nested = self.sized_fields.as_ref()?.descend(field_name)?;
        Some(ListSizeDirective {
            expected_size: self.expected_size,
            sized_fields: Some(nested),
        })
    }
}

pub(in crate::plugins::demand_control) struct RequiresDirective {
    pub(in crate::plugins::demand_control) fields: SelectionSet,
}

impl RequiresDirective {
    pub(in crate::plugins::demand_control) fn from_field_definition(
        definition: &FieldDefinition,
        parent_type_name: &NamedType,
        schema: &Valid<Schema>,
    ) -> Result<Option<Self>, DemandControlError> {
        let requires_arg = definition
            .directives
            .get("join__field")
            .and_then(|requires| requires.specified_argument_by_name("requires"))
            .and_then(|arg| arg.as_str());

        if let Some(arg) = requires_arg {
            let field_set =
                Parser::new().parse_field_set(schema, parent_type_name.clone(), arg, "")?;

            Ok(Some(RequiresDirective {
                fields: field_set.selection_set.clone(),
            }))
        } else {
            Ok(None)
        }
    }
}

pub(in crate::plugins::demand_control) struct SkipDirective {
    pub(in crate::plugins::demand_control) is_skipped: bool,
}

impl SkipDirective {
    pub(in crate::plugins::demand_control) fn from_field(
        field: &Field,
    ) -> Result<Option<Self>, BoxError> {
        let directive = field
            .directives
            .get("skip")
            .and_then(|skip| skip.specified_argument_by_name("if"))
            .and_then(|arg| arg.to_bool())
            .map(|cond| Self { is_skipped: cond });

        Ok(directive)
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::ty;

    use super::*;

    mod infer_size_from_variable_tests {
        use serde_json_bytes::json;

        use super::*;

        #[rstest::rstest]
        #[case::integer_value(json!(42), Some(42))]
        #[case::zero(json!(0), Some(0))]
        // A negative count is clamped to zero so it cannot contribute negative cost.
        #[case::negative_integer_clamped_to_zero(json!(-5), Some(0))]
        #[case::array_with_items(json!(["a", "b", "c"]), Some(3))]
        #[case::empty_array(json!([]), Some(0))]
        #[case::null_value(json!(null), None)]
        #[case::string_value(json!("not a size"), None)]
        #[case::boolean_value(json!(true), None)]
        #[case::object_value(json!({"key": "value"}), None)]
        #[case::float_value(json!(1.5), None)]
        fn test_infer_size(#[case] input: JsonValue, #[case] expected: Option<i32>) {
            assert_eq!(infer_size_from_variable(Some(&input), &ty!(Int)), expected);
        }

        #[test]
        fn none_input_returns_none() {
            assert_eq!(infer_size_from_variable(None, &ty!(Int)), None);
        }
    }

    mod infer_size_from_argument_tests {
        use apollo_compiler::Node;
        use apollo_compiler::ast::IntValue;
        use serde_json_bytes::json;

        use super::*;

        // Helper to create a list with n string items
        fn list_of_size(n: usize) -> AstValue {
            let items = (0..n)
                .map(|i| Node::new(AstValue::String(format!("item{i}"))))
                .collect();
            AstValue::List(items)
        }

        #[rstest::rstest]
        #[case::integer_10("10", Some(10))]
        #[case::integer_0("0", Some(0))]
        // A negative slicing count is clamped to zero so it cannot contribute negative cost.
        #[case::negative_clamped_to_zero("-5", Some(0))]
        fn integer_values(#[case] input: &str, #[case] expected: Option<i32>) {
            let value = AstValue::Int(IntValue::new_parsed(input));
            assert_eq!(
                infer_size_from_argument(Some(&value), &ty!(Int), &Object::new()),
                expected
            );
        }

        #[rstest::rstest]
        #[case::three_items(3, Some(3))]
        #[case::one_item(1, Some(1))]
        #[case::empty(0, Some(0))]
        fn list_values(#[case] size: usize, #[case] expected: Option<i32>) {
            let value = list_of_size(size);
            assert_eq!(
                infer_size_from_argument(Some(&value), &ty!(Int), &Object::new()),
                expected
            );
        }

        #[rstest::rstest]
        #[case::resolves_to_int("count", json!(5), Some(5))]
        #[case::resolves_to_negative_int_clamped("count", json!(-10), Some(0))]
        #[case::resolves_to_array("ids", json!(["x", "y", "z"]), Some(3))]
        #[case::resolves_to_empty_array("empty", json!([]), Some(0))]
        #[case::resolves_to_null("nullval", json!(null), None)]
        fn variable_resolution(
            #[case] var_name: &str,
            #[case] var_value: serde_json_bytes::Value,
            #[case] expected: Option<i32>,
        ) {
            let value = AstValue::Variable(apollo_compiler::Name::new_unchecked(var_name));
            let mut variables = Object::new();
            variables.insert(var_name, var_value);
            assert_eq!(
                infer_size_from_argument(Some(&value), &ty!(Int), &variables),
                expected
            );
        }

        #[rstest::rstest]
        #[case::none_input(None)]
        #[case::string_value(Some(AstValue::String("not a size".into())))]
        #[case::boolean_value(Some(AstValue::Boolean(true)))]
        #[case::missing_variable(Some(AstValue::Variable(apollo_compiler::Name::new_unchecked(
            "missing"
        ))))]
        fn unsupported_values_return_none(#[case] value: Option<AstValue>) {
            assert_eq!(
                infer_size_from_argument(value.as_ref(), &ty!(Int), &Object::new()),
                None
            );
        }
    }

    /// Per GraphQL input coercion, a single value supplied for a list-typed argument is a list
    /// of one item. The declared type decides whether an integer is a count or a list element.
    mod list_typed_argument_coercion_tests {
        use apollo_compiler::ast::IntValue;
        use serde_json_bytes::json;

        use super::*;

        #[rstest::rstest]
        #[case::int_for_list(json!(1000000), ty!([Int!]!), Some(1))]
        #[case::int_for_nullable_list(json!(5), ty!([Int]), Some(1))]
        #[case::string_for_list(json!("abc"), ty!([ID!]!), Some(1))]
        #[case::object_for_list(json!({"a": 1}), ty!([SomeInput!]), Some(1))]
        #[case::array_for_list(json!([1, 2, 3]), ty!([Int!]!), Some(3))]
        #[case::null_for_list(json!(null), ty!([Int!]), None)]
        #[case::int_for_int(json!(5), ty!(Int!), Some(5))]
        #[case::negative_int_for_int(json!(-5), ty!(Int), Some(0))]
        fn variable_size_respects_declared_type(
            #[case] value: JsonValue,
            #[case] declared_type: Type,
            #[case] expected: Option<i32>,
        ) {
            assert_eq!(
                infer_size_from_variable(Some(&value), &declared_type),
                expected
            );
        }

        #[rstest::rstest]
        #[case::int_for_list(AstValue::Int(IntValue::new_parsed("5")), ty!([Int!]!), Some(1))]
        #[case::string_for_list(AstValue::String("abc".into()), ty!([ID!]!), Some(1))]
        #[case::null_for_list(AstValue::Null, ty!([Int!]), None)]
        #[case::int_for_int(AstValue::Int(IntValue::new_parsed("5")), ty!(Int!), Some(5))]
        fn argument_size_respects_declared_type(
            #[case] value: AstValue,
            #[case] declared_type: Type,
            #[case] expected: Option<i32>,
        ) {
            assert_eq!(
                infer_size_from_argument(Some(&value), &declared_type, &Object::new()),
                expected
            );
        }

        #[test]
        fn variable_reference_uses_declared_type() {
            let value = AstValue::Variable(apollo_compiler::Name::new_unchecked("ids"));
            let mut variables = Object::new();
            variables.insert("ids", json!(1000000));
            assert_eq!(
                infer_size_from_argument(Some(&value), &ty!([Int!]!), &variables),
                Some(1)
            );
            assert_eq!(
                infer_size_from_argument(Some(&value), &ty!(Int!), &variables),
                Some(1000000)
            );
        }
    }

    mod slicing_argument_tests {
        use apollo_compiler::Schema;

        use super::*;
        use crate::test_harness::tracing_test;

        const SCHEMA: &str = r#"
            type Query {
                search(first: Int, ids: [ID!], input: SearchInput): [String]
            }
            input SearchInput {
                query: String
                pagination: PaginationInput
            }
            input PaginationInput {
                first: Int!
                ids: [ID!]
            }
        "#;

        fn resolve(path: &str) -> Option<SlicingArgument> {
            let schema = Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
            let field = schema.type_field("Query", "search").unwrap();
            SlicingArgument::resolve(&schema, field, path)
        }

        #[rstest::rstest]
        #[case::argument("first", &[], ty!(Int))]
        #[case::list_argument("ids", &[], ty!([ID!]))]
        #[case::nested("input.pagination.first", &["pagination", "first"], ty!(Int!))]
        #[case::nested_list("input.pagination.ids", &["pagination", "ids"], ty!([ID!]))]
        fn resolves_declared_type_along_path(
            #[case] path: &str,
            #[case] nested_path: &[&str],
            #[case] declared_type: Type,
        ) {
            let resolved = resolve(path).unwrap();
            assert_eq!(resolved.nested_path, nested_path);
            assert_eq!(resolved.declared_type, declared_type);
        }

        #[rstest::rstest]
        #[case::empty("")]
        #[case::unknown_argument("nope")]
        #[case::unknown_input_field("input.nope")]
        #[case::unknown_nested_input_field("input.pagination.nope")]
        #[case::path_through_scalar("first.nope")]
        #[case::path_through_scalar_input_field("input.query.nope")]
        fn unresolvable_path_is_none(#[case] path: &str) {
            assert!(resolve(path).is_none());
        }

        #[test]
        fn unresolvable_paths_are_warned_and_dropped() {
            let _guard = tracing_test::dispatcher_guard();
            let schema = Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
            let field = schema.type_field("Query", "search").unwrap();
            let paths: IndexSet<String> = ["first", "input.nope", "ids"]
                .into_iter()
                .map(String::from)
                .collect();

            let resolved = SlicingArgument::from_strings(
                &schema,
                &apollo_compiler::name!("Query"),
                field,
                &paths,
            );

            let names: Vec<&str> = resolved
                .iter()
                .map(|arg| arg.argument_name.as_str())
                .collect();
            assert_eq!(names, ["first", "ids"]);
            assert!(tracing_test::logs_contain(
                "Slicing argument `input.nope` of @listSize on `Query.search` does not resolve"
            ));
        }
    }

    mod traverse_ast_value_tests {
        use apollo_compiler::Node;
        use apollo_compiler::ast::Value as AstValue;

        use super::traverse_ast_value;

        fn make_object(fields: Vec<(&str, AstValue)>) -> AstValue {
            AstValue::Object(
                fields
                    .into_iter()
                    .map(|(name, value)| {
                        (apollo_compiler::Name::new_unchecked(name), Node::new(value))
                    })
                    .collect(),
            )
        }

        #[test]
        fn empty_path_returns_value() {
            let value = AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("42"));
            assert!(matches!(
                traverse_ast_value(&value, &[] as &[&str]),
                Some(AstValue::Int(_))
            ));
        }

        #[test]
        fn single_level_traversal() {
            let value = make_object(vec![(
                "count",
                AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("10")),
            )]);
            let result = traverse_ast_value(&value, ["count"].as_slice());
            assert!(matches!(result, Some(AstValue::Int(_))));
        }

        #[test]
        fn nested_traversal() {
            let inner = make_object(vec![(
                "first",
                AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("5")),
            )]);
            let outer = make_object(vec![("pagination", inner)]);
            let result = traverse_ast_value(&outer, ["pagination", "first"].as_slice());
            assert!(matches!(result, Some(AstValue::Int(_))));
        }

        #[test]
        fn missing_field_returns_none() {
            let value = make_object(vec![(
                "other",
                AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("10")),
            )]);
            assert!(traverse_ast_value(&value, &["missing"]).is_none());
        }

        #[test]
        fn non_object_with_path_returns_none() {
            let value = AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("42"));
            assert!(traverse_ast_value(&value, &["field"]).is_none());
        }

        /// Edge case: empty segment in path won't match any field
        #[test]
        fn empty_segment_returns_none() {
            let value = make_object(vec![(
                "count",
                AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("10")),
            )]);
            // An empty string segment won't match "count"
            assert!(traverse_ast_value(&value, &[""]).is_none());
        }

        /// Edge case: path with empty segment in middle fails at that point
        #[test]
        fn empty_segment_in_middle_returns_none() {
            let inner = make_object(vec![(
                "first",
                AstValue::Int(apollo_compiler::ast::IntValue::new_parsed("5")),
            )]);
            let outer = make_object(vec![("pagination", inner)]);
            assert!(traverse_ast_value(&outer, &["pagination", "", "first"]).is_none());
        }
    }

    mod traverse_json_value_tests {
        use serde_json_bytes::json;

        use super::traverse_json_value;

        #[test]
        fn empty_path_returns_value() {
            let value = json!(42);
            assert_eq!(traverse_json_value(&value, &[] as &[&str]), Some(&value));
        }

        #[test]
        fn single_level_traversal() {
            let value = json!({"count": 10});
            let result = traverse_json_value(&value, ["count"].as_slice());
            assert_eq!(result, Some(&json!(10)));
        }

        #[test]
        fn nested_traversal() {
            let value = json!({"pagination": {"first": 5}});
            let result = traverse_json_value(&value, ["pagination", "first"].as_slice());
            assert_eq!(result, Some(&json!(5)));
        }

        #[test]
        fn deeply_nested_traversal() {
            let value = json!({"level1": {"level2": {"level3": {"count": 99}}}});
            let result =
                traverse_json_value(&value, ["level1", "level2", "level3", "count"].as_slice());
            assert_eq!(result, Some(&json!(99)));
        }

        #[test]
        fn missing_field_returns_none() {
            let value = json!({"other": 10});
            assert!(traverse_json_value(&value, &["missing"]).is_none());
        }

        #[test]
        fn non_object_with_path_returns_none() {
            let value = json!(42);
            assert!(traverse_json_value(&value, &["field"]).is_none());
        }

        #[test]
        fn partial_path_missing_returns_none() {
            let value = json!({"level1": {"other": 5}});
            assert!(traverse_json_value(&value, &["level1", "level2", "count"]).is_none());
        }

        /// Edge case: empty segment won't match any field
        #[test]
        fn empty_segment_returns_none() {
            let value = json!({"count": 10});
            assert!(traverse_json_value(&value, &[""]).is_none());
        }

        /// Edge case: path with empty segment in middle fails at that point
        #[test]
        fn empty_segment_in_middle_returns_none() {
            let value = json!({"pagination": {"first": 5}});
            assert!(traverse_json_value(&value, &["pagination", "", "first"]).is_none());
        }

        /// Edge case: whitespace in segment name won't match trimmed field names
        #[test]
        fn whitespace_segment_returns_none() {
            let value = json!({"count": 10});
            assert!(traverse_json_value(&value, &[" count"]).is_none());
        }

        /// Edge case: null values in the path
        #[test]
        fn null_value_in_path_returns_none() {
            let value = json!({"pagination": null});
            assert!(traverse_json_value(&value, &["pagination", "first"]).is_none());
        }

        /// Edge case: array in the path (not supported for simple traversal)
        #[test]
        fn array_value_in_path_returns_none() {
            let value = json!({"items": [{"first": 5}]});
            assert!(traverse_json_value(&value, &["items", "first"]).is_none());
        }
    }
}
