//! Tooling for moving Apollo Connectors customers between `connect/v0.X`
//! specs.
//!
//! The user-facing surface is the `connect-migrate` CLI defined in
//! [`main.rs`](./main.rs) — built via `cargo build --bin connect-migrate
//! --features connect-migrate` and packaged for cross-platform release
//! out of a separate distribution repository.
//!
//! The library surface in this module is intentionally narrow: it
//! exists so the binary can use private parser helpers from
//! `connectors::json_selection` that aren't part of the public Rust
//! API of `apollo_federation`. Anything in here that needs to be
//! reachable from `main.rs` must be `pub` (the binary is a separate
//! compilation unit and only sees the crate's public API).
//!
//! See `agent_guide.md` for the developer-facing migration prose that
//! the binary embeds via `include_str!` and prints under the
//! `agent-guide` subcommand.

/// The embedded developer-facing migration guide. Surfaced to agents
/// via `connect-migrate agent-guide`.
pub const AGENT_GUIDE: &str = include_str!("agent_guide.md");
