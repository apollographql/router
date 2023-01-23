//! Print the amount of physical and virtual memory used by a process
//! at various points of starting up (most of) a Router (through `TestHarness`)
//! and running a number of successive queries.
//!
//! The queries are pseudo-random and should all be different from each other,
//! so that the Router’s query cache grows.
//!
//! Example use:
//!
//! ```
//! cargo bench -p apollo-router-benchmarks --bench memory_use > /tmp/memory.tsv
//! ```
//!
//! Results from runs with different Router code (such as with and without
//! a PR applied) can be compared in a spreadsheet.
//! To make runs more comparable they use the same queries:
//! the PRNG seed is fixed.

use tower::Service;
use tower::ServiceExt;

// Generated in a build script so we don’t measure memory use of apollo-smith
include!(concat!(env!("OUT_DIR"), "/queries.rs"));

#[tokio::main]
async fn main() {
    println!("Physical (MiB)\tVirtual (MiB)\tWhen");
    print_stats();
    println!("tokio::main");
    let mut harness = apollo_router::TestHarness::builder()
        .schema(include_str!("fixtures/supergraph.graphql"))
        .build_supergraph()
        .await
        .unwrap();
    print_stats();
    println!("harness built");
    for (i, &query) in QUERIES.iter().enumerate() {
        let request = apollo_router::services::supergraph::Request::fake_builder()
            .query(query)
            .build()
            .unwrap();
        let _response = harness.ready().await.unwrap().call(request).await.unwrap();
        print_stats();
        println!("after {} requests", i + 1)
    }
}

fn print_stats() {
    let stats = memory_stats::memory_stats().unwrap();
    print_mebibyte(stats.physical_mem);
    print_mebibyte(stats.virtual_mem)
}

fn print_mebibyte(bytes: usize) {
    print!("{:0.2}\t", (bytes as f64) / 1024. / 1024.)
}
