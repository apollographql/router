use apollo_compiler::executable::Name;
use indexmap::IndexMap;

#[derive(Debug, Clone)]
pub(crate) enum Conditions {
    Variables(VariableConditions),
    Boolean(bool),
}

#[derive(Debug, Clone)]
pub(crate) enum Condition {
    Variable(VariableCondition),
    Boolean(bool),
}

/// A list of variable conditions, represented as a map from variable names to whether that variable
/// is negated in the condition. We maintain the invariant that there's at least one condition (i.e.
/// the map is non-empty), and that there's at most one condition per variable name.
#[derive(Debug, Clone)]
pub(crate) struct VariableConditions(IndexMap<Name, bool>);

#[derive(Debug, Clone)]
pub(crate) struct VariableCondition {
    variable: Name,
    negated: bool,
}
