//! Deterministic differential runner.
//!
//! Without `--lean-oracle` this compares the two Rust implementations and reports how often the
//! grammar produced a document GraphQL rejects. With an oracle it additionally holds
//! `query_compare` to exact agreement with the Lean model.
//!
//! ```text
//! cargo run --example differential -- --exhaustive
//! cargo run --example differential -- --seed 1 --cases 5000
//! cargo run --example differential -- --lean-oracle target/query-inclusion-lean-oracle --exhaustive
//! cargo run --example differential -- --input-hex 0a0b0c
//! ```

use query_inclusion_fuzz::harness::Harness;
use query_inclusion_fuzz::harness::PreparedCase;
use query_inclusion_fuzz::lean_oracle::LeanOracle;
use query_inclusion_fuzz::model;

/// Byte values the exhaustive matrix ranges over. Chosen to hit every residue the grammar takes
/// (`% 3`, `% 4`, `% 5`, `% 8`) rather than to be uniformly spread.
const ALPHABET: [u8; 8] = [0, 1, 2, 3, 4, 5, 7, 11];
/// Byte offsets the exhaustive matrix varies; every other byte reads as zero.
///
/// Spread deliberately rather than taken as a prefix. One operation spends its first eight bytes
/// on the response-slot table and the ninth on variable declarations, so varying a prefix leaves
/// every selection structure decoding from zeros — a matrix built that way ran half a million
/// comparisons while reaching exactly one rejection reason. These offsets touch two slot fields,
/// the declaration byte, the selection count, and two bytes of selection structure.
const EXHAUSTIVE_POSITIONS: [usize; 6] = [0, 2, 8, 9, 10, 12];

struct Options {
    oracle: Option<String>,
    exhaustive: bool,
    seed: u64,
    cases: usize,
    input_hex: Option<String>,
    verbose: bool,
    dump_seeds: Option<String>,
}

fn parse_options() -> Options {
    let mut options = Options {
        oracle: None,
        exhaustive: false,
        seed: 1,
        cases: 2000,
        input_hex: None,
        verbose: false,
        dump_seeds: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lean-oracle" => options.oracle = Some(args.next().expect("--lean-oracle PATH")),
            "--exhaustive" => options.exhaustive = true,
            "--seed" => options.seed = args.next().expect("--seed N").parse().expect("seed"),
            "--cases" => options.cases = args.next().expect("--cases N").parse().expect("cases"),
            "--input-hex" => options.input_hex = Some(args.next().expect("--input-hex HEX")),
            "--verbose" => options.verbose = true,
            "--dump-seeds" => options.dump_seeds = Some(args.next().expect("--dump-seeds DIR")),
            other => panic!("unknown argument: {other}"),
        }
    }
    options
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).expect("hex"))
        .collect()
}

/// xorshift64*, so a seed reproduces a campaign without pulling in a dependency.
struct Rng(u64);

impl Rng {
    fn next_byte(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 33) as u8
    }
}

fn exhaustive_inputs() -> Vec<Vec<u8>> {
    let width = EXHAUSTIVE_POSITIONS.iter().max().copied().unwrap_or(0) + 1;
    let total = ALPHABET.len().pow(EXHAUSTIVE_POSITIONS.len() as u32);
    (0..total)
        .map(|index| {
            let mut bytes = vec![0u8; width];
            let mut remainder = index;
            for offset in EXHAUSTIVE_POSITIONS {
                bytes[offset] = ALPHABET[remainder % ALPHABET.len()];
                remainder /= ALPHABET.len();
            }
            bytes
        })
        .collect()
}

/// A campaign mixes three input shapes:
///
/// * plain random bytes, giving unrelated operation pairs;
/// * a prefix repeated exactly, so both operations decode identically — inclusion must hold in
///   both directions, which is a reflexivity check every lane has to pass;
/// * a repeated prefix with one byte perturbed, which lands just off that boundary.
///
/// The last two are where a subtly wrong checker separates from a correct one. They need no
/// change to the grammar, and so none to the Lean adapter: the split point is computed from the
/// decoder itself and the input is assembled before either side sees it.
fn generated_inputs(seed: u64, count: usize) -> Vec<Vec<u8>> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut inputs = Vec::with_capacity(count);
    while inputs.len() < count {
        let length = 8 + (rng.next_byte() as usize % 24);
        let base: Vec<u8> = (0..length).map(|_| rng.next_byte()).collect();
        match rng.next_byte() % 4 {
            0 => inputs.push(base),
            _ => {
                let split = model::left_consumed(&base);
                let prefix = &base[..split];
                let mut related: Vec<u8> = prefix.iter().chain(prefix.iter()).copied().collect();
                // Three quarters of the related inputs are nudged off exact equality.
                if !rng.next_byte().is_multiple_of(4) && !prefix.is_empty() {
                    let index = split + (rng.next_byte() as usize % prefix.len());
                    related[index] = related[index].wrapping_add(1 + rng.next_byte() % 7);
                }
                inputs.push(related);
            }
        }
    }
    inputs
}

