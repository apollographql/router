use std::sync::LazyLock;

use apollo_compiler::Node;
use apollo_compiler::ast::Value;
use apollo_compiler::collections::HashSet;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::schema::Type;
use apollo_compiler::ty;
use itertools::Itertools;

use crate::schema::FederationSchema;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum ArgumentCompositionStrategy {
    Max,
    Min,
    // Sum,
    Intersection,
    Union,
    NullableAnd,
    NullableMax,
    NullableUnion,
    DnfConjunction,
}

pub(crate) static MAX_STRATEGY: LazyLock<MaxArgumentCompositionStrategy> =
    LazyLock::new(MaxArgumentCompositionStrategy::new);
pub(crate) static MIN_STRATEGY: LazyLock<MinArgumentCompositionStrategy> =
    LazyLock::new(MinArgumentCompositionStrategy::new);
// pub(crate) static SUM_STRATEGY: LazyLock<SumArgumentCompositionStrategy> =
//     LazyLock::new(SumArgumentCompositionStrategy::new);
pub(crate) static INTERSECTION_STRATEGY: LazyLock<IntersectionArgumentCompositionStrategy> =
    LazyLock::new(|| IntersectionArgumentCompositionStrategy {});
pub(crate) static UNION_STRATEGY: LazyLock<UnionArgumentCompositionStrategy> =
    LazyLock::new(|| UnionArgumentCompositionStrategy {});
pub(crate) static NULLABLE_AND_STRATEGY: LazyLock<NullableAndArgumentCompositionStrategy> =
    LazyLock::new(NullableAndArgumentCompositionStrategy::new);
pub(crate) static NULLABLE_MAX_STRATEGY: LazyLock<NullableMaxArgumentCompositionStrategy> =
    LazyLock::new(NullableMaxArgumentCompositionStrategy::new);
pub(crate) static NULLABLE_UNION_STRATEGY: LazyLock<NullableUnionArgumentCompositionStrategy> =
    LazyLock::new(|| NullableUnionArgumentCompositionStrategy {});
pub(crate) static DNF_CONJUNCTION_STRATEGY: LazyLock<DnfConjunctionArgumentCompositionStrategy> =
    LazyLock::new(|| DnfConjunctionArgumentCompositionStrategy {});

impl ArgumentCompositionStrategy {
    fn get_impl(&self) -> &dyn ArgumentComposition {
        match self {
            Self::Max => &*MAX_STRATEGY,
            Self::Min => &*MIN_STRATEGY,
            // Self::Sum => &*SUM_STRATEGY,
            Self::Intersection => &*INTERSECTION_STRATEGY,
            Self::Union => &*UNION_STRATEGY,
            Self::NullableAnd => &*NULLABLE_AND_STRATEGY,
            Self::NullableMax => &*NULLABLE_MAX_STRATEGY,
            Self::NullableUnion => &*NULLABLE_UNION_STRATEGY,
            Self::DnfConjunction => &*DNF_CONJUNCTION_STRATEGY,
        }
    }

    pub(crate) fn name(&self) -> &str {
        self.get_impl().name()
    }

    pub(crate) fn is_type_supported(
        &self,
        schema: &FederationSchema,
        ty: &Type,
    ) -> Result<(), String> {
        self.get_impl().is_type_supported(schema, ty)
    }

    /// Merges the argument values by the specified strategy.
    /// - `None` return value indicates that the merged value is undefined (meaning the argument
    ///   should be omitted).
    /// - PORT_NOTE: The JS implementation could handle `undefined` input values. However, in Rust,
    ///   undefined values should be omitted in `values`, instead.
    pub(crate) fn merge_values(&self, values: &[Value]) -> Option<Value> {
        self.get_impl().merge_values(values)
    }
}

//////////////////////////////////////////////////////////////////////////////
// Implementation of ArgumentCompositionStrategy's

/// Argument composition strategy for directives.
///
/// Determines how to compose the merged argument value from directive
/// applications across subgraph schemas.
pub(crate) trait ArgumentComposition {
    /// The name of the validator.
    fn name(&self) -> &str;
    /// Is the type `ty`  supported by this validator?
    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String>;
    /// Merges the argument values.
    /// - Assumes that schemas are validated by `is_type_supported`.
    /// - `None` return value indicates that the merged value is undefined (meaning the argument
    ///   should be omitted).
    /// - PORT_NOTE: The JS implementation could handle `undefined` input values. However, in Rust,
    ///   undefined values should be omitted in `values`, instead.
    fn merge_values(&self, values: &[Value]) -> Option<Value>;
}

