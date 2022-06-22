//! Main entry point for CLI command to start spaceport.
// This entire file is license key functionality
use std::net::SocketAddr;

use apollo_spaceport::server::ReportSpaceport;
use clap::Parser;
use tracing_subscriber::filter::EnvFilter;

const DEFAULT_LISTEN: &str = "127.0.0.1:50051";

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Address to serve
    #[clap(short, long, default_value = DEFAULT_LISTEN)]
    address: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // By default, tracing will give us a filter which filters
    // at level ERROR. That's not what we want, so if we don't
    // have a filter specification, let's create one set at
    // level INFO.
    let filter = match EnvFilter::try_from_default_env() {
        Ok(f) => f,
        Err(_e) => EnvFilter::new("info"),
    };
    tracing_subscriber::fmt::fmt()
        .with_env_filter(filter)
        .json()
        .init();
    tracing::info!("spaceport starting");
    let spaceport = ReportSpaceport::new(args.address, None).await?;
    spaceport.serve().await?;

    Ok(())
}
