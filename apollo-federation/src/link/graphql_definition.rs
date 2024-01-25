use apollo_compiler::executable::Name;
use apollo_compiler::NodeStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeferDirectiveArguments {
    label: Option<NodeStr>,
    if_: Option<BooleanOrVariable>,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum BooleanOrVariable {
    Boolean(bool),
    Variable(Name),
}
