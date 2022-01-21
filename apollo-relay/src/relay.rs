//! Main entry point for CLI command to start relay.
use apollo_relay::server::ReportServer;
use clap::Parser;

const DEFAULT_LISTEN: &str = "0.0.0.0:50051";

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Address to serve
    #[clap(short, long, default_value = DEFAULT_LISTEN)]
    address: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    tracing_subscriber::fmt::fmt().json().init();
    let report_server = ReportServer::new(args.address.parse()?);
    report_server.serve().await?;

    Ok(())
}
