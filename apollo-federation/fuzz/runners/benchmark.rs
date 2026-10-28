//! Three-way performance comparison on the same generated cases.
//!
//! ```text
//! cargo run --release --example benchmark -- --lean-oracle target/query-inclusion-lean-oracle
//! ```
//!
//! The two Rust lanes are timed in-process. The Lean lane is timed *inside* the oracle process,
//! so the line protocol is not counted against it. All three answer the identical question on the
//! identical case, which is what makes the numbers comparable at all — but note that the Lean and
//! `query_compare` lanes run one algorithm and `response_shape` runs a different one, so a gap
//! between the Rust lanes is an algorithmic result while a gap between Lean and `query_compare`
//! is a runtime-and-language result.

use std::time::Instant;

use apollo_federation::correctness::compare_operations;
use apollo_federation::correctness::query_compare;
use apollo_federation::correctness::query_compare::QueryComparator;
use query_inclusion_fuzz::harness::Harness;
use query_inclusion_fuzz::harness::PreparedCase;
use query_inclusion_fuzz::lean_oracle::LeanOracle;
use query_inclusion_fuzz::model;

struct Options {
    oracle: Option<String>,
    iterations: usize,
    seed: u64,
    candidates: usize,
    samples: usize,
}

fn parse_options() -> Options {
    let mut options = Options {
        oracle: None,
        iterations: 500,
        seed: 11,
        candidates: 4000,
        samples: 24,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lean-oracle" => options.oracle = Some(args.next().expect("--lean-oracle PATH")),
            "--iterations" => options.iterations = args.next().unwrap().parse().unwrap(),
            "--seed" => options.seed = args.next().unwrap().parse().unwrap(),
            "--candidates" => options.candidates = args.next().unwrap().parse().unwrap(),
            "--samples" => options.samples = args.next().unwrap().parse().unwrap(),
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

/// Times one closure, discarding a warm-up run so the first-call costs of lazily built caches do
/// not land in the measurement.
fn time(iterations: usize, mut run: impl FnMut() -> bool) -> (u64, bool) {
    let verdict = run();
    let started = Instant::now();
    let mut accepted = 0usize;
    for _ in 0..iterations {
        accepted += run() as usize;
    }
    let elapsed = started.elapsed().as_nanos() as u64;
    std::hint::black_box(accepted);
    (elapsed / iterations.max(1) as u64, verdict)
}

fn main() {
    let options = parse_options();
    let harness = Harness::new();
    let mut oracle = options.oracle.as_ref().map(LeanOracle::open);
    if let Some(oracle) = oracle.as_mut() {
        assert_eq!(
            oracle.schema_digest(),
            model::schema_digest(),
            "the Lean oracle and this harness hold different schemas"
        );
    }

    // Sample across the size range rather than taking the largest cases only: a checker that is
    // fast on big queries and slow on small ones would otherwise look uniformly good.
    let mut rng = Rng(options.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut candidates: Vec<(Vec<u8>, PreparedCase)> = Vec::new();
    while candidates.len() < options.candidates {
        let length = 8 + (rng.next_byte() as usize % 24);
        let bytes: Vec<u8> = (0..length).map(|_| rng.next_byte()).collect();
        if let Some(case) = harness.prepare(&model::decode(&bytes)) {
            candidates.push((bytes, case));
        }
    }
    candidates.sort_by_key(|(_, case)| case.left_source.len() + case.right_source.len());

    // Sample the two verdicts separately. A rejection can stop at the first uncovered response
    // name, so a sample dominated by rejections measures early-exit cost and says little about
    // the traversal an accepting run performs. Random pairs are mostly unrelated, so without the
    // split the accepting column would be nearly empty.
    let mut by_verdict: [Vec<&(Vec<u8>, PreparedCase)>; 2] = [Vec::new(), Vec::new()];
    for candidate in &candidates {
        let included =
            query_compare::includes(harness.schema(), &candidate.1.left, &candidate.1.right)
                .is_ok();
        by_verdict[included as usize].push(candidate);
    }
    let per_group = (options.samples / 2).max(1);
    let mut samples: Vec<&(Vec<u8>, PreparedCase)> = Vec::new();
    for group in &by_verdict {
        let stride = (group.len() / per_group).max(1);
        samples.extend(group.iter().step_by(stride).take(per_group).copied());
    }
    samples.sort_by_key(|(_, case)| case.left_source.len() + case.right_source.len());
    println!(
        "candidate pool: {} not included, {} included",
        by_verdict[0].len(),
        by_verdict[1].len()
    );

    // The floor: the smallest case either implementation can be asked about. Whatever cost does
    // not vary with the query is per-call setup, and on this schema that is dominated by the
    // possible-type index each call builds before looking at the operations.
    {
        let floor = harness
            .prepare(&model::Case {
                left: "query { animals { r0: name } }\n".to_string(),
                right: "query { animals { r0: name } }\n".to_string(),
            })
            .expect("the floor case validates");
        let (query_compare_ns, _) = time(options.iterations, || {
            query_compare::includes(harness.schema(), &floor.left, &floor.right).is_ok()
        });
        let (response_shape_ns, _) = time(options.iterations, || {
            compare_operations(harness.schema(), &floor.right, &floor.left).is_ok()
        });
        println!(
            "per-call floor (trivial identical queries): query_compare {query_compare_ns} ns, \
             compare_operations {response_shape_ns} ns"
        );
        println!();
    }

    // Reusing one comparator separates the algorithm from the per-call schema indexing that the
    // floor above measures.
    let comparator = QueryComparator::new(harness.schema()).expect("index the shared schema");

    println!(
        "{:>6}  {:>8}  {:>12}  {:>14}  {:>10}  {:>14}  {:>9}",
        "chars", "verdict", "lean ns/op", "query_compare", "reused", "response_shape", "qc vs rs"
    );
    // [not included, included] x [lean, per-call, response_shape, reused]
    let mut totals = [[0u64; 4]; 2];
    let mut counted = [0u64; 2];
    for (bytes, case) in samples {
        let size = case.left_source.len() + case.right_source.len();
        let (query_compare_ns, verdict) = time(options.iterations, || {
            query_compare::includes(harness.schema(), &case.left, &case.right).is_ok()
        });
        // Reversed arguments: `compare_operations(this, other)` asks whether `this` is a subset
        // of `other`, the opposite direction from `includes(left, right)`.
        let (response_shape_ns, shape_verdict) = time(options.iterations, || {
            compare_operations(harness.schema(), &case.right, &case.left).is_ok()
        });
        assert_eq!(verdict, shape_verdict, "lanes disagree while benchmarking");
        let (reused_ns, _) = time(options.iterations, || {
            comparator.includes(&case.left, &case.right).is_ok()
        });
        let lean_ns = oracle
            .as_mut()
            .map(|oracle| oracle.bench(bytes, options.iterations))
            .unwrap_or(0);

        let group = verdict as usize;
        totals[group][0] += lean_ns;
        totals[group][1] += query_compare_ns;
        totals[group][2] += response_shape_ns;
        totals[group][3] += reused_ns;
        counted[group] += 1;
        println!(
            "{size:>6}  {:>8}  {lean_ns:>12}  {query_compare_ns:>14}  {reused_ns:>10}  {response_shape_ns:>14}  {:>8.2}x",
            if verdict { "included" } else { "not" },
            response_shape_ns as f64 / reused_ns.max(1) as f64,
        );
    }

    println!();
    for (group, label) in [(1usize, "included"), (0usize, "not included")] {
        let n = counted[group];
        if n == 0 {
            continue;
        }
        println!(
            "mean over {n} {label} cases, {} iterations each:",
            options.iterations
        );
        if oracle.is_some() {
            println!(
                "  lean includesBool        {:>9} ns/op",
                totals[group][0] / n
            );
        }
        println!(
            "  query_compare::includes  {:>9} ns/op  (indexes the schema per call)",
            totals[group][1] / n
        );
        println!(
            "  QueryComparator reused   {:>9} ns/op",
            totals[group][3] / n
        );
        println!(
            "  compare_operations       {:>9} ns/op  ({:.2}x reused)",
            totals[group][2] / n,
            totals[group][2] as f64 / totals[group][3].max(1) as f64
        );
        println!();
    }
}
