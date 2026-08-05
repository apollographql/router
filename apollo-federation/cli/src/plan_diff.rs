//! Source-aware query planning — Phase 0, Spike B: the `plan-diff` harness.
//!
//! Rev 2 of the source-aware proposal calls for a rebuilt plan-comparison
//! differ (the old `apollo-router/src/query_planner/plan_compare.rs`, removed
//! with the legacy JS planner in #6418, compared JS vs Rust router plans). This
//! is its source-aware successor: it plans one operation under two *modes* and
//! classifies the pair, so that later phases can assert parity between the
//! expansion planner and the source-aware planner over a corpus.
//!
//! This module holds the pure, unit-testable core — the [`Verdict`]
//! classification and a line-level [`structural_diff`]. The actual planning
//! (which needs a composed supergraph) lives in the `plan-diff` CLI command.
//!
//! ## Modes and the "mode B" seam
//!
//! [`PlanMode::Expansion`] is today's synthetic-subgraph planner.
//! [`PlanMode::SourceAware`] is the Phase-1 seam: the CLI wires it through the
//! same path but returns an explicit "not yet implemented" outcome, so the
//! entire harness — CLI surface, classification, reporting — is exercised and
//! stable *before* the source-aware planner exists. When Phase 1 lands, only
//! the planning step behind `SourceAware` changes; this classifier does not.
//!
//! ## Classification
//!
//! Equivalence uses the `apollo-federation` `correctness` engine
//! (`check_plan`): a plan is "correct" when its response shape is a subset of
//! the operation's. The caller runs that check per mode and passes the result
//! in as the `correctness` field of [`ModeOutcome::Planned`]; the classifier
//! stays free of federation internals and thus trivially testable.

use serde::Serialize;

/// Which planner produced (or would produce) a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanMode {
    /// Today's planner: connectors expanded to synthetic subgraphs.
    Expansion,
    /// The source-aware planner (Phase 1). Not yet implemented — the seam only.
    SourceAware,
}

impl std::fmt::Display for PlanMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanMode::Expansion => f.write_str("expansion"),
            PlanMode::SourceAware => f.write_str("source-aware"),
        }
    }
}

/// The result of planning one operation under one [`PlanMode`].
#[derive(Debug, Clone)]
pub enum ModeOutcome {
    /// Planning succeeded. `rendered` is the plan's canonical text form;
    /// `correctness` is `Ok` when the correctness engine accepted the plan
    /// against the operation, or `Err(reason)` when it rejected it.
    Planned {
        rendered: String,
        correctness: Result<(), String>,
    },
    /// Planning itself failed (or the mode is not yet implemented).
    Failed { reason: String },
}

/// How two modes' plans relate for a single operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Both modes produced the byte-identical plan text.
    Identical,
    /// Plans differ textually, but both are correct against the operation —
    /// interchangeable at execution time.
    Equivalent,
    /// Plans differ and their correctness verdicts diverge (or a plan the
    /// correctness engine rejects) — a genuine behavioral difference to
    /// investigate.
    Different,
    /// At least one mode failed to produce a plan.
    Error,
}

/// A single operation's diff result, ready to serialize into the JSON report.
#[derive(Debug, Clone, Serialize)]
pub struct PlanDiff {
    pub operation: String,
    pub left_mode: PlanMode,
    pub right_mode: PlanMode,
    pub verdict: Verdict,
    /// Human-readable explanation (failure reasons, correctness divergence).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Line-level diff of the two rendered plans, present only when they differ.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structural_diff: Option<String>,
}

/// Aggregate verdict tally over a corpus of operations, plus the per-operation
/// diffs — the JSON report shape for a CI parity run.
#[derive(Debug, Clone, Serialize)]
pub struct CorpusReport {
    pub total: usize,
    pub identical: usize,
    pub equivalent: usize,
    pub different: usize,
    pub error: usize,
    pub diffs: Vec<PlanDiff>,
}

