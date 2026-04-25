//! Generative differential testing for the Apollo Federation query planner.
//!
//! Two versions of `apollo-federation` are linked side-by-side:
//! - `apollo_federation` from the local workspace (HEAD)
//! - `apollo_federation_base` from crates.io (BASELINE)
//!
//! Each is wrapped behind the [`PlannerHarness`] trait so the diff layer is
//! agnostic to API drift between versions.
//!
//! Layered modules:
//! 1. [`subgraph_gen`] — generates federated subgraph SDL sets
//! 2. [`compose`]      — runs composition and yields a supergraph + api schema
//! 3. [`op_gen`]       — generates valid operations against an api schema
//! 4. [`diff`]         — runs both planners and compares query plans
//! 5. binaries & tests in `src/bin/` and `tests/`
//!
//! Phase A scope: harness trait + both adapters + smoke test. Subgraph/op
//! generation and the proptest/libfuzzer drivers are introduced in later phases.

pub mod harness;
pub mod harness_base;
pub mod harness_head;

pub mod compose;
pub mod diff;
pub mod op_gen;
pub mod subgraph_gen;

pub use harness::{CommonConfig, CommonOptions, HarnessError, PlannerHarness};
pub use harness_base::BasePlanner;
pub use harness_head::HeadPlanner;

/// Default federation v2 `@link` preamble used when a generated subgraph SDL
/// snippet does not provide its own. Mirrors the constant used by the
/// in-tree query-plan tests at
/// `apollo-federation/tests/query_plan/build_query_plan_support.rs`.
///
/// The Phase-A scope only imports `@key` and `@shareable`; expand here as
/// the generator grows.
pub const DEFAULT_LINK_DIRECTIVE: &str = r#"@link(url: "https://specs.apollo.dev/federation/v2.9", import: ["@key", "@shareable"])"#;
