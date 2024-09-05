use apollo_compiler::collections::IndexMap;
use apollo_compiler::collections::IndexSet;
use lazy_static::lazy_static;
use serde_json_bytes::Value as JSON;

use super::immutable::InputPath;
use super::location::Parsed;
use super::ApplyToError;
use super::MethodArgs;
use super::PathList;
use super::VarsWithPathsMap;

// Two kinds of methods: public ones and not-yet-public ones. The future ones
// have proposed implementations and tests, and some are even used within the
// tests of other methods, but are not yet exposed for use in connector schemas.
// Graduating to public status requires updated documentation, careful review,
// and team discussion to make sure the method is one we want to support
// long-term. Once we have a better story for checking method type signatures
// and versioning any behavioral changes, we should be able to expand/improve
// the list of public::* methods more quickly/confidently.
mod future;
mod public;

#[cfg(test)]
mod tests;

type ArrowMethod = fn(
    // Method name
    method_name: &Parsed<String>,
    // Arguments passed to this method
    method_args: Option<&Parsed<MethodArgs>>,
    // The JSON input value (data)
    data: &JSON,
    // The variables
    vars: &VarsWithPathsMap,
    // The input_path (may contain integers)
    input_path: &InputPath<JSON>,
    // The rest of the PathList
    tail: &Parsed<PathList>,
    // Errors
    errors: &mut IndexSet<ApplyToError>,
) -> Option<JSON>;

lazy_static! {
    // This set controls which ->methods are exposed for use in connector
    // schemas. Non-public methods are still implemented and tested, but will
    // not be returned from lookup_arrow_method outside of tests.
    static ref PUBLIC_ARROW_METHODS: IndexSet<&'static str> = {
        let mut public_methods = IndexSet::default();

        // Before enabling a method here, move it from the future:: namespace to
        // the top level of the methods.rs file.
        public_methods.insert("echo");
        // public_methods.insert("typeof");
        public_methods.insert("map");
        // public_methods.insert("eq");
        public_methods.insert("match");
        // public_methods.insert("matchIf");
        // public_methods.insert("match_if");
        // public_methods.insert("add");
        // public_methods.insert("sub");
        // public_methods.insert("mul");
        // public_methods.insert("div");
        // public_methods.insert("mod");
        public_methods.insert("first");
        public_methods.insert("last");
        public_methods.insert("slice");
        public_methods.insert("size");
        // public_methods.insert("has");
        // public_methods.insert("get");
        // public_methods.insert("keys");
        // public_methods.insert("values");
        public_methods.insert("entries");
        // public_methods.insert("not");
        // public_methods.insert("or");
        // public_methods.insert("and");

        public_methods
    };

    // This map registers all the built-in ->methods that are currently
    // implemented, even the non-public ones that are not included in the
    // PUBLIC_ARROW_METHODS set.
    static ref ARROW_METHODS: IndexMap<String, ArrowMethod> = {
        let mut methods = IndexMap::<String, ArrowMethod>::default();

        // This built-in method returns its first input argument as-is, ignoring
        // the input data. Useful for embedding literal values, as in
        // $->echo("give me this string").
        methods.insert("echo".to_string(), public::echo_method);

        // Returns the type of the data as a string, e.g. "object", "array",
        // "string", "number", "boolean", or "null". Note that `typeof null` is
        // "object" in JavaScript but "null" for our purposes.
        methods.insert("typeof".to_string(), future::typeof_method);

        // When invoked against an array, ->map evaluates its first argument
        // against each element of the array and returns an array of the
        // results. When invoked against a non-array, ->map evaluates its first
        // argument against the data and returns the result.
        methods.insert("map".to_string(), public::map_method);

        // Returns true if the data is deeply equal to the first argument, false
        // otherwise. Equality is solely value-based (all JSON), no references.
        methods.insert("eq".to_string(), future::eq_method);

        // Takes any number of pairs [candidate, value], and returns value for
        // the first candidate that equals the input data $. If none of the
        // pairs match, a runtime error is reported, but a single-element
        // [<default>] array as the final argument guarantees a default value.
        methods.insert("match".to_string(), public::match_method);

        // Like ->match, but expects the first element of each pair to evaluate
        // to a boolean, returning the second element of the first pair whose
        // first element is true. This makes providing a final catch-all case
        // easy, since the last pair can be [true, <default>].
        methods.insert("matchIf".to_string(), future::match_if_method);
        methods.insert("match_if".to_string(), future::match_if_method);

        // Arithmetic methods
        methods.insert("add".to_string(), future::add_method);
        methods.insert("sub".to_string(), future::sub_method);
        methods.insert("mul".to_string(), future::mul_method);
        methods.insert("div".to_string(), future::div_method);
        methods.insert("mod".to_string(), future::mod_method);

        // Array/string methods (note that ->has and ->get also work for array
        // and string indexes)
        methods.insert("first".to_string(), public::first_method);
        methods.insert("last".to_string(), public::last_method);
        methods.insert("slice".to_string(), public::slice_method);
        methods.insert("size".to_string(), public::size_method);

        // Object methods (note that ->size also works for objects)
        methods.insert("has".to_string(), future::has_method);
        methods.insert("get".to_string(), future::get_method);
        methods.insert("keys".to_string(), future::keys_method);
        methods.insert("values".to_string(), future::values_method);
        methods.insert("entries".to_string(), public::entries_method);

        // Logical methods
        methods.insert("not".to_string(), future::not_method);
        methods.insert("or".to_string(), future::or_method);
        methods.insert("and".to_string(), future::and_method);

        methods
    };
}

pub(super) fn lookup_arrow_method(method_name: &str) -> Option<&ArrowMethod> {
    if cfg!(test) || PUBLIC_ARROW_METHODS.contains(method_name) {
        ARROW_METHODS.get(method_name)
    } else {
        None
    }
}
