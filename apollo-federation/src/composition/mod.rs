mod satisfiability;

use std::sync::Arc;
use std::vec;

use tracing::instrument;

pub use crate::composition::satisfiability::validate_satisfiability;
use crate::connectors::Connector;
use crate::connectors::expand::Connectors;
use crate::connectors::expand::ExpansionResult;
use crate::connectors::expand::expand_connectors;
use crate::error::CompositionError;
use crate::merger::merge::Merger;
pub use crate::schema::schema_upgrader::upgrade_subgraphs_if_necessary;
use crate::schema::validators::root_fields::validate_consistent_root_fields;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Initial;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
pub use crate::supergraph::Merged;
pub use crate::supergraph::Satisfiable;
pub use crate::supergraph::Supergraph;

/// Mirrors the JS `compose` function.
#[instrument(skip(subgraphs))]
pub fn compose(
    subgraphs: Vec<Subgraph<Initial>>,
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    // explicitly sort subgraphs by their names
    // this was done automatically in JS as Subgraphs class stored subgraphs in OrderedMap (by name)
    let mut subgraphs = subgraphs;
    subgraphs.sort_by(|s1, s2| s1.name.cmp(&s2.name));

    tracing::debug!("Expanding subgraphs...");
    let expanded_subgraphs = expand_subgraphs(subgraphs)?;
    tracing::debug!("Upgrading subgraphs...");
    let validated_subgraphs = upgrade_subgraphs_if_necessary(expanded_subgraphs)?;

    tracing::debug!("Pre-merge validations...");
    pre_merge_validations(&validated_subgraphs)?;
    tracing::debug!("Merging subgraphs...");
    let supergraph = merge_subgraphs(validated_subgraphs)?;
    tracing::debug!("Post-merge validations...");
    post_merge_validations(&supergraph)?;
    tracing::debug!("Validating satisfiability...");
    validate_satisfiability(supergraph)
}

/// Mirrors the `HybridComposition::compose` from the apollo-composition crate.
pub fn compose_with_connectors(
    subgraphs: Vec<Subgraph<Initial>>,
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    // Pre-expand validation
    // - These were supposed to be pre-merge validations, but historically FBP performed these
    //   Rust-based validation, before JS composition.
    // - Once JS-to-Rust migration is done, we can move these to pre-merge validations.
    // TODO: (FED-855) Call `connectors::validation`, which may change the subgraphs before upgrading.

    tracing::debug!("Expanding subgraphs...");
    let expanded_subgraphs = expand_subgraphs(subgraphs)?;

    tracing::debug!("Upgrading subgraphs...");
    let validated_subgraphs = upgrade_subgraphs_if_necessary(expanded_subgraphs)?;

    tracing::debug!("Pre-merge validations...");
    pre_merge_validations(&validated_subgraphs)?;

    tracing::debug!("Merging subgraphs...");
    let supergraph = merge_subgraphs(validated_subgraphs)?;

    tracing::debug!("Post-merge validations...");
    post_merge_validations(&supergraph)?;
    // TODO: (FED-855) Call `validate_overrides`, which validates the original subgraphs for connectors after merging.
    // - Once JS-to-Rust migration is done, we may consider to move that to the pre-merge validation step.

    tracing::debug!("Validating satisfiability...");
    validate_satisfiability_with_connectors(supergraph)
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
    tracing::trace!(
        "Merge has {} errors and {} hints",
        result.errors.len(),
        result.hints.len()
    );
    if result.errors.is_empty() {
        let Some(supergraph_schema) = result.supergraph else {
            return Err(vec![CompositionError::InternalError {
                message: "Merge completed with no supergraph schema".to_string(),
            }]);
        };
        let supergraph = Supergraph::with_hints(supergraph_schema, result.hints);
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

/// Mirroring HybridComposition in the apollo-composition crate.
/// - Expand connectors as needed.
pub fn validate_satisfiability_with_connectors(
    supergraph: Supergraph<Merged>,
) -> Result<Supergraph<Satisfiable>, Vec<CompositionError>> {
    // Expand connectors for satisfiability validation.
    let supergraph_str = supergraph.schema().schema().to_string();
    let expansion_result = expand_connectors(&supergraph_str, &Default::default())
        .map_err(|e| vec![CompositionError::InternalError {
            message: format!("Composition failed due to an internal error when expanding connectors, please report this: {e}"),
        }])?;

    // Verify satisfiability
    match expansion_result {
        ExpansionResult::Expanded {
            raw_sdl,
            connectors: Connectors {
                by_service_name, ..
            },
            ..
        } => {
            let supergraph = Supergraph::parse(&raw_sdl).map_err(|e| {
                vec![CompositionError::InternalError {
                    message: e.to_string(),
                }]
            })?;
            let mut result = validate_satisfiability(supergraph);

            // Sanitize connectors subgraph names in errors and hints.
            match &mut result {
                Ok(supergraph) => {
                    for hint in supergraph.hints_mut() {
                        sanitize_connectors_message(&mut hint.message, by_service_name.iter());
                    }
                }
                Err(issues) => {
                    for issue in issues.iter_mut() {
                        sanitize_connectors_error(issue, by_service_name.iter());
                    }
                }
            }
            result
        }
        ExpansionResult::Unchanged => validate_satisfiability(supergraph),
    }
}

fn sanitize_connectors_error<'a>(
    issue: &mut CompositionError,
    connector_subgraphs: impl Iterator<Item = (&'a Arc<str>, &'a Connector)>,
) {
    match issue {
        CompositionError::SatisfiabilityError { message } => {
            sanitize_connectors_message(message, connector_subgraphs);
        }
        CompositionError::ShareableHasMismatchedRuntimeTypes { message } => {
            sanitize_connectors_message(message, connector_subgraphs);
        }
        _ => {}
    }
}

fn sanitize_connectors_message<'a>(
    message: &mut String,
    connector_subgraphs: impl Iterator<Item = (&'a Arc<str>, &'a Connector)>,
) {
    for (service_name, connector) in connector_subgraphs {
        *message = message.replace(&**service_name, connector.id.subgraph_name.as_str());
    }
}
