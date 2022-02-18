//! Main entry point for CLI command to start spaceport.
use std::net::SocketAddr;

use apollo_spaceport::server::ReportSpaceport;
use clap::Parser;

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

    tracing_subscriber::fmt::fmt().json().init();
    let spaceport = ReportSpaceport::new(args.address);
    spaceport.serve().await?;

    Ok(())
}
