use crate::error::FederationError;
use crate::query_plan::operation::{FragmentSpreadNormalizationOption, NormalizedSelectionSet};
use crate::schema::ValidFederationSchema;
use apollo_compiler::executable::{FieldSet, SelectionSet};
use apollo_compiler::schema::NamedType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{NodeStr, Schema};
use indexmap::IndexMap;

// TODO: In the JS codebase, this optionally runs an additional validation to forbid aliases, and
// has some error-rewriting to help give the user better hints around non-existent fields.
pub(super) fn parse_field_set(
    schema: &ValidFederationSchema,
    parent_type_name: NamedType,
    value: NodeStr,
) -> Result<NormalizedSelectionSet, FederationError> {
    // Note this parsing takes care of adding curly braces ("{" and "}") if they aren't in the
    // string.
    let field_set = FieldSet::parse_and_validate(
        schema.schema(),
        parent_type_name,
        value.as_str(),
        "field_set.graphql",
    )?;
    NormalizedSelectionSet::normalize_and_expand_fragments(
        &field_set.selection_set,
        &IndexMap::new(),
        schema,
        FragmentSpreadNormalizationOption::InlineFragmentSpread,
    )
}

/// This exists because there's a single callsite in extract_subgraphs_from_supergraph() that needs
/// to parse field sets before the schema has finished building. Outside that case, you should
/// always use `parse_field_set()` instead.
// TODO: As noted in the single callsite, ideally we could move the parsing to after extraction, but
// it takes time to determine whether that impacts correctness, so we're leaving it for later.
pub(super) fn parse_field_set_without_normalization(
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
    Ok(field_set.into_inner().selection_set)
}
