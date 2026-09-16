//! Static type and Boolean conditions extracted from selection sets.
//!
//! Port of `GraphQL/Theories/SelectionConditions.lean`. Every selection in a set carries a
//! *cumulative* condition: the intersection of the runtime types its enclosing inline fragments
//! admit, plus the conjunction of `@skip`/`@include` literals that gate it. Flattening the
//! condition onto each field occurrence is what lets the inclusion checker reason about one
//! response name at a time, without a condition-tree representation.
//!
//! Extraction stops at nested response-field boundaries: a field's own sub-selection stays raw
//! syntax and is extracted separately once the checker descends into it.
//!
//! Deviation from the Lean source: `extractFields` there threads an `inheritedBooleanCondition`
//! that both entry points (`ofSelectionSet`, `ofTypeRegion`) pass as empty. With an empty
//! inherited condition its `filter` and re-canonicalization steps are the identity, so they are
//! specialized away here.

use apollo_compiler::Name;
use apollo_compiler::ast;
use apollo_compiler::executable::Field;
use apollo_compiler::executable::Selection;

use super::error::ComparisonError;
use super::error::Mismatch;
use super::schema_view::SchemaView;

//==================================================================================================
// Boolean literals and conditions

/// A `@skip`/`@include` condition reduced to a signed variable. `Positive(x)` is
/// `@include(if: $x)`; `Negative(x)` is `@skip(if: $x)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BooleanLiteral {
    Positive(Name),
    Negative(Name),
}

impl BooleanLiteral {
    pub(crate) fn variable(&self) -> &Name {
        match self {
            BooleanLiteral::Positive(name) | BooleanLiteral::Negative(name) => name,
        }
    }

    pub(crate) fn required_value(&self) -> bool {
        matches!(self, BooleanLiteral::Positive(_))
    }

    fn complement(&self) -> BooleanLiteral {
        match self {
            BooleanLiteral::Positive(name) => BooleanLiteral::Negative(name.clone()),
            BooleanLiteral::Negative(name) => BooleanLiteral::Positive(name.clone()),
        }
    }

    /// Canonical order: variable name first, then positive before negative.
    fn ordered_before_or_equal(&self, other: &BooleanLiteral) -> bool {
        if self.variable() == other.variable() {
            self.required_value() || !other.required_value()
        } else {
            self.variable() <= other.variable()
        }
    }

    /// Is this literal true under `assignment`? An unassigned variable has no truth value here;
    /// callers only ask once every relevant variable is assigned.
    pub(crate) fn allows(&self, assignment: &Assignment) -> bool {
        assignment.get(self.variable()) == Some(self.required_value())
    }
}

impl std::fmt::Display for BooleanLiteral {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BooleanLiteral::Positive(name) => write!(f, "${name}"),
            BooleanLiteral::Negative(name) => write!(f, "¬${name}"),
        }
    }
}

/// A conjunction of literals. Canonical form is sorted and duplicate-free; the empty
/// conjunction is `true`. A positive/negative contradiction has no canonical form, which is
/// how unsatisfiable conditions are represented (`None`).
pub(crate) type BooleanCondition = Vec<BooleanLiteral>;

fn insert_literal(
    literal: &BooleanLiteral,
    condition: &[BooleanLiteral],
) -> Option<BooleanCondition> {
    let Some((candidate, rest)) = condition.split_first() else {
        return Some(vec![literal.clone()]);
    };
    if literal == candidate {
        Some(condition.to_vec())
    } else if literal.complement() == *candidate {
        None
    } else if literal.ordered_before_or_equal(candidate) {
        let mut result = Vec::with_capacity(condition.len() + 1);
        result.push(literal.clone());
        result.extend_from_slice(condition);
        Some(result)
    } else {
        let inserted = insert_literal(literal, rest)?;
        let mut result = Vec::with_capacity(inserted.len() + 1);
        result.push(candidate.clone());
        result.extend(inserted);
        Some(result)
    }
}

/// The assignments satisfying `condition` that do *not* satisfy `cover`, as disjoint clauses.
/// Each literal of `cover` that `condition` leaves open splits off the branch falsifying it.
fn subtract_boolean_condition(
    condition: &[BooleanLiteral],
    cover: &[BooleanLiteral],
) -> Vec<BooleanCondition> {
    let Some((literal, rest)) = cover.split_first() else {
        return Vec::new();
    };
    if condition.contains(&literal.complement()) {
        // `condition` already falsifies this literal, so nothing of it is covered.
        vec![condition.to_vec()]
    } else if condition.contains(literal) {
        subtract_boolean_condition(condition, rest)
    } else {
        let mut falsifying = vec![literal.complement()];
        falsifying.extend_from_slice(condition);
        let mut satisfying = vec![literal.clone()];
        satisfying.extend_from_slice(condition);
        let mut result = vec![falsifying];
        result.extend(subtract_boolean_condition(&satisfying, rest));
        result
    }
}