impl CorpusReport {
    pub fn from_diffs(diffs: Vec<PlanDiff>) -> Self {
        let mut report = CorpusReport {
            total: diffs.len(),
            identical: 0,
            equivalent: 0,
            different: 0,
            error: 0,
            diffs: Vec::new(),
        };
        for diff in &diffs {
            match diff.verdict {
                Verdict::Identical => report.identical += 1,
                Verdict::Equivalent => report.equivalent += 1,
                Verdict::Different => report.different += 1,
                Verdict::Error => report.error += 1,
            }
        }
        report.diffs = diffs;
        report
    }

    /// True when no operation diverged or errored — the CI-green condition.
    pub fn all_ok(&self) -> bool {
        self.different == 0 && self.error == 0
    }
}

/// Classify one operation's pair of [`ModeOutcome`]s into a [`Verdict`].
///
/// This is the whole decision procedure, kept pure so it can be unit-tested
/// without composing a supergraph.
pub fn classify(
    operation: impl Into<String>,
    left_mode: PlanMode,
    right_mode: PlanMode,
    left: &ModeOutcome,
    right: &ModeOutcome,
) -> PlanDiff {
    let operation = operation.into();
    let base = |verdict, detail, structural_diff| PlanDiff {
        operation: operation.clone(),
        left_mode,
        right_mode,
        verdict,
        detail,
        structural_diff,
    };

    match (left, right) {
        (ModeOutcome::Failed { reason }, ModeOutcome::Failed { reason: other }) => base(
            Verdict::Error,
            Some(format!(
                "{left_mode} failed: {reason}\n{right_mode} failed: {other}"
            )),
            None,
        ),
        (ModeOutcome::Failed { reason }, _) => base(
            Verdict::Error,
            Some(format!("{left_mode} failed: {reason}")),
            None,
        ),
        (_, ModeOutcome::Failed { reason }) => base(
            Verdict::Error,
            Some(format!("{right_mode} failed: {reason}")),
            None,
        ),
        (
            ModeOutcome::Planned {
                rendered: left_plan,
                correctness: left_correct,
            },
            ModeOutcome::Planned {
                rendered: right_plan,
                correctness: right_correct,
            },
        ) => {
            if left_plan == right_plan {
                return base(Verdict::Identical, None, None);
            }

            let diff = structural_diff(left_plan, right_plan);
            match (left_correct, right_correct) {
                (Ok(()), Ok(())) => base(Verdict::Equivalent, None, Some(diff)),
                _ => {
                    let mut detail = String::new();
                    if let Err(e) = left_correct {
                        detail.push_str(&format!("{left_mode} correctness: {e}\n"));
                    }
                    if let Err(e) = right_correct {
                        detail.push_str(&format!("{right_mode} correctness: {e}\n"));
                    }
                    if detail.is_empty() {
                        detail.push_str(
                            "plans differ and could not be confirmed equivalent by the correctness engine",
                        );
                    }
                    base(
                        Verdict::Different,
                        Some(detail.trim_end().to_string()),
                        Some(diff),
                    )
                }
            }
        }
    }
}

