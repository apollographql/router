// Compare response shapes from a query plan and an input operation.

use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::executable::Field;

use super::response_shape::BoolExpr;
use super::response_shape::DefinitionVariant;
use super::response_shape::FieldSelectionKey;
use super::response_shape::NormalizedTypeCondition;
use super::response_shape::PossibleDefinitions;
use super::response_shape::PossibleDefinitionsPerTypeCondition;
use super::response_shape::ResponseShape;
use crate::schema::position::ObjectTypeDefinitionPosition;

#[derive(Debug, derive_more::Display)]
pub struct ComparisonError {
    description: String,
}

impl ComparisonError {
    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn new(description: String) -> ComparisonError {
        ComparisonError { description }
    }

    pub fn add_description(self: ComparisonError, description: &str) -> ComparisonError {
        ComparisonError {
            description: format!("{}\n{}", self.description, description),
        }
    }
}

macro_rules! check_match_eq {
    ($a:expr, $b:expr) => {
        if $a != $b {
            let message = format!(
                "mismatch between {} and {}:\nleft: {:?}\nright: {:?}",
                stringify!($a),
                stringify!($b),
                $a,
                $b,
            );
            return Err(ComparisonError::new(message));
        }
    };
}

/// Path-specific type constraints on top of GraphQL type conditions.
pub(crate) trait PathConstraint
where
    Self: Sized,
{
    /// Returns a new path constraint under the given type condition.
    fn under_type_condition(&self, type_cond: &NormalizedTypeCondition) -> Self;

    /// Returns a new path constraint for field's response shape.
    fn for_field(&self, representative_field: &Field) -> Result<Self, ComparisonError>;

    /// Is `ty` allowed under the path constraint?
    fn allows(&self, _ty: &ObjectTypeDefinitionPosition) -> bool;

    /// Is `defs` feasible under the path constraint?
    fn allows_any(&self, _defs: &PossibleDefinitions) -> bool;
}

struct DummyPathConstraint;

impl PathConstraint for DummyPathConstraint {
    fn under_type_condition(&self, _type_cond: &NormalizedTypeCondition) -> Self {
        DummyPathConstraint
    }

    fn for_field(&self, _representative_field: &Field) -> Result<Self, ComparisonError> {
        Ok(DummyPathConstraint)
    }

    fn allows(&self, _ty: &ObjectTypeDefinitionPosition) -> bool {
        true
    }

    fn allows_any(&self, _defs: &PossibleDefinitions) -> bool {
        true
    }
}

// Check if `this` is a subset of `other`.
pub fn compare_response_shapes(
    this: &ResponseShape,
    other: &ResponseShape,
) -> Result<(), ComparisonError> {
    let assumption = BoolExpr::always_true();
    compare_response_shapes_with_constraint(&DummyPathConstraint, &assumption, this, other)
}

/// Check if `this` is a subset of `other`, but also use the `PathConstraint` to ignore infeasible
/// type conditions in `other`.
/// - `assumption`: Boolean condition that is assumed to hold. This may affect the
///   interpretation of the `this` and `other` response shapes.
pub(crate) fn compare_response_shapes_with_constraint<T: PathConstraint>(
    path_constraint: &T,
    assumption: &BoolExpr,
    this: &ResponseShape,
    other: &ResponseShape,
) -> Result<(), ComparisonError> {
    // Note: `default_type_condition` is for display.
    //       Only response key and definitions are compared.
    this.iter().try_for_each(|(key, this_def)| {
        let Some(other_def) = other.get(key) else {
            // check this_def's type conditions are feasible under the path constraint.
            if !path_constraint.allows_any(this_def) {
                return Ok(());
            }
            return Err(ComparisonError::new(format!("missing response key: {key}")));
        };
        compare_possible_definitions(path_constraint, assumption, this_def, other_def)
            .map_err(|e| e.add_description(&format!("mismatch for response key: {key}")))
    })
}

