//! Main entry point for CLI command to start server.

use log::LevelFilter;

use configuration::Configuration;
use server::ShutdownType;
use server::{FederatedServer, FederatedServerError};

#[tokio::main]
async fn main() -> Result<(), FederatedServerError> {
    // TODO Actually implement this properly. Command line should allow setting of log level, config location and schema location. Make sure to have sensible defaults.
    let _ = env_logger::builder()
        .filter_level(LevelFilter::Debug)
        .init();

    let configuration =
        serde_yaml::from_str::<Configuration>(include_str!("testdata/supergraph_config.yaml"))
            .unwrap();
    let schema = include_str!("testdata/supergraph.graphql").to_string();
    let server = FederatedServer::builder()
        .configuration(configuration)
        .schema(schema)
        .shutdown(ShutdownType::CtrlC)
        .build();
    server.serve().await
}