#[derive(Default)]
struct Totals {
    decoded: usize,
    discarded: usize,
    compared: usize,
    lean_mismatches: usize,
    shape_divergences: usize,
    /// Comparisons the response-shape checker cannot be expected to match, because the finding
    /// lies outside what it models at all.
    shape_scope_differences: usize,
    out_of_scope: usize,
    /// How many comparisons concluded "included". A campaign where this is 0 or equals
    /// `compared` proves nothing: every lane would agree by answering the same way always.
    included: usize,
}

fn report_mismatch(
    bytes: &[u8],
    case: &PreparedCase,
    reversed: bool,
    lane: &str,
    expected: bool,
    actual: bool,
    harness: &Harness,
) {
    let (left, right) = if reversed {
        (&case.right_source, &case.left_source)
    } else {
        (&case.left_source, &case.right_source)
    };
    println!("--------------------------------------------------------------------");
    println!("MISMATCH ({lane}) on --input-hex {}", hex(bytes));
    println!(
        "direction: left {} right",
        if reversed { "<-" } else { "->" }
    );
    println!("expected: {expected}, query_compare: {actual}");
    println!("left:\n{left}");
    println!("right:\n{right}");
    if let Some(explanation) = harness.explain(case, reversed) {
        println!("query_compare says:\n{explanation}");
    }
}