/// Collect and merge all definitions applicable to the given type condition.
/// Returns `None` if no definitions are applicable.
pub(crate) fn collect_definitions_for_type_condition(
    defs: &PossibleDefinitions,
    filter_cond: &NormalizedTypeCondition,
) -> Result<Option<PossibleDefinitionsPerTypeCondition>, ComparisonError> {
    let mut filter_iter = defs
        .iter()
        .filter(|(type_cond, _)| filter_cond.implies(type_cond));
    let Some((_type_cond, first)) = filter_iter.next() else {
        return Ok(None);
    };
    let mut digest = first.clone();
    // Merge the rest of filter_iter into digest.
    filter_iter.try_for_each(|(type_cond, def)|
        def.conditional_variants()
            .iter()
            .try_for_each(|variant| digest.insert_variant(variant.clone()))
            .map_err(|e| {
                ComparisonError::new(format!(
                    "collect_definitions_for_type_condition failed for {filter_cond}\ntype_cond: {type_cond}\nerror: {e}",
                ))
            })
    )?;
    Ok(Some(digest))
}

fn path_constraint_allows_type_condition<T: PathConstraint>(
    path_constraint: &T,
    type_cond: &NormalizedTypeCondition,
) -> bool {
    type_cond
        .ground_set()
        .iter()
        .any(|ty| path_constraint.allows(ty))
}

fn detail_single_object_type_condition(type_cond: &NormalizedTypeCondition) -> String {
    let Some(ground_ty) = type_cond.ground_set().iter().next() else {
        return "".to_string();
    };
    if !type_cond.is_named_object_type() {
        format!(" (has single object type: {ground_ty})")
    } else {
        "".to_string()
    }
}

fn compare_possible_definitions<T: PathConstraint>(
    path_constraint: &T,
    assumption: &BoolExpr,
    this: &PossibleDefinitions,
    other: &PossibleDefinitions,
) -> Result<(), ComparisonError> {
    this.iter().try_for_each(|(this_cond, this_def)| {
        if !path_constraint_allows_type_condition(path_constraint, this_cond) {
            // Skip `this_cond` since it's not satisfiable under the path constraint.
            return Ok(());
        }

        let updated_constraint = path_constraint.under_type_condition(this_cond);

        // First try: Use the single exact match (common case).
        if let Some(other_def) = other.get(this_cond)
            && let Ok(result) = compare_possible_definitions_per_type_condition(
                &updated_constraint,
                assumption,
                this_def,
                other_def,
            )
        {
            return Ok(result);
        }
        // fall through

        // Second try: Collect all definitions implied by the `this_cond`.
        if let Some(other_def) = collect_definitions_for_type_condition(other, this_cond)? {
            let result = compare_possible_definitions_per_type_condition(
                &updated_constraint,
                assumption,
                this_def,
                &other_def,
            );
            match result {
                Ok(result) => return Ok(result),
                Err(err) => {
                    // See if we can case-split over ground set items.
                    if this_cond.ground_set().len() == 1 {
                        // Single object type has no other option. Stop and report the error.
                        let detail = detail_single_object_type_condition(this_cond);
                        return Err(err.add_description(&format!(
                            "mismatch for type condition: {this_cond}{detail}",
                        )));
                    }
                    // fall through
                }
            }
            // fall through
        };

        // Finally: Case-split over individual ground types.
        let ground_set_iter = this_cond.ground_set().iter();
        let mut ground_set_iter = ground_set_iter.filter(|ty| path_constraint.allows(ty));
        ground_set_iter.try_for_each(|ground_ty| {
            let filter_cond = NormalizedTypeCondition::from_object_type(ground_ty);
            let Some(other_def) = collect_definitions_for_type_condition(other, &filter_cond)?
            else {
                return Err(ComparisonError::new(format!(
                    "no definitions found for type condition: {this_cond} (case: {ground_ty})"
                )));
            };
            let updated_constraint = path_constraint.under_type_condition(&filter_cond);
            compare_possible_definitions_per_type_condition(
                &updated_constraint,
                assumption,
                this_def,
                &other_def,
            )
            .map_err(|e| {
                e.add_description(&format!(
                    "mismatch for type condition: {this_cond} (case: {ground_ty})"
                ))
            })
        })
    })
}

fn compare_possible_definitions_per_type_condition<T: PathConstraint>(
    path_constraint: &T,
    assumption: &BoolExpr,
    this: &PossibleDefinitionsPerTypeCondition,
    other: &PossibleDefinitionsPerTypeCondition,
) -> Result<(), ComparisonError> {
    compare_field_selection_key(this.field_selection_key(), other.field_selection_key()).map_err(
        |e| {
            e.add_description(
                "mismatch in field selection key of PossibleDefinitionsPerTypeCondition",
            )
        },
    )?;
    this.conditional_variants()
        .iter()
        .try_for_each(|this_variant| {
            solve_boolean_constraints(path_constraint, assumption, this_variant, other)
        })
}