#[derive(Clone)]
pub(crate) struct FixedTypeSupportValidator {
    pub(crate) supported_types: Vec<Type>,
}

impl FixedTypeSupportValidator {
    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        if self.supported_types.contains(ty) {
            return Ok(());
        }

        let expected_types: Vec<String> = self
            .supported_types
            .iter()
            .map(|ty| ty.to_string())
            .collect();
        Err(format!("type(s) {}", expected_types.join(", ")))
    }
}

fn support_any_non_null_array(ty: &Type) -> Result<(), String> {
    if !ty.is_non_null() || !ty.is_list() {
        Err("non-nullable list types of any type".to_string())
    } else {
        Ok(())
    }
}

/// Support for nullable or non-nullable list types of any type.
fn support_any_array(ty: &Type) -> Result<(), String> {
    if ty.is_list() {
        Ok(())
    } else {
        Err("list types of any type".to_string())
    }
}

/// Support for doubly nested non-nullable list types of any non-nullable type `[[Foo!]!]!`
fn support_any_non_null_nested_array(ty: &Type) -> Result<(), String> {
    if ty.is_non_null()
        && ty.is_list()
        && ty.item_type().is_non_null()
        && ty.item_type().is_list()
        && ty.item_type().item_type().is_non_null()
    {
        Ok(())
    } else {
        Err("non-nullable doubly nested list of any type".to_string())
    }
}

fn max_int_value<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    values
        .filter_map(|val| match val {
            // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
            Value::Int(i) => i.try_to_i32().ok().map(|i| (val, i)),
            _ => None, // Shouldn't happen
        })
        .max_by(|x, y| x.1.cmp(&y.1))
        .map(|(val, _)| val)
        .cloned()
        // PORT_NOTE: JS uses `Math.max` which returns `-Infinity` for empty values.
        //            Here, we use `i32::MIN`.
        .unwrap_or_else(|| Value::Int(i32::MIN.into()))
}

fn min_int_value<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    values
        .filter_map(|val| match val {
            // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
            Value::Int(i) => i.try_to_i32().ok().map(|i| (val, i)),
            _ => None, // Shouldn't happen
        })
        .min_by(|x, y| x.1.cmp(&y.1))
        .map(|(val, _)| val)
        .cloned()
        // PORT_NOTE: JS uses `Math.min` which returns `+Infinity` for empty values.
        //            Here, we use `i32::MAX` as default value.
        .unwrap_or_else(|| Value::Int(i32::MAX.into()))
}

fn union_list_values<'a>(values: impl Iterator<Item = &'a Value>) -> Value {
    // Each item in `values` must be a Value::List(...).
    // Using `IndexSet` to maintain order and uniqueness.
    // TODO: port `valueEquals` from JS to Rust
    let mut result = IndexSet::default();
    for val in values {
        result.extend(val.as_list().unwrap_or_default().iter().cloned());
    }
    Value::List(result.into_iter().collect())
}

/// Merge nullable values.
/// - Returns `None` if all values are `null` or values are empty.
/// - Otherwise, calls `merge_values` argument with all the non-null values and returns the result.
// NOTE: This function makes the assumption that for the directive argument
// being merged, it is not "nullable with non-null default" in the supergraph
// schema (this kind of type/default combo is confusing and should be avoided,
// if possible). This assumption allows this function to replace null with
// None, which makes for a cleaner supergraph schema.
fn merge_nullable_values(
    values: &[Value],
    merge_values: impl Fn(&[&Value]) -> Value,
) -> Option<Value> {
    // Filter out `null` values.
    let values = values.iter().filter(|v| !v.is_null()).collect::<Vec<_>>();
    if values.is_empty() {
        return None; // No values to merge, return None (instead of null)
    }
    merge_values(&values).into()
}

