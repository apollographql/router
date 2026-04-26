//! Phase-D: proptest-driven differential testing.
//!
//! Strategy: drive the existing constructive subgraph generator with
//! proptest-supplied byte vectors. Shrinking happens in input space —
//! shorter byte vectors and smaller byte values produce simpler schemas
//! through the generator's `int_in_range`/`choose_index` clamping. This
//! reuses 100% of the Phase-C generator and gets us automatic minimal-repro
//! discovery without re-implementing the generator as a `Strategy`.
//!
//! Failing cases are auto-persisted by proptest under
//! `apollo-federation-fuzz/proptest-regressions/` and replay deterministically
//! on subsequent runs. Override case count with `PROPTEST_CASES=N`.

use arbitrary::Unstructured;
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence};

use apollo_federation_fuzz::compose::{ComposeOutcome, try_compose};
use apollo_federation_fuzz::diff::{DiffOutcome, run_diff};
use apollo_federation_fuzz::harness::{CommonConfig, CommonOptions};
use apollo_federation_fuzz::op_gen::generate_operation;
use apollo_federation_fuzz::subgraph_gen::{GenConfig, generate_federated_subgraphs};
use apollo_federation_fuzz::{BasePlanner, HeadPlanner};

proptest! {
    #![proptest_config(Config {
        cases: 64,
        max_shrink_iters: 2048,
        // Save failing (shrunken) seeds to a known path inside this crate.
        // The default `SourceParallel` policy can't locate lib.rs from an
        // integration-test binary and silently disables persistence.
        failure_persistence: Some(Box::new(
            FileFailurePersistence::Direct("tests/regressions/proptest_diff.txt"),
        )),
        ..Config::default()
    })]

    /// Property: for any schema-bytes/op-bytes pair that successfully passes
    /// through generator → composition → op_gen, both planner versions
    /// produce identical (normalized) query plans.
    #[test]
    fn planners_agree_under_shrink(
        schema_bytes in prop::collection::vec(any::<u8>(), 256..1024),
        op_bytes in prop::collection::vec(any::<u8>(), 128..512),
    ) {
        let mut u = Unstructured::new(&schema_bytes);
        let subgraphs = match generate_federated_subgraphs(&mut u, &GenConfig::default()) {
            Ok(s) => s,
            Err(_) => return Err(TestCaseError::reject("subgraph_gen ran out of bytes")),
        };
        let supergraph_sdl = match try_compose(&subgraphs) {
            ComposeOutcome::Composed { supergraph_sdl } => supergraph_sdl,
            _ => return Err(TestCaseError::reject("composition rejected")),
        };
        let op = match generate_operation(&supergraph_sdl, &op_bytes) {
            Ok(op) => op,
            Err(_) => return Err(TestCaseError::reject("op_gen rejected")),
        };

        let outcome = run_diff::<HeadPlanner, BasePlanner>(
            &supergraph_sdl,
            &op,
            None,
            &CommonConfig::default(),
            &CommonOptions::default(),
        );
        match outcome {
            DiffOutcome::Identical { .. } => {}
            DiffOutcome::Divergent { unified_diff, .. } => {
                let subgraphs_dump = subgraphs
                    .iter()
                    .map(|s| format!("# subgraph {}\n{}", s.name, s.sdl))
                    .collect::<Vec<_>>()
                    .join("\n");
                prop_assert!(
                    false,
                    "planners diverged\n=== SUBGRAPHS ===\n{}\n=== SUPERGRAPH ===\n{}\n=== OP ===\n{}\n=== DIFF ===\n{}",
                    subgraphs_dump, supergraph_sdl, op, unified_diff,
                );
            }
            DiffOutcome::EitherFailed { head, base } => {
                // Different errors on each side is itself a finding (worth
                // reporting), but identical errors on both sides are noise
                // (e.g. both planners reject the same operation).
                if head.is_ok() != base.is_ok() {
                    prop_assert!(false,
                        "asymmetric planner failure: head_ok={} base_ok={} head={:?} base={:?}",
                        head.is_ok(), base.is_ok(), head.err(), base.err());
                }
                return Err(TestCaseError::reject("symmetric plan err"));
            }
            DiffOutcome::PanickedSide { head_panic, base_panic, .. } => {
                // A panic on either side is a planner bug. proptest
                // surfaces it as a test failure; the harness sweep
                // (`bin/fuzz`) saves the inputs as a reproducer.
                prop_assert!(false,
                    "planner panic: head={head_panic:?} base={base_panic:?}");
            }
        }
    }
}

/// Self-test of the shrinking infrastructure. The "property" intentionally
/// fails on any schema with two or more entities, so proptest must shrink
/// to a minimal subgraph set with exactly two entities. Run with
/// `APF_PROVE_SHRINK=1 cargo test -p apollo-federation-fuzz --test proptest_diff -- shrink_proves_itself`.
#[test]
#[ignore]
fn shrink_proves_itself() {
    if std::env::var("APF_PROVE_SHRINK").ok().as_deref() != Some("1") {
        return;
    }

    let cfg = Config {
        cases: 32,
        max_shrink_iters: 1024,
        failure_persistence: Some(Box::new(
            FileFailurePersistence::Direct("tests/regressions/shrink_self_test.txt"),
        )),
        ..Config::default()
    };

    let strat = prop::collection::vec(any::<u8>(), 256..1024);
    let mut runner = proptest::test_runner::TestRunner::new(cfg);
    let result = runner.run(&strat, |bytes| {
        let mut u = Unstructured::new(&bytes);
        let subgraphs = generate_federated_subgraphs(&mut u, &GenConfig::default())
            .map_err(|_| TestCaseError::reject("gen"))?;
        // Synthetic "bug": fail on schemas with >= 2 entities to force the
        // shrinker to exhibit work.
        let entity_count = subgraphs
            .iter()
            .map(|s| s.sdl.matches("@key(fields:").count())
            .sum::<usize>();
        prop_assert!(entity_count < 2, "synthetic bug: {entity_count} entities");
        Ok(())
    });
    match result {
        Err(proptest::test_runner::TestError::Fail(reason, _)) => {
            eprintln!("synthetic shrink finished, reason: {reason}");
        }
        other => panic!("expected synthetic test to fail, got: {other:?}"),
    }
}
