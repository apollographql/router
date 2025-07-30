//! Mapping from a Connectors request or response to GraphQL

use std::collections::HashMap;

use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;

use crate::connectors::ApplyToError;
use crate::connectors::ProblemLocation;

/// A mapping problem
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Problem {
    pub message: String,
    pub path: String,
    pub count: usize,
    pub location: ProblemLocation,
}

/// Aggregate a list of [`ApplyToError`] into [mapping problems](Problem)
pub fn aggregate_apply_to_errors(
    errors: Vec<ApplyToError>,
    location: ProblemLocation,
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
        .map(move |((message, path), count)| Problem {
            message,
            path,
            count,
            location,
        })
}

/// Aggregate a list of [`ApplyToError`] into [mapping problems](Problem) while preserving [`ProblemLocation`]
pub fn aggregate_apply_to_errors_with_problem_locations(
    errors: Vec<(ProblemLocation, ApplyToError)>,
) -> impl Iterator<Item = Problem> {
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
        .flat_map(|(location, errors)| aggregate_apply_to_errors(errors, location))
}
