// Compare response shapes from a query plan and an input operation.

use apollo_compiler::Name;
use apollo_compiler::Node;
use apollo_compiler::ast;
use apollo_compiler::collections::IndexSet;
use apollo_compiler::executable::Field;
use itertools::Itertools;

use super::response_shape::Clause;
use super::response_shape::DefinitionVariant;
use super::response_shape::FieldSelectionKey;
use super::response_shape::Literal;
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

/// The set of runtime object types that are possible at the current path position (the narrowed
/// type context). The comparison functions maintain this: it's narrowed by each type condition
/// (or case-split case) in effect, and re-derived across field boundaries via
/// `PathConstraint::for_field`.
#[derive(Debug, Clone)]
pub(crate) enum PossibleTypes {
    /// All types are possible (unconstrained).
    All,
    /// Only these object types are possible.
    Restricted(IndexSet<ObjectTypeDefinitionPosition>),
}

impl PossibleTypes {
    /// Is `ty` possible?
    pub(crate) fn allows(&self, ty: &ObjectTypeDefinitionPosition) -> bool {
        match self {
            PossibleTypes::All => true,
            PossibleTypes::Restricted(types) => types.contains(ty),
        }
    }

    /// Is any type in `ground_set` possible?
    fn allows_any_of(&self, ground_set: &[ObjectTypeDefinitionPosition]) -> bool {
        ground_set.iter().any(|ty| self.allows(ty))
    }

    /// Is any of `defs`' type conditions feasible?
    fn allows_any_definitions(&self, defs: &PossibleDefinitions) -> bool {
        defs.iter()
            .any(|(type_cond, _)| self.allows_any_of(type_cond.ground_set()))
    }

    /// Returns the possible types narrowed by the given type condition.
    fn under_type_condition(&self, type_cond: &NormalizedTypeCondition) -> Self {
        let ground_set = type_cond.ground_set().iter();
        PossibleTypes::Restricted(match self {
            PossibleTypes::All => ground_set.cloned().collect(),
            // Both the current possible types and the type condition apply here.
            // - Callers only use satisfiable type conditions, so the intersection is non-empty.
            PossibleTypes::Restricted(types) => ground_set
                .filter(|ty| types.contains(*ty))
                .cloned()
                .collect(),
        })
    }

    /// Returns the intersection of two sets of possible types.
    pub(crate) fn intersect(&self, other: &Self) -> Self {
        match (self, other) {
            (PossibleTypes::All, other) => other.clone(),
            (this, PossibleTypes::All) => this.clone(),
            (PossibleTypes::Restricted(these), PossibleTypes::Restricted(others)) => {
                PossibleTypes::Restricted(these.intersection(others).cloned().collect())
            }
        }
    }
}

/// Path-specific oracles for how a field changes the possible types of the response.
/// - The comparison functions themselves maintain the possible types (`PossibleTypes`) and
///   narrow them by type conditions; implementations only answer the schema-specific question
///   at field boundaries.
pub(crate) trait PathConstraint
where
    Self: Sized,
{
    /// Given the possible parent object types, returns the path constraint for the field's
    /// sub-selection scope along with the possible object types of the field's response.
    fn for_field(
        &self,
        representative_field: &Field,
        parent_types: &PossibleTypes,
    ) -> Result<(Self, PossibleTypes), ComparisonError>;
}

/// Conjunction of two path constraints: a runtime type is possible only if both constraints
/// allow it. This is how an extra oracle (e.g. `SubgraphConstraint`) is layered on top of the
/// base `SchemaConstraint`.
impl<A: PathConstraint, B: PathConstraint> PathConstraint for (A, B) {
    fn for_field(
        &self,
        representative_field: &Field,
        parent_types: &PossibleTypes,
    ) -> Result<(Self, PossibleTypes), ComparisonError> {
        let (a, a_types) = self.0.for_field(representative_field, parent_types)?;
        let (b, b_types) = self.1.for_field(representative_field, parent_types)?;
        Ok(((a, b), a_types.intersect(&b_types)))
    }
}

