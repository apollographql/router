use apollo_compiler::executable::Name;
use apollo_compiler::NodeStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeferDirectiveArguments {
    label: Option<NodeStr>,
    if_: Option<BooleanOrVariable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct OperationConditional {
    kind: OperationConditionalKind,
    value: BooleanOrVariable,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum OperationConditionalKind {
    Include,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum BooleanOrVariable {
    Boolean(bool),
    Variable(Name),
}
