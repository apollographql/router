//! Minimized reproducer for the in-tree planner panic
//!
//!     thread 'main' panicked at
//!     apollo-federation/src/query_plan/fetch_dependency_graph.rs:1899:9:
//!     Root nodes should have no remaining nodes unhandled,
//!     but got: [5 (missing: [2])]
//!
//! Originally captured by a 10k op fuzz sweep at iter=2028 seed=17 in
//! `tests/regressions/cross_version_2.5.0_panic_10k/process_root_nodes_panic.txt`.
//! That trigger was 4 subgraphs / ~12 entity declarations + a ~70-line
//! op with two named fragments and 6 nested @defer levels. Hand-reduced
//! down to the form below by repeated bisection: roughly 30 lines of
//! SDL across 4 subgraphs + a 10-line op.
//!
//! Almost certainly the same bug class addressed by PR #9123
//! ("restore missing defer dependencies after transitive reduction"),
//! which was reverted in PR #9250 — the current `dev`/HEAD planner is
//! the post-revert state, hence the panic still reproduces. The
//! fetch-dependency-graph assertion message is a precise match for
//! #9123's stated symptom.
//!
//! Trigger ingredients (each is necessary; removing any one makes it
//! plan cleanly):
//!
//!   1. The entity-traversal field `T2.r2_0: T0` must be `@shareable`
//!      across **three** subgraphs (s0, s2, s3). Reducing to two makes
//!      the planner pick a single source and the panic goes away.
//!   2. A field `T0.f0_0` `@shareable` across two subgraphs (s1, s3).
//!   3. A `@requires` site: `T0.f0_2 @requires(fields: "f0_3")` in s1
//!      with `T0.f0_3 @external` in s1 and owned by s3.
//!   4. The operation must select **both** `f0_0` and an aliased
//!      duplicate `aS3: f0_0` inside an `@defer` block. The aliased
//!      duplicate alone is not enough; the bare `f0_0` alone is not
//!      enough.
//!   5. `f0_1` and `f0_2` selected alongside the duplicates, also
//!      inside the `@defer`.
//!
//! Composition succeeds; the panic is in plan-cost evaluation deep
//! inside `process_root_nodes`. No `@skip`/`@include`, no operation
//! variables, no named fragments, no nested `@defer` — those were all
//! ornamentation in the original.
//!
//! Run:
//!   cargo run -p apollo-federation-fuzz --example minimize_panic --release

use std::panic::{AssertUnwindSafe, catch_unwind};

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions, PlannerHarness};
use apollo_federation_fuzz::subgraph_gen::SubgraphSdl;
use apollo_federation_fuzz::HeadPlanner;

fn subgraphs() -> Vec<SubgraphSdl> {
    vec![
        SubgraphSdl::new(
            "s0",
            r#"
type T2 @key(fields: "id") {
  id: ID!
  r2_0: T0 @shareable
}

type T0 @key(fields: "id") {
  id: ID!
}
"#,
        ),
        SubgraphSdl::new(
            "s1",
            r#"
type Query {
  qT2: T2
}

type T0 @key(fields: "id") {
  id: ID!
  f0_0: ID @shareable
  f0_2: Float! @requires(fields: "f0_3")
  f0_3: Boolean @external
}

type T2 @key(fields: "id") {
  id: ID!
}
"#,
        ),
        SubgraphSdl::new(
            "s2",
            r#"
type T2 @key(fields: "id") {
  id: ID!
  r2_0: T0 @shareable
}

type T0 @key(fields: "id") {
  id: ID!
}
"#,
        ),
        SubgraphSdl::new(
            "s3",
            r#"
type T0 @key(fields: "id") {
  id: ID!
  f0_0: ID @shareable
  f0_1: Boolean! @shareable
  f0_3: Boolean
}

type T2 @key(fields: "id") {
  id: ID!
  r2_0: T0 @shareable
}
"#,
        ),
    ]
}

const OPERATION: &str = r#"
query {
  qT2 {
    r2_0 {
      ... @defer {
        f0_1
        f0_0
        aS3: f0_0
        f0_2
      }
    }
  }
}

"#;

fn main() {
    let subs = subgraphs();
    let supergraph_sdl = match try_compose(&subs) {
        ComposeOutcome::Composed { supergraph_sdl } => supergraph_sdl,
        ComposeOutcome::CompositionFailed { errors } => {
            println!("ERROR: composition failed: {errors:?}");
            std::process::exit(2);
        }
        ComposeOutcome::ParseFailed { errors } => {
            println!("ERROR: parse failed: {errors:?}");
            std::process::exit(2);
        }
    };

    let cfg = CommonConfig {
        incremental_delivery: true,
        ..CommonConfig::default()
    };
    let planner = match HeadPlanner::build(&supergraph_sdl, &cfg) {
        Ok(p) => p,
        Err(e) => {
            println!("ERROR: planner build: {e:?}");
            std::process::exit(2);
        }
    };

    let plan_attempt = catch_unwind(AssertUnwindSafe(|| {
        planner.plan(OPERATION, None, &CommonOptions::default())
    }));

    match plan_attempt {
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic>".to_string()
            };
            println!("STILL PANICS: {msg}");
        }
        Ok(Ok(_plan)) => {
            println!("PLANS CLEANLY");
        }
        Ok(Err(e)) => {
            println!("ERROR: planner returned: {e:?}");
        }
    }
}