/// Does every assignment satisfying `condition` satisfy at least one clause of `covers`?
///
/// This is symbolic subtraction, not enumeration: `covers` is peeled off one clause at a time
/// and the check succeeds when nothing is left uncovered. It is what lets `@include(if: $x)`
/// together with `@skip(if: $x)` be recognized as jointly unconditional.
pub(crate) fn boolean_condition_covered_by(
    condition: &[BooleanLiteral],
    covers: &[BooleanCondition],
) -> bool {
    let mut uncovered = vec![condition.to_vec()];
    for cover in covers {
        uncovered = uncovered
            .iter()
            .flat_map(|clause| subtract_boolean_condition(clause, cover))
            .collect();
        if uncovered.is_empty() {
            return true;
        }
    }
    uncovered.is_empty()
}

//==================================================================================================
// Assignments

/// A partial map from `@skip`/`@include` variables to Boolean values. The checker extends one of
/// these as it case-splits, and never revisits a variable that is already bound.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assignment(Vec<(Name, bool)>);

impl Assignment {
    pub(crate) fn get(&self, variable: &Name) -> Option<bool> {
        self.0
            .iter()
            .find(|(name, _)| name == variable)
            .map(|(_, value)| *value)
    }

    pub(crate) fn extended(&self, variable: Name, value: bool) -> Assignment {
        let mut extended = self.0.clone();
        extended.push((variable, value));
        Assignment(extended)
    }
}

impl std::fmt::Display for Assignment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return write!(f, "true");
        }
        for (i, (name, value)) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, " ∧ ")?;
            }
            if *value {
                write!(f, "${name}")?;
            } else {
                write!(f, "¬${name}")?;
            }
        }
        Ok(())
    }
}

//==================================================================================================
// Conditions

/// The cumulative type and Boolean condition under which a selection is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Condition {
    /// The runtime object types still admitted at this point, in schema order.
    pub(crate) possible_types: Vec<Name>,
    pub(crate) boolean_condition: BooleanCondition,
}

impl Condition {
    /// Is this condition satisfied by `runtime_type` under `assignment`?
    pub(crate) fn allows(&self, assignment: &Assignment, runtime_type: &Name) -> bool {
        self.possible_types.contains(runtime_type)
            && self
                .boolean_condition
                .iter()
                .all(|literal| literal.allows(assignment))
    }
}

/// Intersection that keeps the left list's order, so that equivalent intersections reached by
/// different routes compare equal.
fn intersect_possible_types(left: &[Name], right: &[Name]) -> Vec<Name> {
    left.iter()
        .filter(|ty| right.contains(ty))
        .cloned()
        .collect()
}

/// The literals a modeled directive contributes. `None` means the directive never allows its
/// selection, making it infeasible.
///
/// An `if` argument that is neither a variable nor a Boolean cannot be resolved statically. It
/// behaves like `false` at runtime, so `@skip` becomes a no-op and `@include` becomes infeasible.
fn literals_for_directive(directive: &ast::Directive) -> Option<Vec<BooleanLiteral>> {
    let is_include = match directive.name.as_str() {
        "include" => true,
        "skip" => false,
        // Not a modeled directive: rejected before extraction, see `check_supported_directives`.
        _ => return Some(Vec::new()),
    };
    match directive.specified_argument_by_name("if").map(|arg| &**arg) {
        Some(ast::Value::Variable(name)) => Some(vec![if is_include {
            BooleanLiteral::Positive(name.clone())
        } else {
            BooleanLiteral::Negative(name.clone())
        }]),
        Some(ast::Value::Boolean(value)) => {
            if *value == is_include {
                Some(Vec::new())
            } else {
                None
            }
        }
        _ => {
            if is_include {
                None
            } else {
                Some(Vec::new())
            }
        }
    }
}

fn literals_for_directives(directives: &ast::DirectiveList) -> Option<Vec<BooleanLiteral>> {
    let mut literals = Vec::new();
    for directive in directives.iter() {
        literals.extend(literals_for_directive(directive)?);
    }
    Some(literals)
}

/// Narrows `start` by one inline-fragment type condition. `None` when the intersection is empty.
fn condition_under_type(
    schema: &SchemaView<'_>,
    start: &Condition,
    type_name: &Name,
) -> Option<Condition> {
    let possible_types =
        intersect_possible_types(&start.possible_types, schema.possible_types(type_name));
    if possible_types.is_empty() {
        return None;
    }
    Some(Condition {
        possible_types,
        boolean_condition: start.boolean_condition.clone(),
    })
}