/// Performs conjunction of 2d arrays that represent conditions in Disjunctive Normal Form.
///
/// Each 2D array is interpreted as follows
///  * Inner array is interpreted as the conjunction (an AND) of the conditions in the array.
///  * Outer array is interpreted as the disjunction (an OR) of the inner arrays.
///
/// Algorithm
///  * filter out duplicate entries to limit the amount of necessary computations
///  * calculate cartesian product of the arrays to find all possible combinations
///    * simplify combinations by dropping duplicate conditions (i.e. p ^ p = p, p ^ q = q ^ p)
///  * eliminate entries that are subsumed by others (i.e. (p ^ q) subsumes (p ^ q ^ r))
fn dnf_conjunction(values: &[Value]) -> Value {
    // should never be the case
    if values.is_empty() {
        return Value::List(vec![]); // TODO should this be [[]] instead?
    }

    // copy the 2D arrays, as we'll be modifying them below (due to sorting)
    let mut candidates: Vec<Value> = vec![];
    for value in values.iter() {
        if let Value::List(disjunctions) = value {
            // Normally for DNF, you'd consider [] to be always false and [[]] to be always true,
            // and code that uses any()/all() needs no special-casing to work with these
            // definitions. However, router special-cases [] to also mean true, and so if we're
            // about to do any evaluation on DNFs, we need to do these conversions beforehand.
            if disjunctions.is_empty() {
                candidates.push(Value::List(vec![Node::new(Value::List(vec![]))]));
            } else {
                candidates.push(value.clone());
            }
        }
    }

    // we first filter out duplicate values from candidates
    // this avoids exponential computation of exactly the same conditions
    sort_nested_array(&mut candidates);
    let mut filtered = candidates.into_iter().unique();

    // initialize with first entry
    let mut result = filtered
        .next()
        .expect("At least a single DNF conjunction value should exist");

    // perform cartesian product to find all possible entries
    while let Some(current) = filtered.next() {
        let mut accumulator: Vec<Node<Value>> = Vec::new();
        let mut seen: HashSet<Value> = HashSet::default();

        if let (Value::List(final_disjunctions), Value::List(current_disjunctions)) =
            (&result, &current)
        {
            for final_disjunction in final_disjunctions {
                for current_disjunction in current_disjunctions {
                    if let (Value::List(final_conjunctions), Value::List(current_conjunctions)) =
                        (final_disjunction.as_ref(), current_disjunction.as_ref())
                    {
                        // filter out elements that are already present in final_disjunction
                        let filtered_conjunctions = current_conjunctions
                            .iter()
                            .filter(|e| !final_conjunctions.contains(e));
                        let mut candidate = final_conjunctions.clone();
                        candidate.extend(filtered_conjunctions.cloned());
                        candidate.sort_by_key(|c| c.to_string());

                        let candidate_value = Value::List(candidate);
                        // only add entries which has not been seen yet
                        if seen.insert(candidate_value.clone()) {
                            accumulator.push(Node::new(candidate_value));
                        }
                    }
                }
            }
            // now we need to deduplicate the results to avoid unnecessary computations
            result = deduplicate_subsumed_values(Value::List(accumulator));
        }
    }
    result
}

fn sort_nested_array(values: &mut Vec<Value>) {
    values.iter_mut().for_each(|value| {
        if let Value::List(disjunctions) = value {
            disjunctions.iter_mut().for_each(|disjunction| {
                if let Value::List(conjunctions) = disjunction.make_mut() {
                    // TODO should this also filter duplicates? ["A", "A"]?
                    conjunctions.sort_by_key(|c| c.to_string());
                }
            });
            disjunctions.sort_by_key(|c| c.to_string());
        }
    });
}

/// Deduplicate subsumed values from 2D arrays.
///
/// Given that
/// - outer array implies OR requirements
/// - inner array implies AND requirements
///
/// We can filter out any inner arrays that fully contain other inner arays, i.e.
///   A OR B OR (A AND B) OR (A AND B AND C) => A OR B
fn deduplicate_subsumed_values(value: Value) -> Value {
    let mut result: Vec<Node<Value>> = Vec::new();

    if let Value::List(mut disjunctions) = value {
        // we first sort by length as the longer ones might be dropped
        disjunctions.sort_by_key(|d| {
            if let Value::List(conjunctions) = d.as_ref() {
                conjunctions.len()
            } else {
                0
            }
        });

        for candidate in disjunctions {
            let mut redundant = false;
            if let Value::List(candidate_disjunctions) = candidate.as_ref() {
                let candidate_set: HashSet<&Node<Value>> =
                    HashSet::from_iter(candidate_disjunctions);

                for existing_entry in &result {
                    // if `existing_entry` is a subset of a `candidate` then it means `candidate` is redundant
                    if let Value::List(existing_disjunctions) = existing_entry.as_ref() {
                        if existing_disjunctions
                            .iter()
                            .all(|e| candidate_set.contains(e))
                        {
                            redundant = true;
                            break;
                        }
                    }
                }
            }
            if !redundant {
                result.push(candidate);
            }
        }
    }
    Value::List(result)
}

// MAX
#[derive(Clone)]
pub(crate) struct MaxArgumentCompositionStrategy {
    validator: FixedTypeSupportValidator,
}

