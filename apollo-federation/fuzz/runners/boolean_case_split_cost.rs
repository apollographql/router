//! How the Boolean case-split scales with the number of distinct `@skip`/`@include` variables.
//!
//! Deciding inclusion at one response key may require case-splitting over the Boolean variables
//! that gate its variants. With `K` distinct variables there are `2^K` assignments, so *how* a
//! checker explores them decides whether it stays usable: materializing them costs `O(K · 2^K)`
//! memory, while descending one variable at a time costs `O(K)` and can stop early.
//!
//! This measures peak allocation and wall time for both implementations over a ladder of `K`,
//! on four shapes:
//!
//! | arm | shape | correct answer |
//! | --- | --- | --- |
//! | `uncovered` | `K` conditional variants of a scalar field, nothing unconditional | rejection |
//! | `covered` | the same plus one unconditional variant | accepted |
//! | `witness` | a composite field whose unconditional variant syntactically contains the demand | accepted |
//! | `exhaustive` | jointly-covering variants of a composite field, none a syntactic witness | accepted |
//!
//! The first three are settled without enumerating: `uncovered` fails on the first assignment,
//! `covered` by symbolic clause subtraction, `witness` by the syntactic shortcut. Only `exhaustive`
//! forces every assignment to be visited, which makes it the worst case for a checker whose memory
//! is flat but whose time is not.
//!
//! Background: apollographql/router#10054 reports the old checker exhausting memory on a real
//! operation carrying 88 `@include` directives over 82 distinct variables. Its `uncovered` and
//! `covered` arms are the reproduction from that report.
//!
//! ```text
//! cargo run --release --example boolean_case_split_cost -- --k 18 --arm uncovered --checker old
//! cargo run --release --example boolean_case_split_cost -- --k 82 --arm uncovered --checker new
//! ```

use std::alloc::GlobalAlloc;
use std::alloc::Layout;
use std::alloc::System;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Instant;

use apollo_compiler::ExecutableDocument;
use apollo_compiler::Schema;
use apollo_federation::correctness::compare_operations;
use apollo_federation::correctness::query_compare;
use apollo_federation::schema::ValidFederationSchema;

