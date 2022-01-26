//! Main entry point for CLI command to start spaceport.
use apollo_spaceport::spaceport::ReportSpaceport;
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
    let spaceport = ReportSpaceport::new(args.address.parse()?);
    spaceport.serve().await?;

    Ok(())
}
