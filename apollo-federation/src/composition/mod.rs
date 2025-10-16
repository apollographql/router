mod satisfiability;

use std::vec;

use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use itertools::Itertools;
use tracing::instrument;

pub use crate::composition::satisfiability::validate_satisfiability;
use crate::error::CompositionError;
use crate::merger::merge::Merger;
pub use crate::schema::schema_upgrader::upgrade_subgraphs_if_necessary;
use crate::schema::validators::root_fields::validate_consistent_root_fields;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Initial;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Upgraded;
use crate::subgraph::typestate::Validated;
pub use crate::supergraph::Merged;
pub use crate::supergraph::Satisfiable;
pub use crate::supergraph::Supergraph;

#[instrument(skip(subgraphs))]
pub fn compose(
    subgraphs: Vec<Subgraph<Initial>>,
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    tracing::debug!("Expanding subgraphs...");
    let expanded_subgraphs = expand_subgraphs(subgraphs)?;
    tracing::debug!("Upgrading subgraphs...");
    let mut upgraded_subgraphs = upgrade_subgraphs_if_necessary(expanded_subgraphs)?;
    tracing::debug!("Normalizing root types...");
    for subgraph in upgraded_subgraphs.iter_mut() {
        subgraph
            .normalize_root_types()
            .map_err(|e| e.to_composition_errors().collect_vec())?;
    }
    tracing::debug!("Validating subgraphs...");
    let validated_subgraphs = validate_subgraphs(upgraded_subgraphs)?;

    tracing::debug!("Pre-merge validations...");
    pre_merge_validations(&validated_subgraphs)?;
    tracing::debug!("Merging subgraphs...");
    let supergraph = merge_subgraphs(validated_subgraphs)?;
    tracing::debug!("Post-merge validations...");
    post_merge_validations(&supergraph)?;
    tracing::debug!("Validating satisfiability...");
    validate_satisfiability(supergraph)
}

/// Apollo Federation allow subgraphs to specify partial schemas (i.e. "import" directives through
/// `@link`). This function will update subgraph schemas with all missing federation definitions.
#[instrument(skip(subgraphs))]
pub fn expand_subgraphs(
    subgraphs: Vec<Subgraph<Initial>>,
) -> Result<Vec<Subgraph<Expanded>>, Vec<CompositionError>> {
    let mut errors: Vec<CompositionError> = vec![];
    let expanded: Vec<Subgraph<Expanded>> = subgraphs
        .into_iter()
        .map(|s| s.expand_links())
        .filter_map(|r| r.map_err(|e| errors.extend(e.to_composition_errors())).ok())
        .collect();
    if errors.is_empty() {
        Ok(expanded)
    } else {
        Err(errors)
    }
}

/// Validate subgraph schemas to ensure they satisfy Apollo Federation requirements (e.g. whether
/// `@key` specifies valid `FieldSet`s etc).
#[instrument(skip(subgraphs))]
pub fn validate_subgraphs(
    subgraphs: Vec<Subgraph<Upgraded>>,
) -> Result<Vec<Subgraph<Validated>>, Vec<CompositionError>> {
    let mut errors: Vec<CompositionError> = vec![];
    let validated: Vec<Subgraph<Validated>> = subgraphs
        .into_iter()
        .map(|s| s.validate())
        .filter_map(|r| r.map_err(|e| errors.extend(e.to_composition_errors())).ok())
        .collect();
    if errors.is_empty() {
        Ok(validated)
    } else {
        Err(errors)
    }
}

/// Perform validations that require information about all available subgraphs.
#[instrument(skip(subgraphs))]
pub fn pre_merge_validations(
    subgraphs: &[Subgraph<Validated>],
) -> Result<(), Vec<CompositionError>> {
    validate_consistent_root_fields(subgraphs)?;
    // TODO: (FED-713) Implement any pre-merge validations that require knowledge of all subgraphs.
    Ok(())
}

#[instrument(skip(subgraphs))]
pub fn merge_subgraphs(
    subgraphs: Vec<Subgraph<Validated>>,
) -> Result<Supergraph<Merged>, Vec<CompositionError>> {
    let merger = Merger::new(subgraphs, Default::default()).map_err(|e| {
        vec![CompositionError::InternalError {
            message: e.to_string(),
        }]
    })?;
    let result = merger.merge().map_err(|e| {
        vec![CompositionError::InternalError {
            message: e.to_string(),
        }]
    })?;
    if result.errors.is_empty() {
        let schema = result
            .supergraph
            .map(|s| s.into_inner().into_inner())
            .unwrap_or_else(Schema::new);
        let supergraph = Supergraph::with_hints(Valid::assume_valid(schema), result.hints);
        Ok(supergraph)
    } else {
        Err(result.errors)
    }
}

#[instrument(skip(_supergraph))]
pub fn post_merge_validations(
    _supergraph: &Supergraph<Merged>,
) -> Result<(), Vec<CompositionError>> {
    // TODO: (FED-714) Implement any post-merge validations other than satisfiability, which is
    // checked separately.
    Ok(())
}
