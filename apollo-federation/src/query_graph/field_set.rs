use crate::error::FederationError;
use apollo_compiler::executable::{FieldSet, SelectionSet};
use apollo_compiler::schema::NamedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{NodeStr, Schema};

// TODO: In the JS codebase, this optionally runs an additional validation to forbid aliases, and
// has some error-rewriting to help give the user better hints around non-existent fields.
pub(super) fn parse_field_set(
    schema: &Valid<Schema>,
    parent_type_name: NamedType,
    value: NodeStr,
) -> Result<SelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = FieldSet::parse_and_validate(
        schema,
        parent_type_name,
        value.as_str(),
        "field_set.graphql",
    )?;
    merge_selection_sets(&[&field_set.selection_set])
}

pub(super) fn merge_selection_sets(
    _selection_sets: &[&SelectionSet],
) -> Result<SelectionSet, FederationError> {
    // TODO: Once operation processing is done, we should be able to call into that logic here.
    // We're specifically wanting the equivalent of something like
    // ```
    // SelectionSetUpdates()
    //   .add(selectionSetOfNode(...))
    //   .add(selectionSetOfNode(...))
    //   ...
    //   .add(selectionSetOfNode(...))
    //   .toSelectionSetNode(...);
    // ```
    // from the JS codebase. It may be more performant for federation-next to use its own
    // representation instead of repeatedly inter-converting between its representation and the
    // apollo-rs one, but we'll cross that bridge if we come to it.
    //
    // Note this is unrelated to the concept of "normalization" in the JS codebase, which
    // specifically refers to the "SelectionSet.normalize()" method. The above is primarily useful
    // in merging fields/fragments with the same "key" (which could be thought of as some kind of
    // normalization, but is distinct from the kind of normalization in "SelectionSet.normalize()").
    todo!();
}

pub(super) fn equal_selection_sets(
    _a: &SelectionSet,
    _b: &SelectionSet,
) -> Result<bool, FederationError> {
    // TODO: Once operation processing is done, we should be able to call into that logic here.
    // We're specifically wanting the equivalent of something like
    // ```
    // selectionSetOfNode(...).equals(selectionSetOfNode(...));
    // ```
    // from the JS codebase. It may be more performant for federation-next to use its own
    // representation instead of repeatedly inter-converting between its representation and the
    // apollo-rs one, but we'll cross that bridge if we come to it.
    todo!();
}
