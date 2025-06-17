//! Mapping from a Connectors request or response to GraphQL

use std::collections::HashMap;

use apollo_federation::connectors::ApplyToError;
use apollo_federation::connectors::ProblemLocation;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;

/// A mapping problem
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub(crate) struct Problem {
    pub(crate) message: String,
    pub(crate) path: String,
    pub(crate) count: usize,
}

/// Aggregate a list of [`ApplyToError`] into [mapping problems](Problem)
pub(crate) fn aggregate_apply_to_errors(
    errors: Vec<ApplyToError>,
) -> impl Iterator<Item = Problem> {
    errors
        .into_iter()
        .fold(
            HashMap::default(),
            |mut acc: HashMap<(String, String), usize>, err| {
                let path = err
                    .path()
                    .iter()
                    .map(|p| match p.as_u64() {
                        Some(_) => "@", // ignore array indices for grouping
                        None => p.as_str().unwrap_or_default(),
                    })
                    .join(".");

                acc.entry((err.message().to_string(), path))
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
                acc
            },
        )
        .into_iter()
        .map(|((message, path), count)| Problem {
            message,
            path,
            count,
        })
}

/// Aggregate a list of [`ApplyToError`] into [mapping problems](Problem) while preserving [`ProblemLocation`]
pub(crate) fn aggregate_apply_to_errors_with_problem_locations(
    errors: Vec<(ProblemLocation, ApplyToError)>,
) -> impl Iterator<Item = (ProblemLocation, Problem)> {
    errors
        .into_iter()
        .fold(
            HashMap::new(),
            |mut acc: HashMap<ProblemLocation, Vec<ApplyToError>>, (loc, err)| {
                acc.entry(loc).or_default().push(err);
                acc
            },
        )
        .into_iter()
        .flat_map(|(location, errors)| {
            aggregate_apply_to_errors(errors).map(move |problem| (location, problem))
        })
}
