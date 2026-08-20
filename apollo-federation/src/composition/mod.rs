mod satisfiability;

use std::vec;

use tracing::instrument;

pub use crate::composition::satisfiability::validate_satisfiability;
use crate::error::CompositionError;
use crate::merger::merge::Merger;
pub use crate::schema::schema_upgrader::upgrade_subgraphs_if_necessary;
use crate::schema::validators::connectors::validate_override_on_connector;
use crate::schema::validators::root_fields::validate_consistent_root_fields;
use crate::subgraph::SubgraphError;
use crate::subgraph::typestate::Expanded;
use crate::subgraph::typestate::Initial;
use crate::subgraph::typestate::Subgraph;
use crate::subgraph::typestate::Validated;
pub use crate::supergraph::CompositionHint;
pub use crate::supergraph::Merged;
pub use crate::supergraph::Satisfiable;
pub use crate::supergraph::Supergraph;

#[derive(Debug)]
pub struct CompositionFailure {
    pub errors: Vec<CompositionError>,
    pub hints: Vec<CompositionHint>,
}

impl CompositionFailure {
    pub fn from_errors(errors: Vec<CompositionError>) -> Self {
        Self {
            errors,
            hints: Vec::new(),
        }
    }
}

impl From<SubgraphError> for CompositionFailure {
    fn from(error: SubgraphError) -> Self {
        Self::from_errors(error.to_composition_errors().collect())
    }
}

impl From<Vec<CompositionError>> for CompositionFailure {
    fn from(errors: Vec<CompositionError>) -> Self {
        Self::from_errors(errors)
    }
}

/// Options that configure composition. Mirrors the JS `CompositionOptions` interface
/// (see `composition-js/src/compose.ts`). Most fields are not yet ported — add them here
/// as needed.
#[derive(Debug, Default, Clone)]
pub struct CompositionOptions {
    /// Maximum allowable number of outstanding subgraph paths to validate during satisfiability.
    pub max_validation_subgraph_paths: Option<usize>,
}

/// Mirrors the JS `compose` function.
#[instrument(skip(subgraphs, options))]
pub fn compose(
    subgraphs: Vec<Subgraph<Initial>>,
    options: CompositionOptions,
) -> Result<Supergraph<Satisfiable>, CompositionFailure> {
    // explicitly sort subgraphs by their names
    // this was done automatically in JS as Subgraphs class stored subgraphs in OrderedMap (by name)
    let mut subgraphs = subgraphs;
    subgraphs.sort_by(|s1, s2| s1.name.cmp(&s2.name));

    // Hints raised before merging have nowhere to live yet — a `Supergraph` only exists from the
    // merge onwards, and a `CompositionFailure` is only built at the point of failure. So they are
    // gathered here and attached once there is something to attach them to.
    //
    // This is why `compose_subgraphs` is a separate function: it uses `?`, and every one of those
    // early returns would otherwise drop whatever had been gathered so far.
    let mut early_hints: Vec<CompositionHint> = vec![];

    let mut result = compose_subgraphs(subgraphs, options, &mut early_hints);
    match &mut result {
        Ok(supergraph) => prepend_hints(supergraph.hints_mut(), early_hints),
        Err(failure) => prepend_hints(&mut failure.hints, early_hints),
    }
    result
}

/// Puts `hints` in front of `target`, so hints stay in the order they were raised: everything from
/// the subgraph stages before anything from merging and satisfiability.
fn prepend_hints(target: &mut Vec<CompositionHint>, mut hints: Vec<CompositionHint>) {
    if hints.is_empty() {
        return;
    }
    hints.append(target);
    *target = hints;
}

