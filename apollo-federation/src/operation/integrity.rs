use super::NamedFragments;
use super::Operation;
use super::Selection;
use super::SelectionSet;
use crate::error::FederationError;
use crate::schema::position::CompositeTypeDefinitionPosition;
use crate::schema::ValidFederationSchema;

// `debug_check`: a debug-only sanity check.
// - Executes an expression `$result` that returns a `Result<(), E>` and returns the error if the
// result is an Err.
macro_rules! debug_check {
    ($result:expr) => {
        debug_assert_eq!((), $result?)
    };
}

// `debug_check_unwrap`: a debug-only sanity check taking a closure that returns a `Result<(), E>`.
// - Calls the closure and then unwraps it.
macro_rules! debug_check_unwrap {
    ($result:expr) => {
        debug_assert_eq!((), $result.unwrap())
    };
}

pub(crate) use debug_check;
pub(crate) use debug_check_unwrap;

//================================================================================================
// Well-formedness checks
// - structural invariant checks for operations.

// FragmentSpreadSelection holds a copy of fragment definition's selection set.
// This option controls whether or not to check that copy or skip it.
// Note that `Operation::optimize()` returns a selection set that may have outdated fragment
// selection sets. Since that does not affect the correctness of the query plan, we don't bother to
// update it. So, that part of check should be skipped, assuming the operation was well-formed
// before optimizing it.
#[derive(Clone, Copy)]
pub enum IsWellFormedOption {
    CheckFragmentSpreadSelectionSet,
    SkipFragmentSpreadSelectionSet,
}

impl Selection {
    pub fn is_well_formed(
        &self,
        schema: &ValidFederationSchema,
        named_fragments: &NamedFragments,
        parent_type: &CompositeTypeDefinitionPosition,
        option: IsWellFormedOption,
    ) -> Result<(), FederationError> {
        match self {
            Selection::Field(field) => {
                let field_data = field.field.data();
                if field_data.schema != *schema {
                    return Err(FederationError::internal(format!(
                        "Schema mismatch: expected {:?}, got {:?}",
                        schema, field_data.schema
                    )));
                }
                if field_data.field_position.try_get(schema.schema()).is_none() {
                    return Err(FederationError::internal(format!(
                        "Field not found: {field}",
                    )));
                }
                if let Some(selection_set) = &field.selection_set {
                    let base_type = field_data.output_base_type()?;
                    let sub_selection_set_type = base_type.try_into()?;
                    if selection_set.type_position != sub_selection_set_type {
                        return Err(FederationError::internal(format!(
                            "Selection set type position mismatch: expected {:?}, got {:?}",
                            sub_selection_set_type, selection_set.type_position
                        )));
                    }
                    selection_set.is_well_formed(schema, named_fragments, option)?;
                }
                Ok(())
            }
            Selection::FragmentSpread(fragment_spread) => {
                let fragment_data = fragment_spread.spread.data();
                if fragment_data.schema != *schema {
                    return Err(FederationError::internal(format!(
                        "Schema mismatch: expected {:?}, got {:?}",
                        schema, fragment_data.schema
                    )));
                }

                // Note: `fragment_spread.selection_set` should be rebased to the `schema` (either
                // supergraph or subgraph).
                if fragment_data.type_condition_position
                    != fragment_spread.selection_set.type_position
                {
                    return Err(FederationError::internal(format!(
                        "Fragment's type-condition ({:?}) and the type of its sub-selection-set ({:?}) mismatch.",
                        fragment_data.type_condition_position,
                        fragment_spread.selection_set.type_position
                    )));
                }

                let Some(fragment_def) = named_fragments.get(&fragment_data.fragment_name) else {
                    return Err(FederationError::internal(format!(
                        "Fragment name not found in the given set: {}",
                        fragment_data.fragment_name
                    )));
                };
                // Note: `named_fragments` should be rebased to the `schema` (either supergraph or subgraph).
                if fragment_def.schema != *schema {
                    return Err(FederationError::internal(format!(
                        "Fragment definition's schema mismatch: expected {:?}, got {:?}",
                        schema, fragment_def.schema
                    )));
                }
                if fragment_def.type_condition_position != fragment_data.type_condition_position {
                    return Err(FederationError::internal(format!(
                        "Fragment definition's type-condition mismatch: expected {:?}, got {:?}",
                        fragment_data.type_condition_position, fragment_def.type_condition_position
                    )));
                }

                if matches!(option, IsWellFormedOption::CheckFragmentSpreadSelectionSet) {
                    fragment_spread.selection_set.is_well_formed(
                        schema,
                        named_fragments,
                        option,
                    )?;
                }
                Ok(())
                // Note: fragment_data.type_condition_position and the parent type do not have to have
                // non-empty intersection to be well-formed. It would be an extra check.
            }
            Selection::InlineFragment(inline_fragment) => {
                let fragment_data = inline_fragment.inline_fragment.data();
                if fragment_data.schema != *schema {
                    return Err(FederationError::internal(format!(
                        "Schema mismatch: expected {:?}, got {:?}",
                        schema, fragment_data.schema
                    )));
                }
                if fragment_data.parent_type_position != *parent_type {
                    return Err(FederationError::internal(format!(
                        "Parent type mismatch: expected {:?}, got {:?}",
                        parent_type, fragment_data.parent_type_position
                    )));
                }
                if fragment_data.casted_type() != inline_fragment.selection_set.type_position {
                    return Err(FederationError::internal(format!(
                        "Inline fragment's casted-type ({:?}) and the type of its sub-selection-set ({:?}) mismatch.",
                        fragment_data.casted_type(),
                        inline_fragment.selection_set.type_position
                    )));
                }
                inline_fragment
                    .selection_set
                    .is_well_formed(schema, named_fragments, option)?;
                Ok(())
                // Note: fragment_data.type_condition_position and the parent type do not have to have
                // non-empty intersection to be well-formed. It would be an extra check.
            }
        }
    }
}

impl SelectionSet {
    pub fn is_well_formed(
        &self,
        schema: &ValidFederationSchema,
        named_fragments: &NamedFragments,
        option: IsWellFormedOption,
    ) -> Result<(), FederationError> {
        if self.schema != *schema {
            return Err(FederationError::internal(format!(
                "Schema mismatch: expected {:?}, got {:?}",
                schema, self.schema
            )));
        }
        for selection in self.iter() {
            selection.is_well_formed(schema, named_fragments, &self.type_position, option)?;
        }
        Ok(())
    }
}

impl Operation {
    pub fn is_well_formed(
        &self,
        schema: &ValidFederationSchema,
        option: IsWellFormedOption,
    ) -> Result<(), FederationError> {
        if self.schema != *schema {
            return Err(FederationError::internal(format!(
                "Schema mismatch: expected {:?}, got {:?}",
                schema, self.schema
            )));
        }
        self.selection_set
            .is_well_formed(schema, &self.named_fragments, option)?;
        Ok(())
    }
}
