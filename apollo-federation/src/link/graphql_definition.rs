use std::fmt::Display;

use apollo_compiler::ast::Value;
use apollo_compiler::executable::Directive;
use apollo_compiler::executable::Name;
use apollo_compiler::name;
use apollo_compiler::Node;
use apollo_compiler::NodeStr;

use crate::error::FederationError;
use crate::link::argument::directive_optional_string_argument;
use crate::link::argument::directive_optional_variable_boolean_argument;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeferDirectiveArguments {
    label: Option<NodeStr>,
    if_: Option<BooleanOrVariable>,
}

impl DeferDirectiveArguments {
    pub(crate) fn label(&self) -> Option<&NodeStr> {
        self.label.as_ref()
    }
}

pub(crate) fn defer_directive_arguments(
    application: &Node<Directive>,
) -> Result<DeferDirectiveArguments, FederationError> {
    Ok(DeferDirectiveArguments {
        label: directive_optional_string_argument(application, &name!("label"))?,
        if_: directive_optional_variable_boolean_argument(application, &name!("if"))?,
    })
}

/// This struct is meant for recording the original structure/intent of `@skip`/`@include`
/// applications within the elements of a `GraphPath`. Accordingly, the order of them matters within
/// a `Vec`, and superfluous struct instances aren't elided; `Conditions` is the more appropriate
/// struct when trying to evaluate `@skip`/`@include` conditions (e.g. merging and short-circuiting
/// logic).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OperationConditional {
    pub(crate) kind: OperationConditionalKind,
    pub(crate) value: BooleanOrVariable,
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    strum_macros::Display,
    strum_macros::EnumIter,
    strum_macros::IntoStaticStr,
)]
pub(crate) enum OperationConditionalKind {
    #[strum(to_string = "include")]
    Include,
    #[strum(to_string = "skip")]
    Skip,
}

impl OperationConditionalKind {
    pub(crate) fn name(&self) -> Name {
        match self {
            OperationConditionalKind::Include => name!("include"),
            OperationConditionalKind::Skip => name!("skip"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum BooleanOrVariable {
    Boolean(bool),
    Variable(Name),
}

impl Display for BooleanOrVariable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BooleanOrVariable::Boolean(b) => b.fmt(f),
            BooleanOrVariable::Variable(name) => name.fmt(f),
        }
    }
}

impl BooleanOrVariable {
    pub(crate) fn to_ast_value(&self) -> Value {
        match self {
            BooleanOrVariable::Boolean(b) => Value::Boolean(*b),
            BooleanOrVariable::Variable(name) => Value::Variable(name.clone()),
        }
    }
}

impl From<BooleanOrVariable> for Value {
    fn from(b: BooleanOrVariable) -> Self {
        match b {
            BooleanOrVariable::Boolean(b) => Value::Boolean(b),
            BooleanOrVariable::Variable(name) => Value::Variable(name),
        }
    }
}

impl From<BooleanOrVariable> for Node<Value> {
    fn from(b: BooleanOrVariable) -> Self {
        Node::new(b.into())
    }
}
