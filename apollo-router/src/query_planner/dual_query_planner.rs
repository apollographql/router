//! Running two query planner implementations and comparing their results

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

use apollo_compiler::validation::Valid;
use apollo_compiler::ExecutableDocument;
use apollo_compiler::Name;
use apollo_federation::error::FederationError;
use apollo_federation::query_plan::query_planner::QueryPlanOptions;
use apollo_federation::query_plan::query_planner::QueryPlanner;

use crate::error::format_bridge_errors;
use crate::query_planner::bridge_query_planner::metric_query_planning_plan_duration;
use crate::query_planner::bridge_query_planner::JS_QP_MODE;
use crate::query_planner::bridge_query_planner::RUST_QP_MODE;
use crate::query_planner::convert::convert_root_query_plan_node;
use crate::query_planner::plan_compare::diff_plan;
use crate::query_planner::plan_compare::opt_plan_node_matches;
use crate::query_planner::QueryPlanResult;

/// Jobs are dropped if this many are already queued
const QUEUE_SIZE: usize = 10;
const WORKER_THREAD_COUNT: usize = 1;

pub(crate) struct BothModeComparisonJob {
    pub(crate) rust_planner: Arc<QueryPlanner>,
    pub(crate) js_duration: f64,
    pub(crate) document: Arc<Valid<ExecutableDocument>>,
    pub(crate) operation_name: Option<String>,
    pub(crate) js_result: Result<QueryPlanResult, Arc<Vec<router_bridge::planner::PlanError>>>,
    pub(crate) plan_options: QueryPlanOptions,
}

type Queue = crossbeam_channel::Sender<BothModeComparisonJob>;

static QUEUE: OnceLock<Queue> = OnceLock::new();

fn queue() -> &'static Queue {
    QUEUE.get_or_init(|| {
        let (sender, receiver) = crossbeam_channel::bounded::<BothModeComparisonJob>(QUEUE_SIZE);
        for _ in 0..WORKER_THREAD_COUNT {
            let job_receiver = receiver.clone();
            std::thread::spawn(move || {
                for job in job_receiver {
                    job.execute()
                }
            });
        }
        sender
    })
}

impl BothModeComparisonJob {
    pub(crate) fn schedule(self) {
        // We use a bounded queue: try_send returns an error when full. This is fine.
        // We prefer dropping some comparison jobs and only gathering some of the data
        // rather than consume too much resources.
        //
        // Either way we move on and let this thread continue proceed with the query plan from JS.
        let _ = queue().try_send(self).is_err();
    }

    fn execute(self) {
        let start = Instant::now();

        let rust_result = self
            .operation_name
            .as_deref()
            .map(|n| Name::new(n).map_err(FederationError::from))
            .transpose()
            .and_then(|operation| {
                self.rust_planner
                    .build_query_plan(&self.document, operation, self.plan_options)
            });

        let elapsed = start.elapsed().as_secs_f64();
        metric_query_planning_plan_duration(RUST_QP_MODE, elapsed);

        metric_query_planning_plan_both_comparison_duration(RUST_QP_MODE, elapsed);
        metric_query_planning_plan_both_comparison_duration(JS_QP_MODE, self.js_duration);

        let name = self.operation_name.as_deref();
        let operation_desc = if let Ok(operation) = self.document.operations.get(name) {
            if let Some(parsed_name) = &operation.name {
                format!(" in {} `{parsed_name}`", operation.operation_type)
            } else {
                format!(" in anonymous {}", operation.operation_type)
            }
        } else {
            String::new()
        };

        let is_matched;
        match (&self.js_result, &rust_result) {
            (Err(js_errors), Ok(_)) => {
                tracing::warn!(
                    "JS query planner error{operation_desc}: {}",
                    format_bridge_errors(js_errors)
                );
                is_matched = false;
            }
            (Ok(_), Err(rust_error)) => {
                tracing::warn!("Rust query planner error{operation_desc}: {}", rust_error);
                is_matched = false;
            }
            (Err(_), Err(_)) => {
                is_matched = true;
            }

            (Ok(js_plan), Ok(rust_plan)) => {
                let js_root_node = &js_plan.query_plan.node;
                let rust_root_node = convert_root_query_plan_node(rust_plan);
                let match_result = opt_plan_node_matches(js_root_node, &rust_root_node);
                is_matched = match_result.is_ok();
                match match_result {
                    Ok(_) => tracing::trace!("JS and Rust query plans match{operation_desc}! 🎉"),
                    Err(err) => {
                        tracing::debug!("JS v.s. Rust query plan mismatch{operation_desc}");
                        tracing::debug!("{}", err.full_description());
                        tracing::debug!(
                            "Diff of formatted plans:\n{}",
                            diff_plan(js_plan, rust_plan)
                        );
                        tracing::trace!("JS query plan Debug: {js_root_node:#?}");
                        tracing::trace!("Rust query plan Debug: {rust_root_node:#?}");
                    }
                }
            }
        }

        u64_counter!(
            "apollo.router.operations.query_planner.both",
            "Comparing JS v.s. Rust query plans",
            1,
            "generation.is_matched" = is_matched,
            "generation.js_error" = self.js_result.is_err(),
            "generation.rust_error" = rust_result.is_err()
        );
    }
}

pub(crate) fn metric_query_planning_plan_both_comparison_duration(
    planner: &'static str,
    elapsed: f64,
) {
    f64_histogram!(
        "apollo.router.operations.query_planner.both.duration",
        "Comparing JS v.s. Rust query plan duration.",
        elapsed,
        "planner" = planner
    );
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn test_metric_query_planning_plan_both_comparison_duration() {
        let start = Instant::now();
        let elapsed = start.elapsed().as_secs_f64();
        metric_query_planning_plan_both_comparison_duration(RUST_QP_MODE, elapsed);
        assert_histogram_exists!(
            "apollo.router.operations.query_planner.both.duration",
            f64,
            "planner" = "rust"
        );

        let start = Instant::now();
        let elapsed = start.elapsed().as_secs_f64();
        metric_query_planning_plan_both_comparison_duration(JS_QP_MODE, elapsed);
        assert_histogram_exists!(
            "apollo.router.operations.query_planner.both.duration",
            f64,
            "planner" = "js"
        );
    }
}