impl MaxArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for MaxArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "MAX"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        max_int_value(values.iter()).into()
    }
}

// MIN
#[derive(Clone)]
pub(crate) struct MinArgumentCompositionStrategy {
    validator: FixedTypeSupportValidator,
}

impl MinArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for MinArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "MIN"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        min_int_value(values.iter()).into()
    }
}

// NOTE: This doesn't work today because directive applications are de-duped
// before being merged, we'd need to modify merge logic if we need this kind
// of behavior.
// SUM
// #[derive(Clone)]
// pub(crate) struct SumArgumentCompositionStrategy {
//     validator: FixedTypeSupportValidator,
// }

// impl SumArgumentCompositionStrategy {
//     fn new() -> Self {
//         Self {
//             validator: FixedTypeSupportValidator {
//                 supported_types: vec![ty!(Int!)],
//             },
//         }
//     }
// }

// impl ArgumentComposition for SumArgumentCompositionStrategy {
//     fn name(&self) -> &str {
//         "SUM"
//     }

//     fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
//         self.validator.is_type_supported(schema, ty)
//     }

//     fn merge_values(&self, values: &[Value]) -> Value {
//         values
//             .iter()
//             .map(|val| match val {
//                 // project the value to i32 (GraphQL `Int` scalar type is a signed 32-bit integer)
//                 Value::Int(i) => i.try_to_i32().unwrap_or(0),
//                 _ => 0, // Shouldn't happen
//             })
//             // Note: `.sum()` would panic if the sum overflows.
//             .fold(0_i32, |acc, i| acc.saturating_add(i))
//             .into()
//     }
// }

// INTERSECTION
#[derive(Clone)]
pub(crate) struct IntersectionArgumentCompositionStrategy {}

impl ArgumentComposition for IntersectionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "INTERSECTION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_non_null_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        // Each item in `values` must be a Value::List(...).
        let Some((first, rest)) = values.split_first() else {
            return Value::List(Vec::new()).into(); // Fallback for empty list
        };

        let mut result = first.as_list().unwrap_or_default().to_owned();
        for val in rest {
            // TODO: port `valueEquals` from JS to Rust
            let val_set: HashSet<&Value> = val
                .as_list()
                .unwrap_or_default()
                .iter()
                .map(|v| v.as_ref())
                .collect();
            result.retain(|result_item| val_set.contains(result_item.as_ref()));
        }
        Value::List(result).into()
    }
}

// UNION
#[derive(Clone)]
pub(crate) struct UnionArgumentCompositionStrategy {}

impl ArgumentComposition for UnionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "UNION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_non_null_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        union_list_values(values.iter()).into()
    }
}

// NULLABLE_AND
#[derive(Clone)]
pub(crate) struct NullableAndArgumentCompositionStrategy {
    type_validator: FixedTypeSupportValidator,
}

impl NullableAndArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            type_validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Boolean), ty!(Boolean!)],
            },
        }
    }
}

impl ArgumentComposition for NullableAndArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "NULLABLE_AND"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.type_validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        merge_nullable_values(values, |values| {
            values
                .iter()
                .all(|v| {
                    if let Value::Boolean(b) = v {
                        *b
                    } else {
                        false // shouldn't happen
                    }
                })
                .into()
        })
    }
}

// NULLABLE_MAX
#[derive(Clone)]
pub(crate) struct NullableMaxArgumentCompositionStrategy {
    type_validator: FixedTypeSupportValidator,
}

impl NullableMaxArgumentCompositionStrategy {
    fn new() -> Self {
        Self {
            type_validator: FixedTypeSupportValidator {
                supported_types: vec![ty!(Int), ty!(Int!)],
            },
        }
    }
}

impl ArgumentComposition for NullableMaxArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "NULLABLE_MAX"
    }

    fn is_type_supported(&self, schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        self.type_validator.is_type_supported(schema, ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        merge_nullable_values(values, |values| max_int_value(values.iter().copied()))
    }
}

// NULLABLE_UNION
#[derive(Clone)]
pub(crate) struct NullableUnionArgumentCompositionStrategy {}

impl ArgumentComposition for NullableUnionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "NULLABLE_UNION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        merge_nullable_values(values, |values| union_list_values(values.iter().copied()))
    }
}

// DNF_CONJUNCTION
#[derive(Clone)]
pub(crate) struct DnfConjunctionArgumentCompositionStrategy {}

impl ArgumentComposition for DnfConjunctionArgumentCompositionStrategy {
    fn name(&self) -> &str {
        "DNF_CONJUNCTION"
    }

