//! Bounded property checks, runnable as `cargo test`.
//!
//! The deterministic runners in `runners/` are for campaigns; this is the same reasoning at a size
//! that finishes in seconds. It exists so `cargo test` in this package is a usable kill signal for
//! mutation testing: a defect seeded into `query_compare` should make one of these fail.
//!
//! None of these need the Lean oracle.

/// Deterministic byte strings spanning the grammar, shared by every property below.
///
/// Mostly *related* pairs, built by repeating the bytes the left operation consumed. A property
/// checked on a pair that already fails for an unrelated reason tests little: the verdict was
/// false before the edit and stays false. The interesting cases are where the two operations
/// nearly agree.
pub fn sample_inputs(count: usize) -> Vec<Vec<u8>> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 33) as u8
    };
    (0..count)
        .map(|_| {
            let length = 8 + (next() as usize % 24);
            let base: Vec<u8> = (0..length).map(|_| next()).collect();
            if next() % 4 == 0 {
                return base;
            }
            let split = crate::model::left_consumed(&base);
            let prefix = &base[..split];
            let mut related: Vec<u8> = prefix.iter().chain(prefix.iter()).copied().collect();
            if next() % 3 != 0 && !prefix.is_empty() {
                let at = split + (next() as usize % prefix.len());
                related[at] = related[at].wrapping_add(1 + next() % 7);
            }
            related
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use apollo_federation::correctness::query_compare;

    use super::*;
    use crate::harness::Harness;
    use crate::model;

    const CASES: usize = 1500;

    #[test]
    fn semantics_preserving_rewrites_keep_the_verdict() {
        let harness = Harness::new();
        for (index, bytes) in sample_inputs(CASES).iter().enumerate() {
            let case = model::rewritten_case(bytes, index as u64 + 1, 4);
            let (Some(base), Some(rewritten)) = (
                harness.prepare(&case.base),
                harness.prepare(&case.rewritten),
            ) else {
                continue;
            };
            for reversed in [false, true] {
                assert_eq!(
                    harness.run(&base, reversed).query_compare,
                    harness.run(&rewritten, reversed).query_compare,
                    "rewrites {:?} changed the verdict for {}",
                    case.applied,
                    hex(bytes)
                );
            }
        }
    }

    #[test]
    fn every_operation_includes_itself() {
        let harness = Harness::new();
        for bytes in sample_inputs(CASES) {
            let Some(case) = harness.prepare(&model::decode(&bytes)) else {
                continue;
            };
            for document in [&case.left, &case.right] {
                assert!(
                    query_compare::includes(harness.schema(), document, document).is_ok(),
                    "an operation did not include itself: {}",
                    hex(&bytes)
                );
            }
        }
    }

    #[test]
    fn a_larger_left_and_smaller_right_keep_inclusion() {
        let harness = Harness::new();
        for (index, bytes) in sample_inputs(CASES).iter().enumerate() {
            let case = model::weakened_case(bytes, index as u64 + 1, 4);
            let (Some(base), Some(weakened)) =
                (harness.prepare(&case.base), harness.prepare(&case.weakened))
            else {
                continue;
            };
            if harness.run(&base, false).query_compare {
                assert!(
                    harness.run(&weakened, false).query_compare,
                    "weakening broke a holding inclusion: {}",
                    hex(bytes)
                );
            }
        }
    }

    #[test]
    fn inclusion_is_transitive() {
        let harness = Harness::new();
        for bytes in sample_inputs(CASES) {
            let (a, b, c) = model::triple(&bytes);
            let pairs = [(a.clone(), b.clone()), (b, c.clone()), (a, c)];
            let prepared: Vec<_> = pairs
                .iter()
                .map(|(left, right)| {
                    harness.prepare(&model::Case {
                        left: left.clone(),
                        right: right.clone(),
                    })
                })
                .collect();
            let [Some(ab), Some(bc), Some(ac)] = prepared.as_slice() else {
                continue;
            };
            if harness.run(ab, false).query_compare && harness.run(bc, false).query_compare {
                assert!(
                    harness.run(ac, false).query_compare,
                    "inclusion is not transitive: {}",
                    hex(&bytes)
                );
            }
        }
    }

    /// The two Rust implementations decide the same predicate by different routes. They are not
    /// claimed to be equally complete, so a difference is only a failure when `query_compare`
    /// rejects for a reason the response-shape checker models at all.
    #[test]
    fn the_two_rust_checkers_agree() {
        let harness = Harness::new();
        for bytes in sample_inputs(CASES) {
            let Some(case) = harness.prepare(&model::decode(&bytes)) else {
                continue;
            };
            for reversed in [false, true] {
                let verdicts = harness.run(&case, reversed);
                if verdicts.reason == Some("VariableDeclarationMismatch") {
                    continue;
                }
                assert_eq!(
                    verdicts.query_compare,
                    verdicts.response_shape,
                    "the two Rust checkers disagree: {}",
                    hex(&bytes)
                );
            }
        }
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}
