//! `connect-migrate` — CLI for moving Apollo Connectors customers
//! between `connect/v0.X` specs.
//!
//! Built behind the `connect-migrate` feature of the `apollo-federation`
//! crate so the dependency on `clap` doesn't enter the default build
//! graph for library consumers:
//!
//!     cargo build --release --bin connect-migrate --features connect-migrate
//!
//! The library helpers the binary calls into live in the sibling
//! `mod.rs` and are part of the `apollo_federation::connectors::migration`
//! public surface.

use apollo_federation::connectors::migration::AGENT_GUIDE;
use clap::Parser;
use clap::Subcommand;

/// Binary version reported by `connect-migrate --version`.
///
/// Decoupled from `apollo-federation`'s package version because
/// `connect-migrate` ships from a separate release repo with its own
/// cadence. The release CI overrides this via the
/// `CONNECT_MIGRATE_VERSION` env var at compile time (set from the
/// pushed git tag); local dev builds get the `-dev` suffix.
const VERSION: &str = match option_env!("CONNECT_MIGRATE_VERSION") {
    Some(v) => v,
    None => "0.0.0-dev",
};

#[derive(Parser, Debug)]
#[command(
    name = "connect-migrate",
    about = "Help upgrade Apollo Connectors schemas across connect/v0.X spec versions",
    long_about = None,
    version = VERSION,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the developer-facing migration guide that an agent can
    /// follow when walking a customer through a v0.3 → v0.4 upgrade.
    ///
    /// The guide is embedded in the binary at compile time. Print it
    /// and pipe to your agent of choice, or read it manually.
    AgentGuide,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::AgentGuide => {
            print!("{}", AGENT_GUIDE);
        }
    }
}
