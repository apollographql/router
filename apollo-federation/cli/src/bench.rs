use std::fmt::Display;
use std::path::PathBuf;
use std::time::Instant;

use apollo_compiler::ExecutableDocument;
use apollo_federation::error::FederationError;
use apollo_federation::query_plan::query_planner::QueryPlanner;
use apollo_federation::query_plan::query_planner::QueryPlannerConfig;
use apollo_federation::Supergraph;

pub(crate) fn run_bench(
    supergraph: Supergraph,
    queries_dir: &PathBuf,
    config: QueryPlannerConfig,
) -> Result<Vec<BenchOutput>, FederationError> {
    let planner = QueryPlanner::new(&supergraph, config.clone()).expect("Invalid planner");

    let mut entries = std::fs::read_dir(queries_dir)
        .unwrap()
        .map(|res| res.map(|e| e.path()))
        .collect::<Result<Vec<_>, std::io::Error>>()
        .unwrap();

    entries.sort();

    let mut results = Vec::with_capacity(entries.len());

    for query_path in entries.into_iter() {
        let query_string = std::fs::read_to_string(query_path.clone()).unwrap();

        let file_name = query_path
            .file_name()
            .to_owned()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let document = match ExecutableDocument::parse_and_validate(
            supergraph.schema.schema(),
            query_string,
            "query",
        ) {
            Ok(document) => document,
            Err(_) => {
                results.push(BenchOutput {
                    query_name: file_name.split('-').next().unwrap().to_string(),
                    file_name,
                    timing: 0.0,
                    eval_plans: None,
                    error: Some("error".to_string()),
                });

                continue;
            }
        };
        let now = Instant::now();
        let plan = planner.build_query_plan(&document, None, None);
        let elapsed = now.elapsed().as_secs_f64() * 1000.0;
        let mut eval_plans = None;
        let mut error = None;
        if let Ok(p) = plan {
            eval_plans = Some(p.statistics.evaluated_plan_count.into_inner().to_string());
        } else {
            error = Some("error".to_string());
        };

        results.push(BenchOutput {
            query_name: file_name.split('-').next().unwrap().to_string(),
            file_name,
            timing: elapsed,
            eval_plans,
            error,
        });
    }

    // totally arbitrary
    results.sort_by(|a, b| a.partial_cmp(b).unwrap_or(a.query_name.cmp(&b.query_name)));
    Ok(results)
}

#[derive(Debug)]
#[cfg_attr(test, derive(serde::Serialize))]
pub(crate) struct BenchOutput {
    file_name: String,
    query_name: String,
    timing: f64,
    eval_plans: Option<String>,
    error: Option<String>,
}

impl PartialEq for BenchOutput {
    fn eq(&self, other: &Self) -> bool {
        self.timing == other.timing
    }
}

impl PartialOrd for BenchOutput {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match other.timing.partial_cmp(&self.timing) {
            Some(core::cmp::Ordering::Equal) => Some(core::cmp::Ordering::Equal),
            ord => ord,
        }
    }
}

impl Display for BenchOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "| [{}](queries/{}) | {} | {} | {} |",
            self.query_name,
            self.file_name,
            self.timing,
            self.eval_plans.clone().unwrap_or(" ".to_string()),
            self.error.clone().unwrap_or(" ".to_string())
        )
    }
}
