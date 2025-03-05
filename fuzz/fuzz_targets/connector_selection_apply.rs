//! Connector's Selection Mapping Application Fuzzing
//!
//! This fuzzing target seeks to ensure that applying a selection to a JSON input
//! always behaves as expected. In order to do so, this target generates selections
//! with various no-op / identity operations such that the final result should always
//! simplify down to the following:
//!
//! ```selection
//! selection: data
//! ```
//!
//! Which, when given the following data:
//!
//! ```json
//! { "data": 5 }
//! ```
//!
//! Should evaluate to:
//!
//! ```json
//! { "selection": 5 }
//! ```
//!
//! Refer to [BNF_GRAMMAR] for more info on the no-ops generated.
//!
#![no_main]

use std::iter::FromIterator;
use std::sync::LazyLock;

use apollo_federation::sources::connect::JSONSelection;
use bnf::Grammar;
use libfuzzer_sys::arbitrary;
use libfuzzer_sys::arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use libfuzzer_sys::Corpus;
use rand::rngs::StdRng;
use serde_json_bytes::Map;
use serde_json_bytes::Value;

const DATA_VALUE: u8 = 5;
static INPUT: LazyLock<Value> =
    LazyLock::new(|| Value::Object(Map::from_iter([("data".into(), DATA_VALUE.into())])));
static OUTPUT: LazyLock<Value> =
    LazyLock::new(|| Value::Object(Map::from_iter([("selection".into(), DATA_VALUE.into())])));

fuzz_target!(|input: GeneratedSelection| -> Corpus {
    // Generating a selection might choose a path which recurses too deeply, so
    // we just mark those traversals as being rejected since they would require
    // seeding and iterating the Rng.
    let Some(selection) = input.0 else {
        return Corpus::Reject;
    };

    // Apply the fuzzed selection and ensure that it matches the output we expect
    let (applied, errors) = selection.apply_to(&*INPUT);
    assert!(errors.is_empty());
    assert_eq!(applied, Some(OUTPUT.clone()));

    Corpus::Keep
});

/// BNF Grammar for generating selections that simplify to `selection: data`
///
/// This grammar tries to capture as many features of the selection language that
/// can simplify to an identity function so as to ensure that each feature works
/// as expected while still evaluating the same as the simplified selection.
///
/// Most importantly, this grammar generates arbitrarily nested chains of these
/// identity functions.
///
/// Techniques captured below:
/// - Array selectors: Evaluation of an in-place literal array sliced with the
///   index of the original data.
/// - Echo: Echoing the original data directly or by using `@`
/// - Matching: Matching the data against itself using a prefixed key for unwrapping
/// - Mapping: Mapping the original data through `@` or in-place literals
/// - Literal wrapping: Wrapping the data and then unwrapping through member access
const BNF_GRAMMAR: &str = r##"
    <selection> ::= "selection: " <base>
    <base>      ::= "$.data" | <unwrap> | <array> | <echo> | <entries> | <map>
    <array>     ::= "$([ " <base> ", 0, 0 ])->first"
                  | "$([ 0, 0, " <base> " ])->last"
                  | "$([ 0, " <base> ", 0 ])->slice(1, 2)->first"
                  | "$([ 0, " <base> ", 0 ])->slice(1, 2)->last"
    <echo>      ::= <base> "->echo(@)"
                  | "$->echo(" <base> ")"
    <entries>   ::= "$({ thing: " <base> " })->entries->match([false, 0], [@, @.value])->first"
                  | "$({ thing: " <base> " })->entries->match([[{ key: 'thing', value: $.data }], @.value], [@, 0])->first"
    <map>       ::= <base> "->map({ mapped: @ })->first.mapped"
                  | <base> "->map({ mapped: @ })->last.mapped"
                  | "$->map({ mapped: " <base> " })->first.mapped"
                  | "$->map({ mapped: " <base> " })->last.mapped"
    <unwrap>    ::= "$({ unwrap: " <base> " }).unwrap"
"##;
static GRAMMAR: LazyLock<Grammar> = LazyLock::new(|| BNF_GRAMMAR.parse().unwrap());

struct GeneratedSelection(Option<JSONSelection>);
impl<'a> Arbitrary<'a> for GeneratedSelection {
    fn arbitrary(u: &mut libfuzzer_sys::arbitrary::Unstructured<'a>) -> arbitrary::Result<Self> {
        let bytes = <[u8; 32] as Arbitrary>::arbitrary(u)?;
        let mut rng: StdRng = rand::SeedableRng::from_seed(bytes);

        let selection = GRAMMAR
            .generate_seeded(&mut rng)
            .ok()
            .as_deref()
            .map(JSONSelection::parse)
            .transpose()
            .expect("failed to parse JSONSelection");
        Ok(GeneratedSelection(selection))
    }
}

impl std::fmt::Debug for GeneratedSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0
            .as_ref()
            .map(|selection| write!(f, "{}", selection))
            .unwrap_or(Ok(()))
    }
}
