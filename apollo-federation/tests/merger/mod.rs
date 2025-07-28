// Test modules for comprehensive composition testing
mod basic_composition;
mod error_scenarios;
mod federation_directives;
mod interface_handling;
mod merged_directives;
mod test_helpers;
mod type_merging;
mod type_specific;
mod validation;

// Re-export test helpers for use across test modules
pub use test_helpers::*;