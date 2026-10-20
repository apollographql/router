//! Coverage-guided differential target.
//!
//! Requires a built Lean oracle named by `QUERY_INCLUSION_LEAN_ORACLE`. The oracle process is
//! started once and reused for the whole campaign, so per-case cost is one line of IPC rather
//! than a Lean startup.
//!
//! ```text
//! QUERY_INCLUSION_LEAN_ORACLE=target/query-inclusion-lean-oracle \
//!   cargo +nightly fuzz run differential corpus/differential
//! ```

#![no_main]

use std::cell::RefCell;

use libfuzzer_sys::fuzz_target;
use query_inclusion_fuzz::harness::Harness;
use query_inclusion_fuzz::lean_oracle::LeanOracle;
use query_inclusion_fuzz::model;

thread_local! {
    static STATE: RefCell<(Harness, LeanOracle)> = RefCell::new({
        let harness = Harness::new();
        let mut oracle = LeanOracle::from_env()
            .expect("QUERY_INCLUSION_LEAN_ORACLE must name a built Lean oracle");
        assert_eq!(
            oracle.schema_digest(),
            model::schema_digest(),
            "the Lean oracle and this harness hold different schemas"
        );
        (harness, oracle)
    });
}

fuzz_target!(|data: &[u8]| {
    // Long inputs only add unreachable trailing bytes: the grammar's nesting budget bounds how
    // much it can consume, and everything past that is decoded as zero on both sides.
    if data.len() > 64 {
        return;
    }
    STATE.with(|state| {
        let (harness, oracle) = &mut *state.borrow_mut();
        let Some(case) = harness.prepare(&model::decode(data)) else {
            return;
        };
        let (forward, backward) = oracle.verdicts(data);
        for (expected, reversed) in [(forward, false), (backward, true)] {
            let verdicts = harness.run(&case, reversed);
            if verdicts.out_of_scope {
                continue;
            }
            assert_eq!(
                expected.includes,
                verdicts.query_compare,
                "Lean and query_compare disagree (reversed={reversed})\nleft:\n{}\nright:\n{}\n{}",
                case.left_source,
                case.right_source,
                harness.explain(&case, reversed).unwrap_or_default(),
            );
        }
    });
});