/// Narrows `start` by a set of Boolean literals. `None` when they contradict what is already
/// assumed, making the selection infeasible.
fn condition_under_literals(start: &Condition, literals: &[BooleanLiteral]) -> Option<Condition> {
    let mut boolean_condition = start.boolean_condition.clone();
    for literal in literals {
        boolean_condition = insert_literal(literal, &boolean_condition)?;
    }
    Some(Condition {
        possible_types: start.possible_types.clone(),
        boolean_condition,
    })
}

//==================================================================================================
// Flat extraction

/// A field occurrence paired with the condition under which it is active. Modeled directives have
/// been absorbed into the condition; the field's own sub-selection stays raw.
#[derive(Debug, Clone)]
pub(crate) struct ConditionedField<'doc> {
    pub(crate) condition: Condition,
    pub(crate) field: &'doc Field,
}

impl<'doc> ConditionedField<'doc> {
    pub(crate) fn response_name(&self) -> &'doc Name {
        self.field.response_key()
    }
}

/// Rejects selections outside the modeled fragment before any verdict is produced.
///
/// The model covers `@skip` and `@include` only. Silently ignoring another directive would let
/// the checker answer a question it was not asked — `@defer` in particular changes what a
/// response contains — so an unmodeled directive is reported rather than dropped.
fn check_supported_directives(directives: &ast::DirectiveList) -> Result<(), ComparisonError> {
    for directive in directives.iter() {
        if directive.name != "skip" && directive.name != "include" {
            return Err(ComparisonError::new(Mismatch::UnsupportedDirective {
                name: directive.name.clone(),
            }));
        }
    }
    Ok(())
}

fn extract_selection<'doc>(
    schema: &SchemaView<'_>,
    current: &Condition,
    selection: &'doc Selection,
    out: &mut Vec<ConditionedField<'doc>>,
) -> Result<(), ComparisonError> {
    match selection {
        Selection::Field(field) => {
            check_supported_directives(&field.directives)?;
            let Some(literals) = literals_for_directives(&field.directives) else {
                return Ok(()); // never active
            };
            let Some(condition) = condition_under_literals(current, &literals) else {
                return Ok(()); // contradicts the enclosing condition
            };
            out.push(ConditionedField { condition, field });
        }
        Selection::InlineFragment(fragment) => {
            check_supported_directives(&fragment.directives)?;
            let mut condition = current.clone();
            // An inline fragment expands to its type condition first, then its directives.
            if let Some(type_condition) = &fragment.type_condition {
                let Some(narrowed) = condition_under_type(schema, &condition, type_condition)
                else {
                    return Ok(());
                };
                condition = narrowed;
            }
            let Some(literals) = literals_for_directives(&fragment.directives) else {
                return Ok(());
            };
            let Some(narrowed) = condition_under_literals(&condition, &literals) else {
                return Ok(());
            };
            for nested in &fragment.selection_set.selections {
                extract_selection(schema, &narrowed, nested, out)?;
            }
        }
        Selection::FragmentSpread(_) => {
            return Err(ComparisonError::new(Mismatch::UnsupportedFragmentSpread));
        }
    }
    Ok(())
}

fn extract_fields<'doc>(
    schema: &SchemaView<'_>,
    current: &Condition,
    selections: &[&'doc Selection],
    out: &mut Vec<ConditionedField<'doc>>,
) -> Result<(), ComparisonError> {
    selections
        .iter()
        .try_for_each(|selection| extract_selection(schema, current, selection, out))
}

/// Extracts one selection-set boundary rooted at a named composite type.
pub(crate) fn of_selection_set<'doc>(
    schema: &SchemaView<'_>,
    parent_type: &Name,
    selections: &[&'doc Selection],
) -> Result<Vec<ConditionedField<'doc>>, ComparisonError> {
    let root = Condition {
        possible_types: schema.possible_types(parent_type).to_vec(),
        boolean_condition: Vec::new(),
    };
    let mut out = Vec::new();
    extract_fields(schema, &root, selections, &mut out)?;
    Ok(out)
}

/// Extracts a boundary whose root is an exact set of possible object types rather than the
/// possible types of one named interface or union. This is what keeps a covariant field return
/// from being widened back to every implementation of its declared interface.
pub(crate) fn of_type_region<'doc>(
    schema: &SchemaView<'_>,
    region: &[Name],
    selections: &[&'doc Selection],
) -> Result<Vec<ConditionedField<'doc>>, ComparisonError> {
    let root = Condition {
        possible_types: region.to_vec(),
        boolean_condition: Vec::new(),
    };
    let mut out = Vec::new();
    extract_fields(schema, &root, selections, &mut out)?;
    Ok(out)
}