fn main() {
    let options = parse_options();
    let harness = Harness::new();
    let mut oracle = options.oracle.as_ref().map(LeanOracle::open);

    if let Some(oracle) = oracle.as_mut() {
        let theirs = oracle.schema_digest();
        let ours = model::schema_digest();
        assert_eq!(
            theirs, ours,
            "the Lean oracle and this harness hold different schemas; \
             model.rs and lean/QueryInclusionOracle.lean have drifted apart"
        );
        println!("schema digest agrees with the Lean oracle");
    } else {
        println!("no --lean-oracle: comparing the two Rust implementations only");
    }

    let inputs = match &options.input_hex {
        Some(text) => vec![from_hex(text)],
        None if options.exhaustive => exhaustive_inputs(),
        None => generated_inputs(options.seed, options.cases),
    };

    // One retained seed per semantically distinct outcome. Inclusion is not symmetric, so the
    // four (forward, backward) combinations are the meaningful categories: equivalent queries,
    // each strict containment, and unrelated. Picking them by observed behavior keeps the corpus
    // honest — a hand-written seed can silently stop exercising what its name claims.
    let seed_names = [
        ("equivalent-both-directions", (true, true)),
        ("left-strictly-includes-right", (true, false)),
        ("right-strictly-includes-left", (false, true)),
        ("unrelated-neither-includes", (false, false)),
    ];
    let mut seeds: Vec<Option<Vec<u8>>> = vec![None; seed_names.len()];

    // Which decision paths the campaign actually reaches. A variant that never appears is a
    // variant the differential lane has never tested, however many executions it ran.
    let mut reasons: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    let mut totals = Totals::default();
    for bytes in &inputs {
        totals.decoded += 1;
        let decoded = model::decode(bytes);
        let case = match harness.prepare_explained(&decoded) {
            Ok(case) => case,
            Err(error) => {
                totals.discarded += 1;
                if (options.verbose && totals.discarded <= 3) || options.input_hex.is_some() {
                    println!("---- discarded, --input-hex {}", hex(bytes));
                    println!("left:\n{}right:\n{}", decoded.left, decoded.right);
                    println!("{error}");
                }
                continue;
            }
        };

        let lean = oracle.as_mut().map(|oracle| oracle.verdicts(bytes));
        for (index, reversed) in [false, true].into_iter().enumerate() {
            let verdicts = harness.run(&case, reversed);
            totals.compared += 1;
            if verdicts.out_of_scope {
                totals.out_of_scope += 1;
                continue;
            }
            if verdicts.query_compare {
                totals.included += 1;
            }
            if let Some(reason) = verdicts.reason {
                *reasons.entry(reason).or_default() += 1;
            }
            if let Some((forward, backward)) = lean {
                let expected = if index == 0 { forward } else { backward };
                if expected.includes != verdicts.query_compare {
                    totals.lean_mismatches += 1;
                    report_mismatch(
                        bytes,
                        &case,
                        reversed,
                        "lean",
                        expected.includes,
                        verdicts.query_compare,
                        &harness,
                    );
                }
                if expected.reference != expected.includes {
                    println!(
                        "note: Lean includesBool and includesBoolReference disagree on {}",
                        hex(bytes)
                    );
                }
            }
            if index == 1 {
                let observed = (
                    harness.run(&case, false).query_compare,
                    verdicts.query_compare,
                );
                if let Some(slot) = seed_names
                    .iter()
                    .position(|(_, wanted)| *wanted == observed)
                    .filter(|slot| seeds[*slot].is_none())
                {
                    seeds[slot] = Some(bytes.clone());
                }
            }
            // `compare_operations` does not look at variable declarations, so it cannot agree
            // when the only reason for rejection is a shared declaration that differs. That is a
            // difference in modeled scope, not a disagreement about inclusion.
            if verdicts.reason == Some("VariableDeclarationMismatch")
                && verdicts.response_shape != verdicts.query_compare
            {
                totals.shape_scope_differences += 1;
            } else if verdicts.response_shape != verdicts.query_compare {
                totals.shape_divergences += 1;
                if options.verbose || options.input_hex.is_some() {
                    report_mismatch(
                        bytes,
                        &case,
                        reversed,
                        "response_shape (advisory)",
                        verdicts.response_shape,
                        verdicts.query_compare,
                        &harness,
                    );
                }
            }
        }
    }

    println!("--------------------------------------------------------------------");
    println!("inputs decoded:            {}", totals.decoded);
    println!(
        "discarded (invalid GraphQL): {} ({:.1}%)",
        totals.discarded,
        100.0 * totals.discarded as f64 / totals.decoded.max(1) as f64
    );
    println!("comparisons run:           {}", totals.compared);
    let decided = totals.compared - totals.out_of_scope;
    println!(
        "verdict split:             {} included / {} not included",
        totals.included,
        decided - totals.included
    );
    // A single replay legitimately answers one way; the guard is about campaigns.
    if options.input_hex.is_none() {
        assert!(
            totals.included > 0 && totals.included < decided,
            "degenerate campaign: every comparison answered the same way, so agreement is vacuous"
        );
    }
    println!("out of modeled scope:      {}", totals.out_of_scope);
    println!(
        "response_shape divergences: {} (advisory)",
        totals.shape_divergences
    );
    println!(
        "response_shape scope gaps:  {} (variable declarations, not modeled there)",
        totals.shape_scope_differences
    );
    println!("rejection reasons reached:");
    for name in [
        "RootTypeMismatch",
        "VariableDeclarationMismatch",
        "MissingResponseName",
        "FieldNameMismatch",
        "FieldArgumentsMismatch",
        "UndefinedField",
        "FieldDirectivesMismatch",
        "UndefinedFragment",
        "Internal",
    ] {
        match reasons.get(name) {
            Some(count) => println!("  {name:<28} {count}"),
            None => println!("  {name:<28} NEVER REACHED"),
        }
    }
    if oracle.is_some() {
        println!("Lean mismatches:           {}", totals.lean_mismatches);
        assert_eq!(totals.lean_mismatches, 0, "Lean and query_compare disagree");
    }

    if let Some(directory) = &options.dump_seeds {
        std::fs::create_dir_all(directory).expect("create the seed directory");
        for (slot, (name, _)) in seed_names.iter().enumerate() {
            match &seeds[slot] {
                Some(bytes) => {
                    std::fs::write(format!("{directory}/{name}"), bytes).expect("write seed");
                    println!("seed {name}: --input-hex {}", hex(bytes));
                }
                None => println!("seed {name}: not observed in this campaign"),
            }
        }
    }
}