/// Runs the composition pipeline, collecting any pre-merge hints into `hints`.
///
/// Split out from [`compose`] so that the `?`s below cannot drop those hints — see the note there.
fn compose_subgraphs(
    subgraphs: Vec<Subgraph<Initial>>,
    options: CompositionOptions,
    hints: &mut Vec<CompositionHint>,
) -> Result<Supergraph<Satisfiable>, CompositionFailure> {
    tracing::debug!("Expanding subgraphs...");
    let expanded_subgraphs = expand_subgraphs(subgraphs)?;

    tracing::debug!("Upgrading and validating subgraphs...");
    let validated_subgraphs = upgrade_subgraphs_if_necessary(expanded_subgraphs)?;
    for subgraph in &validated_subgraphs {
        hints.extend(subgraph.hints().iter().cloned());
    }

    tracing::debug!("Pre-merge validations...");
    pre_merge_validations(&validated_subgraphs)?;

    // Reads the subgraphs, but is only reported if merging succeeds — see
    // `validate_override_on_connector`. Merging consumes the subgraphs, hence running it here and
    // holding on to the result rather than doing both after the merge.
    let override_on_connector = validate_override_on_connector(&validated_subgraphs);

    tracing::debug!("Merging subgraphs...");
    let supergraph = merge_subgraphs(validated_subgraphs, &options)?;
    tracing::debug!("Post-merge validations...");
    post_merge_validations(&supergraph)?;
    override_on_connector.map_err(CompositionFailure::from_errors)?;

    tracing::debug!("Validating satisfiability...");
    validate_satisfiability(supergraph, &options)
}

/// Apollo Federation allow subgraphs to specify partial schemas (i.e. "import" directives through
/// `@link`). This function will update subgraph schemas with all missing federation definitions.
#[instrument(skip(subgraphs))]
pub fn expand_subgraphs(
    subgraphs: Vec<Subgraph<Initial>>,
) -> Result<Vec<Subgraph<Expanded>>, CompositionFailure> {
    let mut errors: Vec<CompositionError> = vec![];
    let expanded: Vec<Subgraph<Expanded>> = subgraphs
        .into_iter()
        .map(|s| s.expand_links())
        .filter_map(|r| r.map_err(|e| errors.extend(e.to_composition_errors())).ok())
        .collect();
    if errors.is_empty() {
        Ok(expanded)
    } else {
        Err(CompositionFailure::from_errors(errors))
    }
}

/// Perform validations that require information about all available subgraphs.
#[instrument(skip(subgraphs))]
pub fn pre_merge_validations(subgraphs: &[Subgraph<Validated>]) -> Result<(), CompositionFailure> {
    validate_consistent_root_fields(subgraphs).map_err(CompositionFailure::from_errors)?;
    // TODO: (FED-713) Implement any pre-merge validations that require knowledge of all subgraphs.
    Ok(())
}

#[instrument(skip(subgraphs, options))]
pub fn merge_subgraphs(
    subgraphs: Vec<Subgraph<Validated>>,
    options: &CompositionOptions,
) -> Result<Supergraph<Merged>, CompositionFailure> {
    let merger = Merger::new(subgraphs, options.clone()).map_err(|e| {
        CompositionFailure::from_errors(vec![CompositionError::InternalError {
            message: e.to_string(),
        }])
    })?;
    let result = merger.merge().map_err(|e| {
        CompositionFailure::from_errors(vec![CompositionError::InternalError {
            message: e.to_string(),
        }])
    })?;
    tracing::trace!(
        "Merge has {} errors and {} hints",
        result.errors.len(),
        result.hints.len()
    );
    if result.errors.is_empty() {
        let Some(supergraph_schema) = result.supergraph else {
            return Err(CompositionFailure::from_errors(vec![
                CompositionError::InternalError {
                    message: "Merge completed with no supergraph schema".to_string(),
                },
            ]));
        };
        let supergraph = Supergraph::with_hints(supergraph_schema, result.hints);
        Ok(supergraph)
    } else {
        Err(CompositionFailure {
            errors: result.errors,
            hints: result.hints,
        })
    }
}

#[instrument(skip(_supergraph))]
pub fn post_merge_validations(_supergraph: &Supergraph<Merged>) -> Result<(), CompositionFailure> {
    // TODO: (FED-714) Implement any post-merge validations other than satisfiability, which is
    // checked separately.
    Ok(())
}