/// Check if `this` is a subset of `other`, but ignore `this`'s type conditions that are
/// infeasible under the possible types and the path constraint.
/// - `possible_types`: The possible runtime types at the current position.
/// - `assumption`: Boolean literals that are assumed to be true. This may affect the
///   interpretation of the `this` and `other` response shapes.
pub(crate) fn compare_response_shapes_with_constraint<T: PathConstraint>(
    path_constraint: &T,
    possible_types: &PossibleTypes,
    assumption: &Clause,
    this: &ResponseShape,
    other: &ResponseShape,
) -> Result<(), ComparisonError> {
    // Note: `default_type_condition` is for display.
    //       Only response key and definitions are compared.
    this.iter().try_for_each(|(key, this_def)| {
        let Some(other_def) = other.get(key) else {
            // check this_def's type conditions are feasible under the possible types.
            if !possible_types.allows_any_definitions(this_def) {
                return Ok(());
            }
            return Err(ComparisonError::new(format!("missing response key: {key}")));
        };
        compare_possible_definitions(
            path_constraint,
            possible_types,
            assumption,
            this_def,
            other_def,
        )
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
    possible_types: &PossibleTypes,
    assumption: &Clause,
    this: &PossibleDefinitions,
    other: &PossibleDefinitions,
) -> Result<(), ComparisonError> {
    this.iter().try_for_each(|(this_cond, this_def)| {
        if !possible_types.allows_any_of(this_cond.ground_set()) {
            // Skip `this_cond` since it's not satisfiable under the possible types.
            return Ok(());
        }

        let updated_types = possible_types.under_type_condition(this_cond);

        // First try: Use the single exact match (common case).
        if let Some(other_def) = other.get(this_cond)
            && let Ok(result) = compare_possible_definitions_per_type_condition(
                path_constraint,
                &updated_types,
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
                path_constraint,
                &updated_types,
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
        let mut ground_set_iter = ground_set_iter.filter(|ty| possible_types.allows(ty));
        ground_set_iter.try_for_each(|ground_ty| {
            let filter_cond = NormalizedTypeCondition::from_object_type(ground_ty);
            let Some(other_def) = collect_definitions_for_type_condition(other, &filter_cond)?
            else {
                return Err(ComparisonError::new(format!(
                    "no definitions found for type condition: {this_cond} (case: {ground_ty})"
                )));
            };
            let case_types = possible_types.under_type_condition(&filter_cond);
            compare_possible_definitions_per_type_condition(
                path_constraint,
                &case_types,
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
    possible_types: &PossibleTypes,
    assumption: &Clause,
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
            solve_boolean_constraints(
                path_constraint,
                possible_types,
                assumption,
                this_variant,
                other,
            )
        })
}

/// Under the given `assumption` and `this_variant`'s clause, match `this_variant` against
/// `other`'s variants.
/// - `this_variant` may match a set of `other`'s variants collectively, even if there are no
///   individual matching variant. Thus, this function tries to collect/merge all implied variants
///   and then compare.
/// - Note that we may need to case-split over Boolean variables. It happens when there are more
///   Boolean variables used in the `other`'s variants. Each variant's own variable set is tried
///   first (fewer hypotheses to check), then the union of all variable sets as a fallback, since
///   full coverage may require combining variables from different variants. For each set of
///   variables, it checks if every possible combination of Boolean values (hypothesis) has a
///   match.
fn solve_boolean_constraints<T: PathConstraint>(
    path_constraint: &T,
    possible_types: &PossibleTypes,
    assumption: &Clause,
    this_variant: &DefinitionVariant,
    other: &PossibleDefinitionsPerTypeCondition,
) -> Result<(), ComparisonError> {
    let Some(base_clause) = this_variant.boolean_clause().concatenate(assumption) else {
        // This variant is infeasible. Skip.
        return Ok(());
    };
    let hypothesis_groups = extract_boolean_hypotheses(&base_clause, other);
    // Try each hypothesis group and see if any one works
    let mut errors = Vec::new();
    for group in &hypothesis_groups {
        // In each group, every hypothesis must match.
        let result = group.iter().try_for_each(|hypothesis| {
            let Some(full_clause) = base_clause.concatenate(hypothesis) else {
                // Inconsistent hypothesis (a bug in extract_boolean_hypotheses)
                return Err(ComparisonError::new(format!(
                    "Internal error: inconsistent generated hypothesis {hypothesis}\n\
                     - assumption: {assumption}\n\
                     - this_clause: {this_clause}",
                     this_clause = this_variant.boolean_clause()
                )));
            };
            let Some(other_variant) = collect_variants_for_boolean_condition(other, &full_clause)? else {
                return Err(ComparisonError::new(format!(
                    "no variants found for Boolean condition in solve_boolean_constraints: {full_clause}"
                )));
            };
            compare_definition_variant(path_constraint, possible_types, &full_clause, this_variant, &other_variant)
                .map_err(|e| {
                    e.add_description(&format!(
                        "mismatched variants for hypothesis: {hypothesis}\n\
                         - Assumption: {assumption}\n\
                         - this_clause: {this_clause}\n\
                         - Full condition: {full_clause}",
                        this_clause = this_variant.boolean_clause())
                    )
                })
        });
        match result {
            Ok(()) => {
                return Ok(());
            }
            Err(e) => {
                let group_str = group.iter().join(", ");
                errors.push(format!(
                    "solve_boolean_constraints: group: {group_str}\n\
                    detail: {e}",
                ));
            }
        }
    }
    // None worked => error
    Err(ComparisonError::new(format!(
        "Failed to solve Boolean constraints w/ assumption {assumption}\n\
         this_variant: {this_variant}\n\
         other: {other}\n\
         detail: {}",
        errors.iter().join("\n")
    )))
}

/// A set of variable names.
/// Must be sorted by the variable name.
type BooleanVariables = Vec<Name>;

/// Generate sets of hypotheses to case-split over that are applicable to the target `defs`.
/// - Construct hypotheses based on the variables used in the Boolean conditions in `defs`.
/// - Excludes the literals in the `base_clause` since it's already assumed to be true.
/// - If there are variants with no extra Boolean variables, it will generate a no-hypothesis
///   group, which contains only one empty clause.
/// - When there are multiple distinct variable groups, the union of all groups is added as the
///   last (fallback) group. Individual variants' groups may each be insufficient when full
///   coverage requires combining variables from different variants (e.g. groups `[v0, v1]` and
///   `[v0, v2]` jointly covering all cases). The union group is the complete case-split: every
///   hypothesis fully determines each variant's condition, so it can verify any coverage the
///   smaller groups miss.
fn extract_boolean_hypotheses(
    base_clause: &Clause,
    defs: &PossibleDefinitionsPerTypeCondition,
) -> Vec<Vec<Clause>> {
    // Collect sets of variables that can be used to case-split over.
    let mut variable_groups = IndexSet::default();
    for variant in defs.conditional_variants() {
        let Some(remaining_condition) = variant.boolean_clause().subtract(base_clause) else {
            // Skip unsatisfiable variants.
            continue;
        };
        // Collect variables from the remaining condition.
        // Invariant: Clauses are expected to be sorted by the variable name.
        let vars: BooleanVariables = remaining_condition
            .literals()
            .iter()
            .map(|lit| lit.variable())
            .cloned()
            .collect();
        variable_groups.insert(vars);
    }
    // Add the union of all variable groups as the fallback group (see doc comment above).
    // With zero or one group, the union adds nothing new.
    if variable_groups.len() > 1 {
        let mut union_vars: BooleanVariables = variable_groups.iter().flatten().cloned().collect();
        union_vars.sort();
        union_vars.dedup();
        variable_groups.insert(union_vars); // no-op if some group already equals the union
    }
    // Generate groups of Boolean hypotheses.
    variable_groups
        .into_iter()
        .map(|group| generate_clauses(&group))
        .collect()
}

/// Generate all possible clauses from the given variables.
/// - If `vars` is empty, it will return a single empty clause.
fn generate_clauses(vars: &[Name]) -> Vec<Clause> {
    let mut state = Vec::new();
    let mut result = Vec::new();
    fn inner_generate(state: &mut Vec<Literal>, result: &mut Vec<Clause>, remaining_vars: &[Name]) {
        match remaining_vars {
            [] => {
                result.push(Clause::from_literals(state));
            }
            [var, rest @ ..] => {
                state.push(Literal::Pos(var.clone()));
                inner_generate(state, result, rest);
                state.pop();
                state.push(Literal::Neg(var.clone()));
                inner_generate(state, result, rest);
                state.pop();
            }
        }
    }
    inner_generate(&mut state, &mut result, vars);
    result
}

/// Collect all variants implied by the Boolean condition and merge them into one.
/// Returns `None` if no variants are applicable.
pub(crate) fn collect_variants_for_boolean_condition(
    defs: &PossibleDefinitionsPerTypeCondition,
    filter_cond: &Clause,
) -> Result<Option<DefinitionVariant>, ComparisonError> {
    let mut iter = defs
        .conditional_variants()
        .iter()
        .filter(|variant| filter_cond.implies(variant.boolean_clause()));
    let Some(first) = iter.next() else {
        return Ok(None);
    };
    let mut result_sub = first.sub_selection_response_shape().cloned();
    for variant in iter {
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
        first.with_updated_fields(filter_cond.clone(), result_sub),
    ))
}

/// Precondition: this.boolean_clause() + hypothesis implies other.boolean_clause().
fn compare_definition_variant<T: PathConstraint>(
    path_constraint: &T,
    possible_types: &PossibleTypes,
    hypothesis: &Clause,
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
            let (field_constraint, field_types) =
                path_constraint.for_field(this.representative_field(), possible_types)?;
            compare_response_shapes_with_constraint(
                &field_constraint,
                &field_types,
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
