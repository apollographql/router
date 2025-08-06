// Ported composition tests from Apollo Federation JavaScript repository
// Original source: federation/composition-js/src/__tests__/compose.test.ts

mod authenticated_directive;
mod basic_composition;
mod directive_merging;
mod enum_types;
mod field_sharing;
mod inaccessible_directive;
mod input_types;
mod interface_object;
mod merge_validations;
mod requires_scopes_policy;
mod satisfiability_validation;
mod tag_directive;
mod type_references;
mod union_types;

// Re-export all helper functions from the composition module to eliminate duplication
pub(crate) use crate::composition::ServiceDefinition;
pub(crate) use crate::composition::assert_api_schema_snapshot;
pub(crate) use crate::composition::assert_composition_success;
pub(crate) use crate::composition::assert_error_contains;
pub(crate) use crate::composition::compose_as_fed2_subgraphs;
pub(crate) use crate::composition::error_messages;