/// Counting allocator with a hard cap. `RLIMIT_AS` is not settable on macOS, so past the cap this
/// returns null and the process aborts through `handle_alloc_error` — the same approach the issue
/// used, so the numbers are comparable.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static CAP: AtomicUsize = AtomicUsize::new(usize::MAX);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        if live > CAP.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            return std::ptr::null_mut();
        }
        PEAK.fetch_max(live, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SCHEMA: &str = r#"
type Query { widget: Widget! }
type Widget { id: ID!  label: String!  vendor: Vendor! }
type Vendor { id: ID!  name: String! }
"#;

/// The left operation asks for `label` unconditionally.
fn left_operation() -> String {
    "query Left { widget { id label } }\n".to_string()
}

/// The right operation puts `K` variants at the `label` response key, one per Boolean variable, so
/// the union group spans exactly `K` variables. The accept arm adds one unconditional `label`,
/// which makes the first hypothesis group match.
/// A composite field with many variants and an unconditional fallback that *is* a syntactic
/// witness: the covering operation literally contains `vendor { name }`, so the shortcut settles
/// the comparison at the enclosing field and the Boolean search never runs.
///
fn witness_operation(k: usize) -> String {
    let declarations: Vec<String> = (0..k).map(|i| format!("$v{i}: Boolean!")).collect();
    let mut selections: Vec<String> = (0..k)
        .map(|i| format!("      vendor @include(if: $v{i}) {{ name }}"))
        .collect();
    selections.push("      vendor { name }".to_string());
    format!(
        "query Right({}) {{\n    widget {{\n{}\n    }}\n  }}\n",
        declarations.join(", "),
        selections.join("\n")
    )
}

/// Jointly-covering variants of a composite field, none of which is a syntactic witness.
///
/// Every variant carries a directive, so none of them syntactically includes the bare
/// `vendor { name }` the other side demands; the composite shortcut handles only the one-to-one
/// case; and the scalar shortcut does not apply to a composite field. Nothing can settle this
/// symbolically, the answer is "included", and so the Boolean search must visit every assignment.
/// This is the actual worst case for the new checker.
fn exhaustive_operation(k: usize) -> String {
    let declarations: Vec<String> = (0..k).map(|i| format!("$v{i}: Boolean!")).collect();
    let mut selections: Vec<String> = (0..k)
        .map(|i| format!("      vendor @include(if: $v{i}) {{ name }}"))
        .collect();
    // `@skip` is not repeatable, so the all-false case nests one inline fragment per variable.
    let mut all_false = "vendor { name }".to_string();
    for i in 0..k {
        all_false = format!("... @skip(if: $v{i}) {{ {all_false} }}");
    }
    selections.push(format!("      {all_false}"));
    format!(
        "query Right({}) {{\n    widget {{\n{}\n    }}\n  }}\n",
        declarations.join(", "),
        selections.join("\n")
    )
}

fn right_operation(k: usize, accept: bool) -> String {
    let declarations: Vec<String> = (0..k).map(|i| format!("$v{i}: Boolean!")).collect();
    let mut selections: Vec<String> = (0..k)
        .map(|i| format!("      label @include(if: $v{i})"))
        .collect();
    if accept {
        selections.push("      label".to_string());
    }
    format!(
        "query Right({}) {{\n    widget {{\n      id\n{}\n    }}\n  }}\n",
        declarations.join(", "),
        selections.join("\n")
    )
}

fn main() {
    let mut k = 16usize;
    let mut accept = false;
    let mut witness = false;
    let mut exhaustive = false;
    let mut checker = "new".to_string();
    let mut cap_gib = 8usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--k" => k = args.next().unwrap().parse().unwrap(),
            "--arm" => {
                let arm = args.next().unwrap();
                accept = arm == "covered";
                witness = arm == "witness";
                exhaustive = arm == "exhaustive";
            }
            "--checker" => checker = args.next().unwrap(),
            "--cap-gib" => cap_gib = args.next().unwrap().parse().unwrap(),
            other => panic!("unknown argument: {other}"),
        }
    }

    let schema = Schema::parse_and_validate(SCHEMA, "schema.graphql").unwrap();
    let schema = ValidFederationSchema::new(schema).unwrap();
    let left_source = if witness || exhaustive {
        "query Left { widget { vendor { name } } }\n".to_string()
    } else {
        left_operation()
    };
    let right_source = if exhaustive {
        exhaustive_operation(k)
    } else if witness {
        witness_operation(k)
    } else {
        right_operation(k, accept)
    };
    let left =
        ExecutableDocument::parse_and_validate(schema.schema(), &left_source, "l.graphql").unwrap();
    let right = ExecutableDocument::parse_and_validate(schema.schema(), &right_source, "r.graphql")
        .unwrap();

    // Cap only the measured section, so parsing and schema construction are not counted against it.
    CAP.store(cap_gib * (1 << 30), Ordering::Relaxed);
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let baseline = LIVE.load(Ordering::Relaxed);

    let started = Instant::now();
    // `compare_operations(this, other)` asks whether `this` is a subset of `other`;
    // `includes(left, right)` asks the reverse, hence the swapped arguments.
    let verdict = match checker.as_str() {
        "old" => compare_operations(&schema, &left, &right).is_ok(),
        _ => query_compare::includes(&schema, &right, &left).is_ok(),
    };
    let elapsed = started.elapsed();

    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(baseline);
    println!(
        "checker={checker} K={k} arm={} verdict={} peak={:.1} MB time={:.3} s",
        if exhaustive {
            "exhaustive"
        } else if witness {
            "witness"
        } else if accept {
            "covered"
        } else {
            "uncovered"
        },
        if verdict { "accepted" } else { "rejected" },
        peak as f64 / 1_048_576.0,
        elapsed.as_secs_f64(),
    );
}