/// A compact line-level diff of two rendered plans, using an LCS so unchanged
/// lines stay aligned. Removed lines are prefixed `-`, added lines `+`, context
/// lines a single space. Adequate for a spike report; the richer Parallel-as-set
/// / selection-sorting structural rules from the old `plan_compare.rs`
/// (`232f40da4^`) are the natural next refinement.
pub fn structural_diff(left: &str, right: &str) -> String {
    let left_lines: Vec<&str> = left.lines().collect();
    let right_lines: Vec<&str> = right.lines().collect();

    // Longest common subsequence over lines (classic DP table).
    let (n, m) = (left_lines.len(), right_lines.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if left_lines[i] == right_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut out = String::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if left_lines[i] == right_lines[j] {
            out.push_str(&format!("  {}\n", left_lines[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push_str(&format!("- {}\n", left_lines[i]));
            i += 1;
        } else {
            out.push_str(&format!("+ {}\n", right_lines[j]));
            j += 1;
        }
    }
    for line in &left_lines[i..] {
        out.push_str(&format!("- {line}\n"));
    }
    for line in &right_lines[j..] {
        out.push_str(&format!("+ {line}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(rendered: &str, correct: Result<(), &str>) -> ModeOutcome {
        ModeOutcome::Planned {
            rendered: rendered.to_string(),
            correctness: correct.map_err(|e| e.to_string()),
        }
    }

    #[test]
    fn identical_plans() {
        let d = classify(
            "Op",
            PlanMode::Expansion,
            PlanMode::Expansion,
            &planned("QueryPlan { Fetch(a) }", Ok(())),
            &planned("QueryPlan { Fetch(a) }", Ok(())),
        );
        assert_eq!(d.verdict, Verdict::Identical);
        assert!(d.structural_diff.is_none());
    }

    #[test]
    fn different_text_but_both_correct_is_equivalent() {
        let d = classify(
            "Op",
            PlanMode::Expansion,
            PlanMode::SourceAware,
            &planned("QueryPlan { Fetch(a) }", Ok(())),
            &planned("QueryPlan { Fetch(b) }", Ok(())),
        );
        assert_eq!(d.verdict, Verdict::Equivalent);
        assert!(d.structural_diff.is_some());
        assert!(d.detail.is_none());
    }

    #[test]
    fn correctness_divergence_is_different() {
        let d = classify(
            "Op",
            PlanMode::Expansion,
            PlanMode::SourceAware,
            &planned("QueryPlan { Fetch(a) }", Ok(())),
            &planned("QueryPlan { Fetch(b) }", Err("response shape mismatch")),
        );
        assert_eq!(d.verdict, Verdict::Different);
        assert!(d.detail.unwrap().contains("response shape mismatch"));
    }

    #[test]
    fn a_failed_mode_is_error() {
        let d = classify(
            "Op",
            PlanMode::Expansion,
            PlanMode::SourceAware,
            &planned("QueryPlan { Fetch(a) }", Ok(())),
            &ModeOutcome::Failed {
                reason: "source-aware planner not yet implemented".to_string(),
            },
        );
        assert_eq!(d.verdict, Verdict::Error);
        assert!(d.detail.unwrap().contains("not yet implemented"));
    }

    #[test]
    fn structural_diff_aligns_common_lines() {
        let left = "a\nb\nc\n";
        let right = "a\nx\nc\n";
        let diff = structural_diff(left, right);
        assert_eq!(diff, "  a\n- b\n+ x\n  c\n");
    }

    #[test]
    fn structural_diff_handles_insertions() {
        let diff = structural_diff("a\nc\n", "a\nb\nc\n");
        assert_eq!(diff, "  a\n+ b\n  c\n");
    }

    #[test]
    fn corpus_report_tallies_verdicts() {
        let diffs = vec![
            classify(
                "op1",
                PlanMode::Expansion,
                PlanMode::Expansion,
                &planned("P", Ok(())),
                &planned("P", Ok(())),
            ), // identical
            classify(
                "op2",
                PlanMode::Expansion,
                PlanMode::Expansion,
                &planned("P", Ok(())),
                &planned("Q", Ok(())),
            ), // equivalent
            classify(
                "op3",
                PlanMode::Expansion,
                PlanMode::Expansion,
                &planned("P", Ok(())),
                &planned("Q", Err("mismatch")),
            ), // different
            classify(
                "op4",
                PlanMode::Expansion,
                PlanMode::SourceAware,
                &planned("P", Ok(())),
                &ModeOutcome::Failed { reason: "x".into() },
            ), // error
        ];
        let report = CorpusReport::from_diffs(diffs);
        assert_eq!(report.total, 4);
        assert_eq!(report.identical, 1);
        assert_eq!(report.equivalent, 1);
        assert_eq!(report.different, 1);
        assert_eq!(report.error, 1);
        assert!(!report.all_ok());

        let clean = CorpusReport::from_diffs(vec![classify(
            "op",
            PlanMode::Expansion,
            PlanMode::Expansion,
            &planned("P", Ok(())),
            &planned("P", Ok(())),
        )]);
        assert!(clean.all_ok());
    }
}
