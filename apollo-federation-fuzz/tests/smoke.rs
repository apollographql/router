//! Phase-A smoke test: compose the canned fixture, run a hand-written
//! operation through both planners, assert their plans agree.

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::diff::{DiffOutcome, run_diff};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions};
use apollo_federation_fuzz::subgraph_gen::smoke_test_fixture;
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

#[test]
fn smoke_two_subgraphs_one_query_planners_agree() {
    let subgraphs = smoke_test_fixture();
    let supergraph_sdl = match try_compose(&subgraphs) {
        ComposeOutcome::Composed { supergraph_sdl } => supergraph_sdl,
        other => panic!("compose failed: {other:?}"),
    };

    let op = r#"
        query Smoke {
          me {
            id
            name
            reviews { id body }
          }
          latestReview { id body author { id name } }
        }
    "#;

    let outcome = run_diff::<HeadPlanner, BasePlanner>(
        &supergraph_sdl,
        op,
        None,
        &CommonConfig::default(),
        &CommonOptions::default(),
    );

    match outcome {
        DiffOutcome::Identical { .. } => {}
        DiffOutcome::Divergent { unified_diff, .. } => {
            panic!("planners diverged on smoke test:\n{unified_diff}");
        }
        DiffOutcome::EitherFailed { head, base } => {
            panic!("planner errored:\nhead={head:?}\nbase={base:?}");
        }
    }
}
