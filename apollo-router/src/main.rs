//! Main entry point for CLI command to start server.

fn main() {
    match apollo_router::main() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
