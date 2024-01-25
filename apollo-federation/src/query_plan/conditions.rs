use crate::error::FederationError;
use apollo_compiler::executable::DirectiveList;
use apollo_compiler::executable::Name;
use apollo_compiler::executable::Value;
use indexmap::map::Entry;
use indexmap::IndexMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Conditions {
    Variables(VariableConditions),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Condition {
    Variable(VariableCondition),
    Boolean(bool),
}

/// A list of variable conditions, represented as a map from variable names to whether that variable
/// is negated in the condition. We maintain the invariant that there's at least one condition (i.e.
/// the map is non-empty), and that there's at most one condition per variable name.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableConditions(Arc<IndexMap<Name, bool>>);

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VariableCondition {
    variable: Name,
    negated: bool,
}

impl Conditions {
    pub(crate) fn from_directives(directives: &DirectiveList) -> Result<Self, FederationError> {
        let mut variables = None;
        for directive in directives {
            let negated = match directive.name.as_str() {
                "include" => false,
                "skip" => true,
                _ => continue,
            };
            let value = directive.argument_by_name("if").ok_or_else(|| {
                FederationError::internal(format!(
                    "missing if argument on @{}",
                    if negated { "skip" } else { "include" },
                ))
            })?;
            match &**value {
                Value::Boolean(false) if !negated => return Ok(Self::Boolean(false)),
                Value::Boolean(true) if negated => return Ok(Self::Boolean(false)),
                Value::Boolean(_) => {}
                Value::Variable(name) => {
                    match variables
                        .get_or_insert_with(IndexMap::new)
                        .entry(name.clone())
                    {
                        Entry::Occupied(entry) => {
                            let previous_negated = *entry.get();
                            if previous_negated != negated {
                                return Ok(Self::Boolean(false));
                            }
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(negated);
                        }
                    }
                }
                _ => {
                    return Err(FederationError::internal(format!(
                        "expected boolean or variable `if` argument, got {value}",
                    )))
                }
            }
        }
        Ok(match variables {
            Some(map) => Self::Variables(VariableConditions(Arc::new(map))),
            None => Self::Boolean(true),
        })
    }

    pub(crate) fn merge(self, other: Self) -> Self {
        match (self, other) {
            // Absorbing element
            (Conditions::Boolean(false), _) | (_, Conditions::Boolean(false)) => {
                Conditions::Boolean(false)
            }

            // Neutral element
            (Conditions::Boolean(true), x) | (x, Conditions::Boolean(true)) => x,

            (Conditions::Variables(mut self_vars), Conditions::Variables(other_vars)) => {
                let vars = Arc::make_mut(&mut self_vars.0);
                for (name, other_negated) in other_vars.0.iter() {
                    match vars.entry(name.clone()) {
                        Entry::Occupied(entry) => {
                            let self_negated = entry.get();
                            if self_negated != other_negated {
                                return Conditions::Boolean(false);
                            }
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(*other_negated);
                        }
                    }
                }
                Conditions::Variables(self_vars)
            }
        }
    }
}
