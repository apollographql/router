//! Metamorphic lane: semantics-preserving rewrites must not change the verdict.
//!
//! The byte grammar is closed — it produces the shapes someone thought to encode. This lane is
//! open: it takes a case and applies random sequences of edits that provably leave both response
//! shapes unchanged, so `includes` must answer exactly as it did before. Composing those edits
//! reaches shapes no enumerated family covers, and two of them reproduce known bug shapes by
//! construction (complementary `@skip`/`@include` branches, and partition by runtime type).
//!
//! No oracle is involved, so this runs at full Rust speed and needs no Lean checkout.
//!
//! ```text
//! cargo run --release --example metamorphic -- --cases 200000
//! cargo run --release --example metamorphic -- --input-hex 0a0b0c --rewrite-seed 7
//! ```

use std::collections::BTreeMap;

use apollo_federation::correctness::query_compare;
use query_inclusion_fuzz::harness::Harness;
use query_inclusion_fuzz::model;

/// Below this, a ratio says more about the rewrite than about the checker.
const COST_FLOOR: u64 = 1;
/// Where a work change stops being what an edit accounts for.
///
/// Calibrated, not guessed: over 40,000 comparisons on this fixture the largest legitimate change
/// is 8x, from composing `split-complementary`, `duplicate` and `partition-by-runtime-type`, each
/// of which really does add groups. Note what the same 40,000 comparisons say about the ceiling:
/// no check here decides more than 6 groups, so this fixture cannot produce the two-orders-of-
/// magnitude change that a real schema did. The guard is cheap and it is exact, but catching that
/// class needs the invariant run against a corpus-scale schema -- see `check_bench --compare-work`.
const COST_RATIO_REPORTED: f64 = 12.0;

struct Options {
    seed: u64,
    cases: usize,
    steps: usize,
    input_hex: Option<String>,
    rewrite_seed: u64,
}

fn parse_options() -> Options {
    let mut options = Options {
        seed: 1,
        cases: 20000,
        steps: 3,
        input_hex: None,
        rewrite_seed: 1,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--seed" => options.seed = args.next().unwrap().parse().unwrap(),
            "--cases" => options.cases = args.next().unwrap().parse().unwrap(),
            "--steps" => options.steps = args.next().unwrap().parse().unwrap(),
            "--rewrite-seed" => options.rewrite_seed = args.next().unwrap().parse().unwrap(),
            "--input-hex" => options.input_hex = Some(args.next().unwrap()),
            other => panic!("unknown argument: {other}"),
        }
    }
    options
}

struct Rng(u64);

impl Rng {
    fn next_byte(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 33) as u8
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() {
    let options = parse_options();
    let harness = Harness::new();

    let inputs: Vec<(Vec<u8>, u64)> = match &options.input_hex {
        Some(text) => {
            let bytes = (0..text.len() / 2)
                .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).expect("hex"))
                .collect();
            vec![(bytes, options.rewrite_seed)]
        }
        None => {
            // Mostly *related* pairs, built by repeating the bytes the left operation consumed.
            // A semantics-preserving rewrite applied to a pair that already fails for an unrelated
            // reason tests nothing: the verdict was false before and stays false. The rewrites
            // only bite where the two operations nearly agree.
            let mut rng = Rng(options.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            (0..options.cases)
                .map(|index| {
                    let length = 8 + (rng.next_byte() as usize % 24);
                    let base: Vec<u8> = (0..length).map(|_| rng.next_byte()).collect();
                    let bytes = if rng.next_byte().is_multiple_of(4) {
                        base
                    } else {
                        let split = model::left_consumed(&base);
                        let prefix = &base[..split];
                        let mut related: Vec<u8> =
                            prefix.iter().chain(prefix.iter()).copied().collect();
                        if !rng.next_byte().is_multiple_of(3) && !prefix.is_empty() {
                            let at = split + (rng.next_byte() as usize % prefix.len());
                            related[at] = related[at].wrapping_add(1 + rng.next_byte() % 7);
                        }
                        related
                    };
                    (bytes, index as u64 + 1)
                })
                .collect()
        }
    };

