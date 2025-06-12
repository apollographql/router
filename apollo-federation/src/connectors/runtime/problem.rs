//! Mapping from a Connectors request or response to GraphQL

use std::collections::HashMap;

use crate::connectors::ApplyToError;
use itertools::Itertools;
use serde::Deserialize;
use serde::Serialize;

/// A mapping problem
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Problem {
    pub message: String,
    pub path: String,
    pub count: usize,
}

/// Aggregate a list of [`ApplyToError`] into [mapping problems](Problem)
pub fn aggregate_apply_to_errors(errors: &[ApplyToError]) -> Vec<Problem> {
    errors
        .iter()
        .fold(
            HashMap::default(),
            |mut acc: HashMap<(&str, String), usize>, err| {
                let path = err
                    .path()
                    .iter()
                    .map(|p| match p.as_u64() {
                        Some(_) => "@", // ignore array indices for grouping
                        None => p.as_str().unwrap_or_default(),
                    })
                    .join(".");

                acc.entry((err.message(), path))
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
                acc
            },
        )
        .iter()
        .map(|(key, &count)| Problem {
            message: key.0.to_string(),
            path: key.1.clone(),
            count,
        })
        .collect()
}
