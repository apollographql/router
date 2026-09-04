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

#[cfg(test)]
mod tests {
    use itertools::Itertools;
    use serde_json_bytes::json;

    use super::Problem;
    use super::aggregate_apply_to_errors;
    use crate::connectors::JSONSelection;
    use crate::connectors::ProblemLocation;

    /// A tap inside `->map` fires once per element, so a mapping over a large
    /// array would otherwise report the same sentence hundreds of times.
    /// Aggregation collapses identical messages into one problem carrying the
    /// count, and array indices are deliberately not part of the grouping key,
    /// which is what lets repeats from different elements land in one bucket.
    #[test]
    fn repeated_messages_aggregate_into_one_problem_with_a_count() {
        let (value, errors) =
            JSONSelection::parse(r#"codes: rows->map(@.code->withError("unrecognized code:", @))"#)
                .unwrap()
                .apply_to(&json!({
                    "rows": [{ "code": 7 }, { "code": 7 }, { "code": 7 }],
                }));

        assert_eq!(value, Some(json!({ "codes": [7, 7, 7] })));

        let problems =
            aggregate_apply_to_errors(errors, ProblemLocation::Selection).collect::<Vec<Problem>>();

        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].message, "unrecognized code: 7");
        assert_eq!(problems[0].count, 3);
        assert_eq!(problems[0].location, ProblemLocation::Selection);
    }

    /// The counterpart to the test above: grouping is by message, so distinct
    /// messages stay distinct and the collapsing cannot hide anything.
    #[test]
    fn distinct_messages_aggregate_into_distinct_problems() {
        let (_, errors) =
            JSONSelection::parse(r#"codes: rows->map(@.code->withError("unrecognized code:", @))"#)
                .unwrap()
                .apply_to(&json!({
                    "rows": [{ "code": 7 }, { "code": 9 }, { "code": 7 }],
                }));

        let problems = aggregate_apply_to_errors(errors, ProblemLocation::Selection)
            .sorted_by_key(|problem| problem.message.clone())
            .collect::<Vec<Problem>>();

        assert_eq!(
            problems
                .iter()
                .map(|problem| (problem.message.as_str(), problem.count))
                .collect::<Vec<_>>(),
            vec![("unrecognized code: 7", 2), ("unrecognized code: 9", 1)],
        );
    }
}