/// Under the given `assumption` and `this_variant`'s clause, match `this_variant` against
/// `other`'s variants.
/// - Computes the coverage (OR of all applicable `other` variant conditions).
/// - Checks if `this_variant`'s condition implies the coverage.
/// - If so, collects and merges the covered variants for comparison.
fn solve_boolean_constraints<T: PathConstraint>(
    path_constraint: &T,
    assumption: &BoolExpr,
    this_variant: &DefinitionVariant,
    other: &PossibleDefinitionsPerTypeCondition,
) -> Result<(), ComparisonError> {
    let this_cond = this_variant.boolean_clause().and(assumption);
    if this_cond.is_unsatisfiable() {
        // This variant is infeasible. Skip.
        return Ok(());
    }

    // Compute coverage = OR of all other variant conditions
    let mut coverage = BoolExpr::unsatisfiable();
    for variant in other.conditional_variants() {
        coverage = coverage.or(variant.boolean_clause());
    }

    // Check if this_cond implies coverage
    if !this_cond.implies(&coverage) {
        return Err(ComparisonError::new(format!(
            "Failed to solve Boolean constraints: condition {this_cond} \
             is not covered by other variants (coverage: {coverage})"
        )));
    }

    // Collect all variants from `other` that are implied by this_cond and merge them.
    let Some(other_variant) = collect_variants_for_boolean_expr(other, &this_cond)? else {
        return Err(ComparisonError::new(format!(
            "no variants found for Boolean condition in solve_boolean_constraints: {this_cond}"
        )));
    };

    compare_definition_variant(path_constraint, &this_cond, this_variant, &other_variant).map_err(
        |e| {
            e.add_description(&format!(
                "mismatched variants\n\
                 - Assumption: {assumption}\n\
                 - this_clause: {this_clause}\n\
                 - Full condition: {this_cond}",
                this_clause = this_variant.boolean_clause()
            ))
        },
    )
}

/// Like `collect_variants_for_boolean_condition`, but filters using a `BoolExpr`.
/// A variant is included if `filter_expr AND variant.boolean_clause()` is satisfiable
/// (i.e., the two conditions are consistent — there exists an assignment satisfying both).
pub(crate) fn collect_variants_for_boolean_expr(
    defs: &PossibleDefinitionsPerTypeCondition,
    filter_expr: &BoolExpr,
) -> Result<Option<DefinitionVariant>, ComparisonError> {
    let mut matching_variants: Vec<&DefinitionVariant> = defs
        .conditional_variants()
        .iter()
        .filter(|variant| {
            // A variant is applicable if filter_expr and the variant's condition are consistent.
            !filter_expr.and(variant.boolean_clause()).is_unsatisfiable()
        })
        .collect();
    if matching_variants.is_empty() {
        return Ok(None);
    }
    let first = matching_variants.remove(0);
    let mut result_sub = first.sub_selection_response_shape().cloned();
    for variant in matching_variants {
        compare_representative_field(variant.representative_field(), first.representative_field())
            .map_err(|e| {
                e.add_description("mismatch in representative_field under definition variant")
            })?;
        match (&mut result_sub, variant.sub_selection_response_shape()) {
            (None, None) => {}
            (Some(result_sub), Some(variant_sub)) => {
                result_sub.merge_with(variant_sub).map_err(|e| {
                    ComparisonError::new(format!("failed to merge implied variants: {e}"))
                })?;
            }
            _ => {
                return Err(ComparisonError::new(
                    "mismatch in sub-selections of implied variants".to_string(),
                ));
            }
        }
    }
    Ok(Some(
        first.with_updated_fields(filter_expr.clone(), result_sub),
    ))
}

