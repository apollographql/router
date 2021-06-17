#!/usr/bin/env bash
export RUSTFLAGS="-D warnings -D missing_docs -D future_incompatible -D missing_debug_implementations -D nonstandard_style -D future_incompatible -D unreachable_pub -D rust_2018_idioms"
cargo test --all
cargo fmt -- --check
cargo clippy --all --no-default-features -- -D warnings
cargo build --all-targets
cargo rustdoc
