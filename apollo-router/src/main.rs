//! Main entry point for CLI command to start server.

use anyhow::Result;
use apollo_router;

fn main() -> Result<()> {
    apollo_router::main()
}