    fn is_type_supported(&self, _schema: &FederationSchema, ty: &Type) -> Result<(), String> {
        support_any_non_null_nested_array(ty)
    }

    fn merge_values(&self, values: &[Value]) -> Option<Value> {
        dnf_conjunction(values).into()
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Node;
    use apollo_compiler::ast::Type;
    use apollo_compiler::ast::Value;

    use crate::schema::argument_composition_strategies::ArgumentComposition;
    use crate::schema::argument_composition_strategies::DnfConjunctionArgumentCompositionStrategy;
    use crate::schema::argument_composition_strategies::deduplicate_subsumed_values;
    use crate::schema::argument_composition_strategies::support_any_non_null_nested_array;

    #[test]
    fn verify_support_any_non_null_nested_array() {
        for unsupported_type in vec![
            "String",
            "String!",
            "[String]",
            "[String!]",
            "[String]!",
            "[String!]!",
            "[[String]]",
            "[[String!]]",
            "[[String]!]",
            "[[String!]!]",
            "[[String]]!",
            "[[String!]]!",
            "[[String]!]!", // this one is incorrectly allowed by JS
        ] {
            let _type = Type::parse(unsupported_type, "schema.graphql").expect("valid type");
            assert!(support_any_non_null_nested_array(&_type).is_err());
        }

        for supported_type in vec!["[[String!]!]!", "[[Foo!]!]!"] {
            let _type = Type::parse(supported_type, "schema.graphql").expect("valid type");
            assert!(support_any_non_null_nested_array(&_type).is_ok());
        }
    }

    #[test]
    fn verify_deduplicate_subsumed_values() {
        let value = parse_into_ast_value_list(vec![vec!["A", "B", "C"], vec!["A", "B"], vec!["A"]]);
        let result = deduplicate_subsumed_values(value);
        assert_eq!(parse_into_ast_value_list(vec![vec!["A"]]), result);

        let value = parse_into_ast_value_list(vec![
            vec!["A", "B"],
            vec!["A", "B", "C"],
            vec!["A", "B", "C", "D"],
            vec!["A", "B", "D"],
            vec!["A", "B"],
            vec!["A", "D"],
            vec!["A", "B"],
        ]);
        let result = deduplicate_subsumed_values(value);
        assert_eq!(
            parse_into_ast_value_list(vec![vec!["A", "B"], vec!["A", "D"]]),
            result
        );
    }

    #[test]
    fn dnf_conjunction_of_empty_values() {
        let strategy = DnfConjunctionArgumentCompositionStrategy {};
        let value = strategy
            .merge_values(&vec![])
            .expect("successfully computed DNF conjunction value");
        if let Value::List(result) = value {
            assert!(result.is_empty());
        } else {
            assert!(
                false,
                "DNF_conjunction result should have been a Value::List"
            );
        }
    }

    #[test]
    fn dnf_conjunction_of_multiple_lists() {
        let strategy = DnfConjunctionArgumentCompositionStrategy {};
        let values = parse_into_ast_vec_value_list(vec![
            vec![vec!["C", "B", "D"], vec!["B", "A"]],
            vec![vec!["A", "D"]],
            vec![vec!["A"]],
            vec![vec!["A"], vec!["B"]],
            vec![vec!["C", "B"]],
            vec![vec!["A", "D"]],
            vec![vec!["A", "A"]],
        ]);
        let result = strategy
            .merge_values(&values)
            .expect("computed DNF conjunction value");
        assert_eq!(
            parse_into_ast_value_list(vec![vec!["A", "B", "C", "D"]]),
            result
        );
    }

    fn parse_into_ast_vec_value_list(values: Vec<Vec<Vec<&str>>>) -> Vec<Value> {
        let mut result = vec![];
        // each outer_array is a specific directive application value of [[Policy!]!]! and/or [[Scope!]!]!
        for outer_array in values {
            result.push(parse_into_ast_value_list(outer_array));
        }
        result
    }

    fn parse_into_ast_value_list(value: Vec<Vec<&str>>) -> Value {
        // outer array is interpreted as the disjunction (an OR) of the inner arrays.
        let mut disjunctions = vec![];
        for inner_array in value {
            // inner array is interpreted as the conjunction (an AND) of the conditions in the array.
            let mut conjunctions = vec![];
            for value in inner_array {
                conjunctions.push(Node::new(Value::String(value.to_string())));
            }
            disjunctions.push(Node::new(Value::List(conjunctions)));
        }
        Value::List(disjunctions)
    }
}
