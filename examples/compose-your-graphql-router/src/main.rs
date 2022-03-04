use anyhow::{anyhow, Result};
use apollo_router_core::{plugin_utils, PluggableRouterServiceBuilder};
use std::sync::Arc;
use tower::{util::BoxService, ServiceExt};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::try_new("info").expect("could not parse log"))
        .init();

    let current_directory = std::env::current_dir()?;

    let schema = Arc::new(
        std::fs::read_to_string(
            current_directory
                .parent()
                .ok_or(anyhow!("no parent"))?
                .parent()
                .ok_or(anyhow!("no parent"))?
                .join("examples/supergraph.graphql"),
        )?
        .parse()?,
    );

    let buffer = 20_000;

    let mut router_builder = PluggableRouterServiceBuilder::new(schema, buffer);

    let subgraph_service = BoxService::new(
        apollo_router::reqwest_subgraph_service::ReqwestSubgraphService::new(
            "accounts".to_string(),
            "https://accounts.demo.starstuff.dev".parse()?,
        ),
    );

    router_builder = router_builder.with_subgraph_service("accounts", subgraph_service);
    let (router_service, _) = router_builder.build().await;

    let request = plugin_utils::RouterRequest::builder()
        .query(r#"query Query { me { name } }"#.to_string())
        .build()
        .into();

    let res = router_service
        .oneshot(request)
        .await
        .map_err(|e| anyhow!("router_service call failed: {}", e))?;

    // {
    //   "data": {
    //     "me": {
    //       "name": "Ada Lovelace"
    //     }
    //   }
    // }
    println!("{}", serde_json::to_string_pretty(res.response.body())?);
    Ok(())
}