    let mut checked = 0usize;
    let mut base_invalid = 0usize;
    let mut rewritten_invalid = 0usize;
    let mut violations = 0usize;
    let mut reflexivity_failures = 0usize;
    let mut monotonicity_failures = 0usize;
    let mut transitivity_failures = 0usize;
    let mut monotonicity_checked = 0usize;
    let mut transitivity_checked = 0usize;
    let mut by_rewrite: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut cost_compared = 0usize;
    let mut cost_outliers = 0usize;
    let mut worst_cost_ratio = 1.0f64;
    let mut worst_cost_case: Option<(String, u64, String)> = None;

    for (bytes, rewrite_seed) in &inputs {
        let case = model::rewritten_case(bytes, *rewrite_seed, options.steps);
        let Some(base) = harness.prepare(&case.base) else {
            base_invalid += 1;
            continue;
        };
        // A rewrite that produces an invalid document is a defect in the rewrite, not a finding
        // about the checker: every edit here is supposed to preserve validity as well as meaning.
        let Some(rewritten) = harness.prepare(&case.rewritten) else {
            rewritten_invalid += 1;
            if rewritten_invalid <= 3 {
                println!("---- rewrite produced an invalid document");
                println!("--input-hex {} --rewrite-seed {rewrite_seed}", hex(bytes));
                println!("applied: {}", case.applied.join(", "));
                println!(
                    "left:\n{}right:\n{}",
                    case.rewritten.left, case.rewritten.right
                );
                if let Err(error) = harness.prepare_explained(&case.rewritten) {
                    println!("{error}");
                }
            }
            continue;
        };
        for name in &case.applied {
            *by_rewrite.entry(name).or_default() += 1;
        }

        // Reflexivity: every operation includes itself. Independent of any rewrite, and it holds
        // for the rewritten documents too, which is where the composed shapes live.
        for (label, document) in [
            ("base left", &base.left),
            ("rewritten left", &rewritten.left),
            ("rewritten right", &rewritten.right),
        ] {
            if query_compare::includes(harness.schema(), document, document).is_err() {
                reflexivity_failures += 1;
                println!("--------------------------------------------------------------------");
                println!("REFLEXIVITY FAILURE on {label}");
                println!("--input-hex {} --rewrite-seed {rewrite_seed}", hex(bytes));
                println!("applied: {}", case.applied.join(", "));
                if let Err(error) = query_compare::includes(harness.schema(), document, document) {
                    println!("{error}");
                }
            }
        }

        // Monotonicity: a left that produces more and a right that demands less cannot turn a
        // holding inclusion into a failing one. Independent of verdict-invariance — a checker can
        // be stable under meaning-preserving edits and still get this wrong.
        let weakened = model::weakened_case(bytes, *rewrite_seed, options.steps);
        if let (Some(before), Some(after)) = (
            harness.prepare(&weakened.base),
            harness.prepare(&weakened.weakened),
        ) {
            monotonicity_checked += 1;
            if harness.run(&before, false).query_compare
                && !harness.run(&after, false).query_compare
            {
                monotonicity_failures += 1;
                println!("--------------------------------------------------------------------");
                println!("MONOTONICITY FAILURE on --input-hex {}", hex(bytes));
                println!("base left:\n{}", weakened.base.left);
                println!("base right:\n{}", weakened.base.right);
                println!("weakened left:\n{}", weakened.weakened.left);
                println!("weakened right:\n{}", weakened.weakened.right);
                if let Some(explanation) = harness.explain(&after, false) {
                    println!("query_compare says:\n{explanation}");
                }
            }
        }

        // Transitivity: if A includes B and B includes C then A includes C.
        let (a, b, c) = model::triple(bytes);
        let ab_case = model::Case {
            left: a.clone(),
            right: b.clone(),
        };
        let bc_case = model::Case {
            left: b,
            right: c.clone(),
        };
        let ac_case = model::Case { left: a, right: c };
        if let (Some(ab), Some(bc), Some(ac)) = (
            harness.prepare(&ab_case),
            harness.prepare(&bc_case),
            harness.prepare(&ac_case),
        ) {
            transitivity_checked += 1;
            if harness.run(&ab, false).query_compare
                && harness.run(&bc, false).query_compare
                && !harness.run(&ac, false).query_compare
            {
                transitivity_failures += 1;
                println!("--------------------------------------------------------------------");
                println!("TRANSITIVITY FAILURE on --input-hex {}", hex(bytes));
                println!("A:\n{}", ab_case.left);
                println!("B:\n{}", ab_case.right);
                println!("C:\n{}", bc_case.right);
                if let Some(explanation) = harness.explain(&ac, false) {
                    println!("A includes C fails:\n{explanation}");
                }
            }
        }

        for reversed in [false, true] {
            checked += 1;
            // Cost is timed alongside the verdict. A rewrite that preserves meaning should not
            // change what the check costs by orders of magnitude; when one does, the checker is
            // reading the two spellings differently even though it answers the same. That is how
            // a query plan written with generated fragments came to cost 400x what the same plan
            // written inline costs, with no verdict ever disagreeing to show it.
            let at = query_compare::groups_decided();
            let before = harness.run(&base, reversed).query_compare;
            let before_cost = query_compare::groups_decided() - at;
            let at = query_compare::groups_decided();
            let after = harness.run(&rewritten, reversed).query_compare;
            let after_cost = query_compare::groups_decided() - at;
            // A rewrite legitimately adds groups -- duplicating a selection adds one, partitioning
            // by runtime type adds one per type -- so only a change far past what an edit can
            // account for is reported.
            if before_cost >= COST_FLOOR && after_cost >= COST_FLOOR {
                cost_compared += 1;
                let ratio = after_cost as f64 / before_cost as f64;
                let ratio = if ratio >= 1.0 { ratio } else { 1.0 / ratio };
                if ratio > worst_cost_ratio {
                    worst_cost_ratio = ratio;
                    worst_cost_case = Some((hex(bytes), *rewrite_seed, case.applied.join(", ")));
                }
                if ratio >= COST_RATIO_REPORTED {
                    cost_outliers += 1;
                    println!("---- work changed {ratio:.0}x under a meaning-preserving rewrite");
                    println!("--input-hex {} --rewrite-seed {rewrite_seed}", hex(bytes));
                    println!("applied: {}", case.applied.join(", "));
                    println!("groups decided before {before_cost}, after {after_cost}");
                }
            }
            if before != after {
                violations += 1;
                println!("--------------------------------------------------------------------");
                println!("METAMORPHIC VIOLATION (reversed={reversed})");
                println!("--input-hex {} --rewrite-seed {rewrite_seed}", hex(bytes));
                println!("applied: {}", case.applied.join(", "));
                println!("verdict before: {before}, after: {after}");
                println!("base left:\n{}", case.base.left);
                println!("base right:\n{}", case.base.right);
                println!("rewritten left:\n{}", case.rewritten.left);
                println!("rewritten right:\n{}", case.rewritten.right);
                if let Some(explanation) = harness.explain(&rewritten, reversed) {
                    println!("query_compare says:\n{explanation}");
                }
            }
        }
    }

