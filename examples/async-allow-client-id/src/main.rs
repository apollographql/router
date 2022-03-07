//! Main entry point for CLI command to start server.

use anyhow::{anyhow, Result};
use apollo_router::ApolloRouterBuilder;
use apollo_router::{ConfigurationKind, SchemaKind, ShutdownKind, State};
use futures::prelude::*;
use tracing_subscriber::EnvFilter;

mod allow_client_id_from_file;

// curl -v \
//     --header 'content-type: application/json' \
//     --header 'x-client-id: unknown' \
//     --url 'http://127.0.0.1:4000' \
//     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
// [...]
// < HTTP/1.1 403 Forbidden
// < content-length: 78
// < date: Mon, 07 Mar 2022 12:08:21 GMT
// <
// * Connection #0 to host 127.0.0.1 left intact
// {"errors":[{"message":"client-id is not allowed","locations":[],"path":null}]}

// curl -v \
//     --header 'content-type: application/json' \
//     --header 'x-client-id: jeremy' \
//     --url 'http://127.0.0.1:4000' \
//     --data '{"query":"query Query {\n  me {\n    name\n  }\n}"}'
// < HTTP/1.1 200 OK
// < content-length: 39
// < date: Mon, 07 Mar 2022 12:09:08 GMT
// <
// * Connection #0 to host 127.0.0.1 left intact
// {"data":{"me":{"name":"Ada Lovelace"}}}
#[tokio::main]
async fn main() -> Result<()> {
    let _ = tracing_subscriber::fmt::fmt()
        .with_env_filter(EnvFilter::try_new("info").expect("could not parse log"))
        .init();

    let current_directory = std::env::current_dir()?;

    let schema = SchemaKind::File {
        path: current_directory
            .parent()
            .ok_or(anyhow!("no parent"))?
            .parent()
            .ok_or(anyhow!("no parent"))?
            .join("examples/supergraph.graphql"),
        watch: false,
        delay: None,
    };

    let configuration = ConfigurationKind::File {
        path: current_directory.join("config.yml"),
        watch: false,
        delay: None,
    };

    let server = ApolloRouterBuilder::default()
        .configuration(configuration)
        .schema(schema)
        .shutdown(ShutdownKind::CtrlC)
        .build();

    let mut server_handle = server.serve();
    server_handle
        .state_receiver()
        .for_each(|state| {
            match state {
                State::Startup => {
                    tracing::info!(r#"Starting Apollo Router"#)
                }
                State::Running { address, .. } => {
                    tracing::info!("Listening on {} 🚀", address)
                }
                State::Stopped => {
                    tracing::info!("Stopped")
                }
                State::Errored => {
                    tracing::info!("Stopped with error")
                }
            }
            future::ready(())
        })
        .await;

    if let Err(err) = server_handle.await {
        tracing::error!("{}", err);
        return Err(err.into());
    }

    Ok(())
}
