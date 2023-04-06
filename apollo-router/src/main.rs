//! Main entry point for CLI command to start server.
// Note: We want to use jemalloc on unix, but we don't enable it if dhat-heap is in use because we
// can only have one global allocator
#[cfg(target = "unix")]
#[cfg(not(feature = "dhat-heap"))]
use tikv_jemallocator::Jemalloc;

#[cfg(target = "unix")]
#[cfg(not(feature = "dhat-heap"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

fn main() {
    match apollo_router::main() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
