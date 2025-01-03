// Compare response shapes from a query plan and an input operation.

use apollo_compiler::ast;
use apollo_compiler::executable::Field;
use apollo_compiler::Node;

use super::response_shape::DefinitionVariant;
use super::response_shape::FieldSelectionKey;
use super::response_shape::NormalizedTypeCondition;
use super::response_shape::PossibleDefinitions;
use super::response_shape::PossibleDefinitionsPerTypeCondition;
use super::response_shape::ResponseShape;
use crate::utils::FallibleIterator;

#[derive(Debug)]
pub struct MatchFailure {
    description: String,
}

impl MatchFailure {
    pub fn description(&self) -> &str {
        &self.description
    }

    fn new(description: String) -> MatchFailure {
        MatchFailure { description }
    }

    fn add_description(self: MatchFailure, description: &str) -> MatchFailure {
        MatchFailure {
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
            return Err(MatchFailure::new(message));
        }
    };
}

// Check if `this` is a subset of `other`.
pub fn compare_response_shapes(
    this: &ResponseShape,
    other: &ResponseShape,
) -> Result<(), MatchFailure> {
    // Note: `default_type_condition` is for display.
    //       Only response key and definitions are compared.
    this.iter().try_for_each(|(key, this_def)| {
        let other_def = other
            .get(key)
            .ok_or_else(|| MatchFailure::new(format!("missing response key: {key}")))?;
        compare_possible_definitions(this_def, other_def)
            .map_err(|e| e.add_description(&format!("mismatch for response key: {key}")))
    })
}

/// Merge all definitions applicable to the given type condition.
fn merge_definitions_for_type_condition(
    defs: &PossibleDefinitions,
    filter_cond: &NormalizedTypeCondition,
) -> Result<PossibleDefinitionsPerTypeCondition, MatchFailure> {
    let mut result: Option<PossibleDefinitionsPerTypeCondition> = None;
    for (type_cond, def) in defs.iter() {
        if filter_cond.implies(type_cond) {
            let result = result.get_or_insert_with(|| def.clone());
            def.conditional_variants()
                .iter()
                .try_for_each(|variant| result.insert_variant(variant.clone()))
                .map_err(|e| {
                    MatchFailure::new(format!(
                        "merge_definitions_for_type_condition failed for {filter_cond}\ntype_cond: {type_cond}\nerror: {e}",
                    ))
                })?;
        }
    }
    if let Some(result) = result {
        Ok(result)
    } else {
        Err(MatchFailure::new(format!(
            "no definitions found for type condition: {filter_cond}"
        )))
    }
}

fn compare_possible_definitions(
    this: &PossibleDefinitions,
    other: &PossibleDefinitions,
) -> Result<(), MatchFailure> {
    this.iter().try_for_each(|(this_cond, this_def)| {
        if let Some(other_def) = other.get(this_cond) {
            let result = compare_possible_definitions_per_type_condition(this_def, other_def);
            match result {
                Ok(result) => return Ok(result),
                Err(err) => {
                    // See if `this_cond` is an object (or non-abstract) type.
                    if this_cond.ground_set().len() == 1 {
                        // Concrete types can't be type blasted.
                        return Err(MatchFailure::new(format!(
                            "mismatch for type condition: {this_cond}\n{}",
                            err.description()
                        )));
                    }
                }
            }
            // fall through
        }

        // If there is no exact match for `this_cond`, try individual ground types.
        this_cond.ground_set().iter().try_for_each(|ground_ty| {
            let filter_cond = NormalizedTypeCondition::from_ground_type(ground_ty);
            let other_def =
                merge_definitions_for_type_condition(other, &filter_cond).map_err(|e| {
                    e.add_description(&format!(
                        "missing type condition: {this_cond} ({ground_ty})"
                    ))
                })?;
            compare_possible_definitions_per_type_condition(this_def, &other_def).map_err(|e| {
                e.add_description(&format!(
                    "mismatch for type condition: {this_cond} ({ground_ty})"
                ))
            })
        })
    })
}

fn compare_possible_definitions_per_type_condition(
    this: &PossibleDefinitionsPerTypeCondition,
    other: &PossibleDefinitionsPerTypeCondition,
) -> Result<(), MatchFailure> {
    compare_field_selection_key(this.field_selection_key(), other.field_selection_key()).map_err(
        |e| {
            e.add_description(
                "mismatch in field selection key of PossibleDefinitionsPerTypeCondition",
            )
        },
    )?;
    this.conditional_variants()
        .iter()
        .try_for_each(|this_def| {
            // search the same boolean clause in other
            let found = other
                .conditional_variants()
                .iter()
                .fallible_any(|other_def| {
                    if this_def.boolean_clause() != other_def.boolean_clause() {
                        Ok(false)
                    } else {
                        compare_definition_variant(this_def, other_def)?;
                        Ok(true)
                    }
                })?;
            if !found {
                Err(MatchFailure::new(
                    format!("mismatch in Boolean conditions of PossibleDefinitionsPerTypeCondition:\n expected clause: {}\ntarget definition: {:?}",
                            this_def.boolean_clause(),
                            other.conditional_variants(),
                    ),
                ))
            } else {
                Ok(())
            }
        })
}

fn compare_definition_variant(
    this: &DefinitionVariant,
    other: &DefinitionVariant,
) -> Result<(), MatchFailure> {
    compare_representative_field(this.representative_field(), other.representative_field())
        .map_err(|e| e.add_description("mismatch in field display under definition variant"))?;
    check_match_eq!(this.boolean_clause(), other.boolean_clause());
    match (
        this.sub_selection_response_shape(),
        other.sub_selection_response_shape(),
    ) {
        (None, None) => Ok(()),
        (Some(this_sub), Some(other_sub)) => {
            compare_response_shapes(this_sub, other_sub).map_err(|e| {
                e.add_description(&format!(
                    "mismatch in response shape under definition variant: ---> {} if {}",
                    this.representative_field(),
                    this.boolean_clause()
                ))
            })
        }
        _ => Err(MatchFailure::new(
            "mismatch in compare_definition_variant".to_string(),
        )),
    }
}

fn compare_field_selection_key(
    this: &FieldSelectionKey,
    other: &FieldSelectionKey,
) -> Result<(), MatchFailure> {
    check_match_eq!(this.name, other.name);
    // Note: Arguments are expected to be normalized.
    check_match_eq!(this.arguments, other.arguments);
    Ok(())
}

fn compare_representative_field(this: &Field, other: &Field) -> Result<(), MatchFailure> {
    check_match_eq!(this.name, other.name);
    // Note: Arguments and directives are NOT normalized.
    if !same_ast_arguments(&this.arguments, &other.arguments) {
        return Err(MatchFailure::new(format!(
            "mismatch in representative field arguments: {:?} vs {:?}",
            this.arguments, other.arguments
        )));
    }
    if !same_directives(&this.directives, &other.directives) {
        return Err(MatchFailure::new(format!(
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
        (ast::Value::Object(ref x), ast::Value::Object(ref y)) => vec_matches_sorted_by(
            x,
            y,
            |(xx_name, _), (yy_name, _)| xx_name.cmp(yy_name),
            |(_, xx_val), (_, yy_val)| same_ast_argument_value(xx_val, yy_val),
        ),

        // Recurse into list items.
        (ast::Value::List(ref x), ast::Value::List(ref y)) => {
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