/// Precondition: this.boolean_clause() + hypothesis implies other.boolean_clause().
fn compare_definition_variant<T: PathConstraint>(
    path_constraint: &T,
    hypothesis: &BoolExpr,
    this: &DefinitionVariant,
    other: &DefinitionVariant,
) -> Result<(), ComparisonError> {
    // Note: `this.boolean_clause()` and `other.boolean_clause()` may not match due to the
    //       hypothesis on `this` or weaker condition on the `other`.
    compare_representative_field(this.representative_field(), other.representative_field())
        .map_err(|e| {
            e.add_description("mismatch in representative_field under definition variant")
        })?;
    match (
        this.sub_selection_response_shape(),
        other.sub_selection_response_shape(),
    ) {
        (None, None) => Ok(()),
        (Some(this_sub), Some(other_sub)) => {
            let field_constraint = path_constraint.for_field(this.representative_field())?;
            compare_response_shapes_with_constraint(
                &field_constraint,
                hypothesis,
                this_sub,
                other_sub,
            )
            .map_err(|e| {
                e.add_description(&format!(
                    "mismatch in response shape under definition variant: ---> {} if {}",
                    this.representative_field(),
                    this.boolean_clause()
                ))
            })
        }
        _ => Err(ComparisonError::new(
            "mismatch in compare_definition_variant".to_string(),
        )),
    }
}

fn compare_field_selection_key(
    this: &FieldSelectionKey,
    other: &FieldSelectionKey,
) -> Result<(), ComparisonError> {
    check_match_eq!(this.name, other.name);
    // Note: Arguments are expected to be normalized.
    check_match_eq!(this.arguments, other.arguments);
    Ok(())
}

pub(crate) fn compare_representative_field(
    this: &Field,
    other: &Field,
) -> Result<(), ComparisonError> {
    check_match_eq!(this.name, other.name);
    // Note: Arguments and directives are NOT normalized.
    if !same_ast_arguments(&this.arguments, &other.arguments) {
        return Err(ComparisonError::new(format!(
            "mismatch in representative field arguments: {:?} vs {:?}",
            this.arguments, other.arguments
        )));
    }
    if !same_directives(&this.directives, &other.directives) {
        return Err(ComparisonError::new(format!(
            "mismatch in representative field directives: {:?} vs {:?}",
            this.directives, other.directives
        )));
    }
    Ok(())
}

//==================================================================================================
// AST comparison functions

fn same_ast_argument_value(x: &ast::Value, y: &ast::Value) -> bool {
    match (x, y) {
        // Object fields may be in different order.
        (ast::Value::Object(x), ast::Value::Object(y)) => vec_matches_sorted_by(
            x,
            y,
            |(xx_name, _), (yy_name, _)| xx_name.cmp(yy_name),
            |(_, xx_val), (_, yy_val)| same_ast_argument_value(xx_val, yy_val),
        ),

        // Recurse into list items.
        (ast::Value::List(x), ast::Value::List(y)) => {
            vec_matches(x, y, |xx, yy| same_ast_argument_value(xx, yy))
        }

        _ => x == y, // otherwise, direct compare
    }
}

fn same_ast_argument(x: &ast::Argument, y: &ast::Argument) -> bool {
    x.name == y.name && same_ast_argument_value(&x.value, &y.value)
}

fn same_ast_arguments(x: &[Node<ast::Argument>], y: &[Node<ast::Argument>]) -> bool {
    vec_matches_sorted_by(
        x,
        y,
        |a, b| a.name.cmp(&b.name),
        |a, b| same_ast_argument(a, b),
    )
}

fn same_directives(x: &ast::DirectiveList, y: &ast::DirectiveList) -> bool {
    vec_matches_sorted_by(
        x,
        y,
        |a, b| a.name.cmp(&b.name),
        |a, b| a.name == b.name && same_ast_arguments(&a.arguments, &b.arguments),
    )
}

//==================================================================================================
// Vec comparison functions

fn vec_matches<T>(this: &[T], other: &[T], item_matches: impl Fn(&T, &T) -> bool) -> bool {
    this.len() == other.len()
        && std::iter::zip(this, other).all(|(this, other)| item_matches(this, other))
}

fn vec_matches_sorted_by<T: Clone>(
    this: &[T],
    other: &[T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    item_matches: impl Fn(&T, &T) -> bool,
) -> bool {
    let mut this_sorted = this.to_owned();
    let mut other_sorted = other.to_owned();
    this_sorted.sort_by(&compare);
    other_sorted.sort_by(&compare);
    vec_matches(&this_sorted, &other_sorted, item_matches)
}