    println!("--------------------------------------------------------------------");
    println!("cases:                 {}", inputs.len());
    println!("base invalid:          {base_invalid}");
    println!("rewrite invalid:       {rewritten_invalid}");
    println!("verdict comparisons:   {checked}");
    println!("work comparisons:      {cost_compared} (over {COST_FLOOR} groups)");
    println!("work outliers:         {cost_outliers} (at or above {COST_RATIO_REPORTED:.0}x)");
    if let Some((bytes, seed, applied)) = &worst_cost_case {
        println!(
            "worst work change:     {worst_cost_ratio:.1}x  --input-hex {bytes} --rewrite-seed {seed}  [{applied}]"
        );
    }
    println!("rewrites applied:");
    for (name, count) in &by_rewrite {
        println!("  {name:<30} {count}");
    }
    println!("monotonicity checked:  {monotonicity_checked}");
    println!("transitivity checked:  {transitivity_checked}");
    println!("reflexivity failures:  {reflexivity_failures}");
    println!("monotonicity failures: {monotonicity_failures}");
    println!("transitivity failures: {transitivity_failures}");
    println!("violations:            {violations}");
    assert_eq!(
        reflexivity_failures, 0,
        "an operation did not include itself"
    );
    assert_eq!(
        monotonicity_failures, 0,
        "a weaker obligation against a larger query stopped holding"
    );
    assert_eq!(transitivity_failures, 0, "inclusion is not transitive");
    assert_eq!(
        violations, 0,
        "semantics-preserving rewrites changed a verdict"
    );
    assert_eq!(
        rewritten_invalid, 0,
        "a rewrite produced a document GraphQL rejects"
    );
}
