//! Main entry point for CLI command to start server.

use anyhow::Result;
use apollo_router::configuration::Configuration;
use apollo_router::ApolloRouterBuilder;
use apollo_router::{ConfigurationKind, SchemaKind, ShutdownKind};
use apollo_router_core::Schema;
use tracing_subscriber::EnvFilter;

mod context_data;

// curl -v \
//     --header 'content-type: application/json' \
//     --url 'http://127.0.0.1:4000' \
//     --data '{"query":"query { topProducts { reviews { author { name } } name } }"}'
// [...]
// {"data":{"topProducts":[{"reviews":[{"author":{"name":"Ada Lovelace"}},{"author":{"name":"Alan Turing"}}],"name":"Table"},{"reviews":[{"author":{"name":"Ada Lovelace"}}],"name":"Couch"},{"reviews":[{"author":{"name":"Alan Turing"}}],"name":"Chair"}]}}
#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::try_new("info").expect("could not parse log"))
        .init();

    let schema = SchemaKind::Instance(
        include_str!("../../supergraph.graphql")
            .parse::<Schema>()?
            .boxed(),
    );

    let configuration = ConfigurationKind::Instance(
        include_str!("../config.yaml")
            .parse::<Configuration>()?
            .boxed(),
    );

    let server = ApolloRouterBuilder::default()
        .configuration(configuration)
        .schema(schema)
        .shutdown(ShutdownKind::CtrlC)
        .build();

    let mut server_handle = server.serve();
    server_handle.with_defualt_state_receiver().await;

    if let Err(err) = server_handle.await {
        tracing::error!("{}", err);
        return Err(err.into());
    }

    Ok(())
}
