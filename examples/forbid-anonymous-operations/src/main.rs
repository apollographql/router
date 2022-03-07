//! Main entry point for CLI command to start server.

use anyhow::Result;
use apollo_router::configuration::Configuration;
use apollo_router::ApolloRouterBuilder;
use apollo_router::{ConfigurationKind, SchemaKind, ShutdownKind};
use apollo_router_core::Schema;
use tracing_subscriber::EnvFilter;

mod forbid_anonymous_operations;

// curl -v \
//     --header 'content-type: application/json' \
//     --url 'http://127.0.0.1:4000' \
//     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
// [...]
// < HTTP/1.1 400 Bad Request
// < content-length: 90
// < date: Thu, 03 Mar 2022 14:31:50 GMT
// <
// * Connection #0 to host 127.0.0.1 left intact
// {"errors":[{"message":"Anonymous operations are not allowed","locations":[],"path":null}]}
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
    server_handle.with_default_state_receiver().await;

    if let Err(err) = server_handle.await {
        tracing::error!("{}", err);
        return Err(err.into());
    }

    Ok(())
}
