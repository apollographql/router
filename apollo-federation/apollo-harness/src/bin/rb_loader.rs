use apollo_harness::common::Cli;

use anyhow::{Error, Result};
use clap::Parser;
use router_bridge::planner::Planner;
use router_bridge::planner::QueryPlannerConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let schema = tokio::fs::read_to_string(cli.schema).await?;
    let planner = Planner::<serde_json::Value>::new(schema, QueryPlannerConfig::default())
        .await
        .map_err(|errs| {
            for err in errs {
                eprintln!("error: {err}");
            }
            Error::msg("schema loading failed")
        })?;

    if let Some(query_file) = cli.query {
        let query = tokio::fs::read_to_string(query_file).await?;
        let _payload = planner
            .plan(query, None)
            .await?
            .into_result()
            .map_err(|err| {
                eprintln!("errors: {err}");
                Error::msg("query planning failed")
            })?;
    }
    Ok(())
}
